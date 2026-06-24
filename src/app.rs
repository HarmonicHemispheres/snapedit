//! Application state, eframe wiring, capture state machine, and the editing canvas.

use crate::document::{Annotation, Document, Kind, Tool, View};
use crate::{capture, export, output};
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2, pos2, vec2};
use std::path::PathBuf;
use std::time::Duration;

/// Delay between requesting a capture (which hides our window) and grabbing pixels.
const CAPTURE_DELAY: f64 = 0.35;

/// Quick-pick annotation colors.
const PALETTE: &[Color32] = &[
    Color32::from_rgb(0xff, 0x3b, 0x30), // red
    Color32::from_rgb(0xff, 0x9f, 0x0a), // orange
    Color32::from_rgb(0xff, 0xd6, 0x0a), // yellow
    Color32::from_rgb(0x34, 0xc7, 0x59), // green
    Color32::from_rgb(0x0a, 0x84, 0xff), // blue
    Color32::from_rgb(0xbf, 0x5a, 0xf2), // purple
    Color32::from_rgb(0xff, 0xff, 0xff), // white
    Color32::from_rgb(0x10, 0x10, 0x12), // near-black
];

const ACCENT: Color32 = Color32::from_rgb(0x4d, 0x9f, 0xff);

#[derive(Clone, Copy)]
enum PendingKind {
    Full,         // primary monitor (quick-capture default)
    Monitor(u32), // a specific monitor by id
    AllMonitors,  // the entire virtual desktop, stitched
    Region,
    Window(u32),
}

struct RegionState {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    texture: egui::TextureHandle,
    origin_x: i32, // physical top-left of the virtual desktop (overlay placement)
    origin_y: i32,
    scale_factor: f32,
    start: Option<Pos2>,
    current: Option<Pos2>,
}

enum CaptureState {
    Idle,
    Pending { kind: PendingKind, fire: f64 },
    Region(RegionState),
}

enum RegionOutcome {
    Continue,
    Cancel,
    Commit(Rect), // crop rectangle in image-space pixels
}

/// Result of the crop tool's Apply / Cancel / Remove bar.
enum CropAction {
    Apply,
    Cancel,
    Remove,
}

/// An in-progress drag gesture on the canvas.
enum Gesture {
    CreatingShape,
    DrawingStroke, // pen or highlighter
    Moving { last: Pos2 },
    Resizing { handle: usize },
    CropNew,
    CropMove { grab: Vec2 },
    CropHandle { which: u8 },
}

struct Drag {
    gesture: Gesture,
    start: Pos2,
    preview: Option<Annotation>,
    moved: bool,
}

struct TextEditState {
    idx: usize,
    buffer: String,
    focused: bool,
}

pub struct SnapEdit {
    docs: Vec<Document>,
    active: Option<usize>,
    tool: Tool,
    prev_tool: Tool,
    color: Color32,
    stroke_width: f32,
    highlight_width: f32,
    fill: bool,
    font_size: f32,
    text_bg: Option<Color32>,
    capture: CaptureState,
    picker: Option<Vec<capture::WindowInfo>>,
    monitor_picker: Option<Vec<capture::MonitorInfo>>,
    drag: Option<Drag>,
    crop_edit: Option<Rect>, // working crop rect while the Crop tool is active
    text_edit: Option<TextEditState>,
    status: String,
    last_save_dir: Option<PathBuf>,
    saved_pos: Option<Pos2>, // window position saved across a capture
    uid_counter: u64,
    doc_counter: u64,
}

impl SnapEdit {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        // Embed the Phosphor icon font so toolbar buttons can use vector icons.
        let mut fonts = egui::FontDefinitions::default();
        egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
        cc.egui_ctx.set_fonts(fonts);
        let mut app = Self {
            docs: Vec::new(),
            active: None,
            tool: Tool::Select,
            prev_tool: Tool::Select,
            color: PALETTE[0],
            stroke_width: 4.0,
            highlight_width: 16.0,
            fill: false,
            font_size: 28.0,
            text_bg: None,
            capture: CaptureState::Idle,
            picker: None,
            monitor_picker: None,
            drag: None,
            crop_edit: None,
            text_edit: None,
            status: "Take a screenshot to begin.".to_owned(),
            last_save_dir: None,
            saved_pos: None,
            uid_counter: 1,
            doc_counter: 0,
        };
        // `--demo` loads a synthetic image so the editor can be exercised without a capture.
        if std::env::args().any(|a| a == "--demo") {
            app.push_doc("Demo", demo_image());
        }
        app
    }

    // ---- document management ------------------------------------------------

    fn push_doc(&mut self, prefix: &str, c: capture::Captured) {
        let (w, h) = (c.width, c.height);
        self.doc_counter += 1;
        let mut doc = Document::from_rgba(format!("{prefix} {}", self.doc_counter), w, h, c.rgba);
        doc.uid = self.uid_counter;
        self.uid_counter += 1;
        self.docs.push(doc);
        self.active = Some(self.docs.len() - 1);
        self.status = format!("Captured {w}×{h}");
    }

    fn close_tab(&mut self, i: usize) {
        if i < self.docs.len() {
            self.docs.remove(i);
            self.active = if self.docs.is_empty() {
                None
            } else {
                Some(self.active.unwrap_or(0).min(self.docs.len() - 1))
            };
            self.drag = None;
            self.text_edit = None;
        }
    }

    fn ensure_textures(&mut self, ctx: &egui::Context) {
        for doc in &mut self.docs {
            if doc.texture.is_none() {
                let ci = egui::ColorImage::from_rgba_unmultiplied(doc.size, &doc.base_rgba);
                doc.texture = Some(ctx.load_texture(
                    format!("snapedit-doc-{}", doc.uid),
                    ci,
                    egui::TextureOptions::LINEAR,
                ));
            }
        }
    }

    // ---- selection editing helpers -----------------------------------------

    fn selected_mut(&mut self) -> Option<&mut Annotation> {
        let idx = self.active?;
        let sel = self.docs[idx].selection?;
        self.docs[idx].annotations.get_mut(sel)
    }

    fn apply_color_to_selection(&mut self, c: Color32) {
        if let Some(idx) = self.active {
            if self.docs[idx].selection.is_some() {
                self.docs[idx].push_undo();
                if let Some(a) = self.selected_mut() {
                    a.color = c;
                }
            }
        }
    }

    fn apply_width_to_selection(&mut self, w: f32, push_undo: bool) {
        if let Some(idx) = self.active {
            if self.docs[idx].selection.is_some() {
                if push_undo {
                    self.docs[idx].push_undo();
                }
                if let Some(a) = self.selected_mut() {
                    a.width = w;
                }
            }
        }
    }

    fn apply_fill_to_selection(&mut self, fill: bool) {
        if let Some(idx) = self.active {
            if self.docs[idx].selection.is_some() {
                self.docs[idx].push_undo();
                if let Some(a) = self.selected_mut() {
                    a.fill = fill;
                }
            }
        }
    }

    fn apply_font_size_to_selection(&mut self, size: f32, push_undo: bool) {
        if let Some(idx) = self.active {
            if self.docs[idx].selection.is_some() {
                if push_undo {
                    self.docs[idx].push_undo();
                }
                if let Some(a) = self.selected_mut() {
                    a.font_size = size;
                }
            }
        }
    }

    fn apply_text_bg_to_selection(&mut self, bg: Option<Color32>) {
        if let Some(idx) = self.active {
            let is_text = self
                .docs[idx]
                .selection
                .and_then(|s| self.docs[idx].annotations.get(s))
                .map(|a| matches!(a.kind, Kind::Text { .. }))
                .unwrap_or(false);
            if is_text {
                self.docs[idx].push_undo();
                if let Some(a) = self.selected_mut() {
                    a.bg = bg;
                }
            }
        }
    }

    // ---- output -------------------------------------------------------------

    fn copy_active(&mut self) {
        let Some(i) = self.active else { return };
        match export::flatten(&self.docs[i]) {
            Ok(f) => {
                self.status = match output::copy_to_clipboard(&f) {
                    Ok(()) => "Copied image to clipboard".to_owned(),
                    Err(e) => e,
                };
            }
            Err(e) => self.status = e,
        }
    }

    fn save_active(&mut self) {
        let Some(i) = self.active else { return };
        let name = format!("{}.png", self.docs[i].title.replace(' ', "_"));
        match export::flatten(&self.docs[i]) {
            Ok(f) => match output::save_with_dialog(&f, &name, self.last_save_dir.as_deref()) {
                Ok(Some(path)) => {
                    self.last_save_dir = path.parent().map(|p| p.to_path_buf());
                    self.docs[i].saved_path = Some(path.clone());
                    self.docs[i].dirty = false;
                    self.status = format!("Saved {}", path.display());
                }
                Ok(None) => {}
                Err(e) => self.status = e,
            },
            Err(e) => self.status = e,
        }
    }

    // ---- capture ------------------------------------------------------------

    fn start_capture(&mut self, kind: PendingKind, ctx: &egui::Context) {
        let fire = ctx.input(|i| i.time) + CAPTURE_DELAY;
        // Remember where the window is, then slide it off-screen for the capture.
        // (Moving off-screen — rather than minimizing — keeps the window in a normal
        // state, so it reliably keeps repainting and reliably comes back focused.)
        self.saved_pos = Some(
            ctx.input(|i| i.viewport().outer_rect)
                .map(|r| r.min)
                .unwrap_or(egui::pos2(80.0, 80.0)),
        );
        self.capture = CaptureState::Pending { kind, fire };
        self.picker = None;
        self.monitor_picker = None;
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(-32000.0, -32000.0)));
        ctx.request_repaint_after(Duration::from_secs_f64(CAPTURE_DELAY));
    }

    /// Start a full-screen capture. With one monitor, grab it immediately; with
    /// several, open the display picker so the user can choose which one (or all).
    fn request_full_capture(&mut self, ctx: &egui::Context) {
        let monitors = capture::list_monitors();
        if monitors.len() <= 1 {
            self.start_capture(PendingKind::Full, ctx);
        } else {
            self.monitor_picker = Some(monitors);
        }
    }

    fn restore_main(&mut self, ctx: &egui::Context) {
        if let Some(pos) = self.saved_pos.take() {
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    fn handle_capture(&mut self, ctx: &egui::Context) {
        let pending = if let CaptureState::Pending { kind, fire } = &self.capture {
            Some((*kind, *fire))
        } else {
            None
        };

        if let Some((kind, fire)) = pending {
            let now = ctx.input(|i| i.time);
            if now < fire {
                ctx.request_repaint_after(Duration::from_secs_f64(fire - now));
                return;
            }
            self.capture = CaptureState::Idle;
            match kind {
                PendingKind::Full => {
                    match capture::capture_full() {
                        Ok(c) => self.push_doc("Screenshot", c),
                        Err(e) => self.status = e,
                    }
                    self.restore_main(ctx);
                }
                PendingKind::Monitor(id) => {
                    match capture::capture_monitor(id) {
                        Ok(c) => self.push_doc("Screenshot", c),
                        Err(e) => self.status = e,
                    }
                    self.restore_main(ctx);
                }
                PendingKind::AllMonitors => {
                    match capture::capture_all_monitors() {
                        Ok(vd) => self.push_doc("Screenshot", vd.captured),
                        Err(e) => self.status = e,
                    }
                    self.restore_main(ctx);
                }
                PendingKind::Window(id) => {
                    match capture::capture_window(id) {
                        Ok(c) => self.push_doc("Window", c),
                        Err(e) => self.status = e,
                    }
                    self.restore_main(ctx);
                }
                PendingKind::Region => match capture::capture_for_region() {
                    Ok(vd) => {
                        let c = vd.captured;
                        let ci = egui::ColorImage::from_rgba_unmultiplied(
                            [c.width as usize, c.height as usize],
                            &c.rgba,
                        );
                        let texture =
                            ctx.load_texture("snapedit-region", ci, egui::TextureOptions::LINEAR);
                        // Restore the main window now (the always-on-top overlay will cover
                        // it) so the overlay isn't a child of a minimized window.
                        self.restore_main(ctx);
                        self.capture = CaptureState::Region(RegionState {
                            width: c.width,
                            height: c.height,
                            rgba: c.rgba,
                            texture,
                            origin_x: vd.origin_x,
                            origin_y: vd.origin_y,
                            scale_factor: vd.scale_factor,
                            start: None,
                            current: None,
                        });
                    }
                    Err(e) => {
                        self.status = e;
                        self.restore_main(ctx);
                    }
                },
            }
            return;
        }

        if matches!(self.capture, CaptureState::Region(_)) {
            self.run_region_overlay(ctx);
        }
    }

    fn run_region_overlay(&mut self, ctx: &egui::Context) {
        let mut region = match std::mem::replace(&mut self.capture, CaptureState::Idle) {
            CaptureState::Region(r) => r,
            other => {
                self.capture = other;
                return;
            }
        };

        let mut outcome = RegionOutcome::Continue;
        // One borderless overlay spanning the entire virtual desktop, so the cursor
        // can select a region across every monitor. The builder takes logical points;
        // convert physical bounds with the primary scale factor as an initial guess,
        // then refine to exact physical bounds each frame inside the callback.
        let sf = region.scale_factor.max(0.1);
        let init_pos = pos2(region.origin_x as f32 / sf, region.origin_y as f32 / sf);
        let init_size = vec2(region.width as f32 / sf, region.height as f32 / sf);
        let builder = egui::ViewportBuilder::default()
            .with_title("SnapEdit — select region")
            .with_decorations(false)
            .with_position(init_pos)
            .with_inner_size(init_size)
            .with_always_on_top()
            .with_taskbar(false);
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("snapedit-region"),
            builder,
            |ui, _class| {
                place_overlay(ui.ctx(), &region);
                outcome = render_region(ui, &mut region);
            },
        );

        match outcome {
            RegionOutcome::Continue => self.capture = CaptureState::Region(region),
            RegionOutcome::Cancel => self.restore_main(ctx),
            RegionOutcome::Commit(img_rect) => {
                if let Some(c) = crop_captured(&region, img_rect) {
                    self.push_doc("Region", c);
                }
                self.restore_main(ctx);
            }
        }
    }

    // ---- keyboard -----------------------------------------------------------

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if !matches!(self.capture, CaptureState::Idle) {
            return;
        }
        let typing = ctx.egui_wants_keyboard_input();
        let (cmd, shift) = ctx.input(|i| (i.modifiers.command, i.modifiers.shift));
        let pressed = |k: egui::Key| ctx.input(|i| i.key_pressed(k));
        use egui::Key;

        if cmd && pressed(Key::C) {
            self.copy_active();
        }
        if cmd && pressed(Key::S) {
            self.save_active();
        }
        if cmd && pressed(Key::N) {
            self.request_full_capture(ctx);
        }
        if cmd && pressed(Key::W) {
            if let Some(i) = self.active {
                self.close_tab(i);
            }
        }
        if cmd && shift && pressed(Key::Z) || cmd && pressed(Key::Y) {
            if let Some(i) = self.active {
                self.docs[i].redo();
            }
        } else if cmd && pressed(Key::Z) {
            if let Some(i) = self.active {
                self.docs[i].undo();
            }
        }

        if !typing {
            if pressed(Key::Delete) || pressed(Key::Backspace) {
                if let Some(i) = self.active {
                    if let Some(sel) = self.docs[i].selection {
                        self.docs[i].push_undo();
                        self.docs[i].annotations.remove(sel);
                        self.docs[i].selection = None;
                    }
                }
            }
            if pressed(Key::Escape) {
                if let Some(i) = self.active {
                    self.docs[i].selection = None;
                }
            }
            for (k, t) in [
                (Key::V, Tool::Select),
                (Key::A, Tool::Arrow),
                (Key::H, Tool::Highlight),
                (Key::R, Tool::Rect),
                (Key::E, Tool::Ellipse),
                (Key::T, Tool::Text),
                (Key::P, Tool::Pen),
                (Key::C, Tool::Crop),
            ] {
                if pressed(k) {
                    self.tool = t;
                }
            }
        }
    }

    // ---- panels -------------------------------------------------------------

    fn top_toolbar(&mut self, ui: &mut egui::Ui) {
        use egui_phosphor::regular as ph;
        egui::Panel::top("toolbar").show_inside(ui, |ui| {
            ui.add_space(5.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().button_padding = vec2(9.0, 8.0);
                ui.spacing_mut().item_spacing.x = 5.0;

                // Capture group.
                if icon_button(ui, ph::CAMERA, "Full screen  (Ctrl+N)", true).clicked() {
                    self.request_full_capture(ui.ctx());
                }
                if icon_button(ui, ph::APP_WINDOW, "Capture a window", true).clicked() {
                    self.picker = Some(capture::list_windows());
                }
                if icon_button(ui, ph::SELECTION, "Capture a region", true).clicked() {
                    self.start_capture(PendingKind::Region, ui.ctx());
                }

                ui.separator();

                // Tool group.
                let has_doc = self.active.is_some();
                for (t, ic, tip) in [
                    (Tool::Select, ph::CURSOR, "Select / move  (V)"),
                    (Tool::Arrow, ph::ARROW_UP_RIGHT, "Arrow  (A)"),
                    (Tool::Highlight, ph::HIGHLIGHTER, "Highlight  (H)"),
                    (Tool::Rect, ph::RECTANGLE, "Rectangle  (R)"),
                    (Tool::Ellipse, ph::CIRCLE, "Ellipse  (E)"),
                    (Tool::Text, ph::TEXT_T, "Text  (T)"),
                    (Tool::Pen, ph::PENCIL_SIMPLE, "Pen  (P)"),
                    (Tool::Crop, ph::CROP, "Crop  (C)"),
                ] {
                    if tool_button(ui, self.tool == t, ic, tip).clicked() {
                        self.tool = t;
                    }
                }

                ui.separator();

                // Tool-settings popover.
                self.style_menu(ui);

                ui.separator();

                let (can_undo, can_redo) = self
                    .active
                    .map(|i| (self.docs[i].can_undo(), self.docs[i].can_redo()))
                    .unwrap_or((false, false));
                if icon_button(ui, ph::ARROW_COUNTER_CLOCKWISE, "Undo  (Ctrl+Z)", can_undo).clicked()
                {
                    if let Some(i) = self.active {
                        self.docs[i].undo();
                    }
                }
                if icon_button(ui, ph::ARROW_CLOCKWISE, "Redo  (Ctrl+Y)", can_redo).clicked() {
                    if let Some(i) = self.active {
                        self.docs[i].redo();
                    }
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icon_button(ui, ph::FLOPPY_DISK, "Save  (Ctrl+S)", has_doc).clicked() {
                        self.save_active();
                    }
                    if icon_button(ui, ph::COPY, "Copy to clipboard  (Ctrl+C)", has_doc).clicked() {
                        self.copy_active();
                    }
                });
            });
            ui.add_space(5.0);
        });
    }

    /// Popover with the settings relevant to the current tool (and the current selection).
    fn style_menu(&mut self, ui: &mut egui::Ui) {
        use egui_phosphor::regular as ph;
        let tool = self.tool;
        let resp = ui
            .menu_button(big_icon(ph::PALETTE), |ui| {
                ui.set_min_width(250.0);
                ui.horizontal(|ui| {
                    ui.strong("Style");
                    if self.active.and_then(|i| self.docs[i].selection).is_some() {
                        ui.weak("· applies to selection");
                    }
                });
                ui.separator();

                if tool == Tool::Crop {
                    ui.label("Drag the handles to set the crop area,");
                    ui.label("then use the Apply / Cancel bar on the image.");
                    return;
                }

                ui.label("Color");
                let picked = color_swatches(ui, &mut self.color);
                if picked {
                    self.apply_color_to_selection(self.color);
                }
                // Inline picker (no nested popup, so it doesn't dismiss the menu).
                if egui::color_picker::color_picker_color32(
                    ui,
                    &mut self.color,
                    egui::color_picker::Alpha::Opaque,
                ) {
                    self.apply_color_to_selection(self.color);
                }

                if matches!(tool, Tool::Arrow | Tool::Rect | Tool::Ellipse | Tool::Pen) {
                    ui.add_space(6.0);
                    ui.label("Stroke width");
                    let sw =
                        ui.add(egui::Slider::new(&mut self.stroke_width, 1.0..=24.0).step_by(1.0));
                    if sw.changed() {
                        self.apply_width_to_selection(self.stroke_width, sw.drag_started());
                    }
                }
                if tool == Tool::Highlight {
                    ui.add_space(6.0);
                    ui.label("Highlighter size");
                    let sw = ui
                        .add(egui::Slider::new(&mut self.highlight_width, 6.0..=48.0).step_by(1.0));
                    if sw.changed() {
                        self.apply_width_to_selection(self.highlight_width, sw.drag_started());
                    }
                }
                if matches!(tool, Tool::Rect | Tool::Ellipse) {
                    ui.add_space(6.0);
                    if ui.checkbox(&mut self.fill, "Fill shape").changed() {
                        self.apply_fill_to_selection(self.fill);
                    }
                }
                if tool == Tool::Text {
                    ui.add_space(6.0);
                    ui.label("Text size");
                    let ts =
                        ui.add(egui::Slider::new(&mut self.font_size, 10.0..=120.0).step_by(1.0));
                    if ts.changed() {
                        self.apply_font_size_to_selection(self.font_size, ts.drag_started());
                    }
                    ui.add_space(6.0);
                    let mut has_bg = self.text_bg.is_some();
                    if ui.checkbox(&mut has_bg, "Background").changed() {
                        self.text_bg = if has_bg {
                            Some(Color32::from_rgba_unmultiplied(0, 0, 0, 160))
                        } else {
                            None
                        };
                        self.apply_text_bg_to_selection(self.text_bg);
                    }
                    if let Some(mut bg) = self.text_bg {
                        if egui::color_picker::color_picker_color32(
                            ui,
                            &mut bg,
                            egui::color_picker::Alpha::OnlyBlend,
                        ) {
                            self.text_bg = Some(bg);
                            self.apply_text_bg_to_selection(self.text_bg);
                        }
                    }
                }
            })
            .response;
        resp.on_hover_text("Style for the selected tool");
    }

    fn tabs_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("tabs").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                let mut to_select = None;
                let mut to_close = None;
                for (i, d) in self.docs.iter().enumerate() {
                    let title = if d.dirty {
                        format!("{} •", d.title)
                    } else {
                        d.title.clone()
                    };
                    if ui.selectable_label(self.active == Some(i), title).clicked() {
                        to_select = Some(i);
                    }
                    if ui.small_button("✕").clicked() {
                        to_close = Some(i);
                    }
                    ui.separator();
                }
                if let Some(i) = to_select {
                    self.active = Some(i);
                }
                if let Some(i) = to_close {
                    self.close_tab(i);
                }
            });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        use egui_phosphor::regular as ph;
        egui::Panel::bottom("status").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                let mut remove_crop = false;
                if let Some(i) = self.active {
                    let d = &self.docs[i];
                    if let Some(c) = d.crop {
                        let r = c.intersect(d.full_rect());
                        // Active crop indicator: show the cropped output size + a remove shortcut.
                        ui.colored_label(
                            ACCENT,
                            format!("{} {}×{}", ph::CROP, r.width().round() as i32, r.height().round() as i32),
                        );
                        if ui
                            .small_button("Remove")
                            .on_hover_text("Remove the crop")
                            .clicked()
                        {
                            remove_crop = true;
                        }
                    } else {
                        ui.label(format!("{}×{}", d.size[0], d.size[1]));
                    }
                    ui.separator();
                    ui.label(format!("{:.0}%", d.view.zoom * 100.0));
                    ui.separator();
                }
                ui.label(self.tool.label());
                ui.separator();
                ui.label(&self.status);

                if remove_crop {
                    if let Some(i) = self.active {
                        self.docs[i].push_undo();
                        self.docs[i].crop = None;
                        self.docs[i].view.initialized = false;
                    }
                }
            });
        });
    }

    fn window_picker(&mut self, ctx: &egui::Context) {
        if self.picker.is_none() {
            return;
        }
        let mut open = true;
        let mut chosen = None;
        egui::Window::new("Select a window")
            .collapsible(false)
            .resizable(true)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label("Pick a window to capture:");
                ui.add_space(4.0);
                egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
                    if let Some(list) = &self.picker {
                        if list.is_empty() {
                            ui.label("No capturable windows found.");
                        }
                        for w in list {
                            let label = if w.app.is_empty() {
                                w.title.clone()
                            } else {
                                format!("{}  —  {}", w.title, w.app)
                            };
                            if ui.button(label).clicked() {
                                chosen = Some(w.id);
                            }
                        }
                    }
                });
            });
        if let Some(id) = chosen {
            self.start_capture(PendingKind::Window(id), ctx);
        } else if !open {
            self.picker = None;
        }
    }

    fn monitor_picker_ui(&mut self, ctx: &egui::Context) {
        use egui_phosphor::regular as ph;
        if self.monitor_picker.is_none() {
            return;
        }
        let mut open = true;
        let mut chosen: Option<PendingKind> = None;
        egui::Window::new("Select a display")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label("Pick a display to capture:");
                ui.add_space(4.0);
                if ui
                    .button(format!("{}  All displays", ph::MONITOR))
                    .clicked()
                {
                    chosen = Some(PendingKind::AllMonitors);
                }
                ui.separator();
                if let Some(list) = &self.monitor_picker {
                    for (i, m) in list.iter().enumerate() {
                        let mut label = format!("{}  Display {}", ph::MONITOR, i + 1);
                        if !m.name.trim().is_empty() {
                            label.push_str(&format!("  ·  {}", m.name));
                        }
                        label.push_str(&format!("  —  {}×{}", m.width, m.height));
                        if m.is_primary {
                            label.push_str("  ·  Primary");
                        }
                        if ui.button(label).clicked() {
                            chosen = Some(PendingKind::Monitor(m.id));
                        }
                    }
                }
            });
        if let Some(kind) = chosen {
            self.monitor_picker = None;
            self.start_capture(kind, ctx);
        } else if !open {
            self.monitor_picker = None;
        }
    }

    fn welcome(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(72.0);
            ui.heading("SnapEdit");
            ui.add_space(6.0);
            ui.label("Capture a screenshot, mark it up, and copy it straight into an AI chat.");
            ui.add_space(24.0);
            if ui.button("📷  Full screen").clicked() {
                self.request_full_capture(ui.ctx());
            }
            ui.add_space(6.0);
            if ui.button("🗗  Window…").clicked() {
                self.picker = Some(capture::list_windows());
            }
            ui.add_space(6.0);
            if ui.button("⛶  Region").clicked() {
                self.start_capture(PendingKind::Region, ui.ctx());
            }
        });
    }

    // ---- canvas -------------------------------------------------------------

    fn canvas_ui(&mut self, ui: &mut egui::Ui) {
        let Some(idx) = self.active else {
            self.welcome(ui);
            return;
        };

        let tool = self.tool;
        let color = self.color;
        let stroke_w = if tool == Tool::Highlight {
            self.highlight_width
        } else {
            self.stroke_width
        };
        let fill = self.fill;
        let font_size = self.font_size;
        let text_bg = self.text_bg;
        let mut drag = self.drag.take();
        let mut crop_edit = self.crop_edit.take();
        let mut text_edit = self.text_edit.take();
        let mut start_edit: Option<usize> = None;
        // Outcome of the crop Apply/Cancel/Remove bar, handled after the doc borrow.
        let mut crop_action: Option<CropAction> = None;
        if tool != Tool::Crop {
            crop_edit = None;
        }

        // Entering/leaving crop mode changes the visible region — re-fit.
        if tool != self.prev_tool && (tool == Tool::Crop || self.prev_tool == Tool::Crop) {
            self.docs[idx].view.initialized = false;
        }
        self.prev_tool = tool;

        {
            let doc = &mut self.docs[idx];
            let (resp, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
            let canvas = resp.rect;

            let cropping = tool == Tool::Crop;
            let full = doc.full_rect();
            // While cropping, show the whole image; otherwise show the cropped region.
            let eff = if cropping { full } else { doc.crop.unwrap_or(full) };

            if !doc.view.initialized {
                doc.view.fit(canvas, eff);
            }

            // Zoom around the cursor (scroll wheel / pinch).
            if resp.hovered() {
                let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                let pinch = ui.input(|i| i.zoom_delta());
                let factor = if (pinch - 1.0).abs() > f32::EPSILON {
                    pinch
                } else if scroll.abs() > 0.0 {
                    (scroll * 0.0015).exp()
                } else {
                    1.0
                };
                if (factor - 1.0).abs() > f32::EPSILON {
                    if let Some(ptr) = resp.hover_pos() {
                        let before = doc.view.to_image(canvas.min, ptr);
                        doc.view.zoom = (doc.view.zoom * factor).clamp(0.05, 20.0);
                        let after = doc.view.to_screen(canvas.min, before);
                        doc.view.offset += ptr - after;
                    }
                }
            }

            // Clip drawing to the visible (effective) region.
            let eff_screen = screen_rect(&doc.view, canvas.min, eff).intersect(canvas);
            let clip = painter.with_clip_rect(eff_screen);

            // Draw base image (clipped to the effective region).
            if let Some(tex) = &doc.texture {
                let tl = doc.view.to_screen(canvas.min, full.min);
                let br = doc.view.to_screen(canvas.min, full.max);
                let img_rect = Rect::from_two_pos(tl, br);
                let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                clip.image(tex.id(), img_rect, uv, Color32::WHITE);
            }

            // Pan with middle-drag or space+drag; otherwise run the active tool.
            let space = ui.input(|i| i.key_down(egui::Key::Space));
            let shift = ui.input(|i| i.modifiers.shift);
            let panning =
                resp.dragged_by(egui::PointerButton::Middle) || (space && resp.dragged());
            if panning {
                doc.view.offset += resp.drag_delta();
            } else {
                // Capture the transform by value so the closure doesn't borrow `doc`.
                let view = doc.view;
                let cmin = canvas.min;
                let to_img = move |p: Pos2| view.to_image(cmin, p);
                match tool {
                    Tool::Select => {
                        if resp.double_clicked() {
                            // Double-click a text object to re-edit it in place.
                            if let Some(p) = resp.interact_pointer_pos().map(to_img) {
                                let tol = 8.0 / doc.view.zoom.max(0.01);
                                if let Some(i) = doc
                                    .annotations
                                    .iter()
                                    .enumerate()
                                    .rev()
                                    .find(|(_, a)| matches!(a.kind, Kind::Text { .. }) && a.hit(p, tol))
                                    .map(|(i, _)| i)
                                {
                                    doc.selection = Some(i);
                                    start_edit = Some(i);
                                }
                            }
                        } else {
                            handle_select(doc, &resp, &mut drag, to_img);
                        }
                    }
                    Tool::Text => {
                        if text_edit.is_none() && resp.clicked() {
                            if let Some(p) = resp.interact_pointer_pos().map(to_img) {
                                doc.push_undo();
                                doc.annotations.push(Annotation {
                                    kind: Kind::Text {
                                        pos: p,
                                        text: String::new(),
                                    },
                                    color,
                                    width: stroke_w,
                                    fill,
                                    font_size,
                                    bg: text_bg,
                                });
                                let i = doc.annotations.len() - 1;
                                doc.selection = Some(i);
                                start_edit = Some(i);
                            }
                        }
                    }
                    Tool::Crop => {
                        let rect = crop_edit.get_or_insert_with(|| {
                            doc.crop.unwrap_or_else(|| {
                                let m = (full.width().min(full.height()) * 0.08).max(8.0);
                                Rect::from_min_max(full.min + vec2(m, m), full.max - vec2(m, m))
                            })
                        });
                        handle_crop(&resp, &mut drag, rect, full, doc.view.zoom, to_img);
                    }
                    _ => handle_draw(
                        doc, tool, color, stroke_w, fill, font_size, shift, &resp, &mut drag,
                        to_img,
                    ),
                }
            }

            // Which text (if any) is being edited — skip drawing its glyphs.
            let editing_idx = text_edit.as_ref().map(|t| t.idx).or(start_edit);

            // Render existing annotations (clipped).
            for (i, ann) in doc.annotations.iter().enumerate() {
                let editing = Some(i) == editing_idx;
                paint_annotation(
                    &clip,
                    &doc.view,
                    canvas.min,
                    ann,
                    doc.selection == Some(i) && !cropping,
                    editing,
                );
            }
            // Render in-progress preview.
            if let Some(d) = &drag {
                if let Some(prev) = &d.preview {
                    paint_annotation(&clip, &doc.view, canvas.min, prev, false, false);
                }
            }

            // Crop overlay: dim outside the working crop rect, draw handles, and show
            // an Apply / Cancel / Remove bar at the top of the canvas.
            if cropping {
                if let Some(rect) = crop_edit {
                    draw_crop_overlay(&painter, &doc.view, canvas, full, rect);
                }
                let has_crop = doc.crop.is_some();
                egui::Area::new(egui::Id::new("snapedit-crop-bar"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(pos2(canvas.center().x - 150.0, canvas.top() + 10.0))
                    .show(ui.ctx(), |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                if ui.button("✓  Apply").clicked() {
                                    crop_action = Some(CropAction::Apply);
                                }
                                if ui.button("✕  Cancel").clicked() {
                                    crop_action = Some(CropAction::Cancel);
                                }
                                if ui
                                    .add_enabled(has_crop, egui::Button::new("🗑  Remove crop"))
                                    .clicked()
                                {
                                    crop_action = Some(CropAction::Remove);
                                }
                            });
                        });
                    });
            }

            // Begin a text edit if requested this frame.
            if let Some(i) = start_edit {
                let buffer = match doc.annotations.get(i).map(|a| &a.kind) {
                    Some(Kind::Text { text, .. }) => text.clone(),
                    _ => String::new(),
                };
                text_edit = Some(TextEditState {
                    idx: i,
                    buffer,
                    focused: false,
                });
            }

            // In-place text editor: a transparent box positioned exactly where the
            // text renders, so the user types directly on the image.
            if let Some(te) = &mut text_edit {
                let mut finished = false;
                if let Some(ann) = doc.annotations.get_mut(te.idx) {
                    let (color_t, size_t) = (ann.color, ann.font_size);
                    if let Kind::Text { pos, text } = &mut ann.kind {
                        let screen = doc.view.to_screen(canvas.min, *pos);
                        let fid = FontId::proportional((size_t * doc.view.zoom).max(8.0));
                        let avail_w = (canvas.right() - screen.x - 8.0).max(120.0);
                        let area = egui::Area::new(egui::Id::new(("snapedit-text", te.idx)))
                            .order(egui::Order::Foreground)
                            .fixed_pos(screen)
                            .show(ui.ctx(), |ui| {
                                ui.add(
                                    egui::TextEdit::multiline(&mut te.buffer)
                                        .frame(egui::Frame::NONE)
                                        .margin(egui::Margin::ZERO)
                                        .font(fid)
                                        .text_color(color_t)
                                        .desired_width(avail_w),
                                )
                            });
                        if !te.focused {
                            area.inner.request_focus();
                            te.focused = true;
                        }
                        *text = te.buffer.clone();
                        let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
                        if area.inner.lost_focus() || esc {
                            finished = true;
                        }
                    } else {
                        finished = true;
                    }
                } else {
                    finished = true;
                }
                if finished {
                    let empty = doc
                        .annotations
                        .get(te.idx)
                        .map(|a| matches!(&a.kind, Kind::Text { text, .. } if text.trim().is_empty()))
                        .unwrap_or(true);
                    if empty && te.idx < doc.annotations.len() {
                        doc.annotations.remove(te.idx);
                        doc.selection = None;
                    }
                    text_edit = None;
                }
            }
        }

        // Apply the crop bar action (doc borrow has ended).
        match crop_action {
            Some(CropAction::Apply) => {
                if let Some(rect) = crop_edit {
                    let full = self.docs[idx].full_rect();
                    let rect = Rect::from_two_pos(rect.min, rect.max).intersect(full);
                    if rect.width() > 4.0 && rect.height() > 4.0 {
                        self.docs[idx].push_undo();
                        self.docs[idx].crop = Some(rect);
                        self.docs[idx].view.initialized = false;
                    }
                }
                crop_edit = None;
                self.tool = Tool::Select;
            }
            Some(CropAction::Cancel) => {
                crop_edit = None;
                self.tool = Tool::Select;
            }
            Some(CropAction::Remove) => {
                if self.docs[idx].crop.is_some() {
                    self.docs[idx].push_undo();
                    self.docs[idx].crop = None;
                    self.docs[idx].view.initialized = false;
                }
                crop_edit = None;
                self.tool = Tool::Select;
            }
            None => {}
        }

        self.drag = drag;
        self.crop_edit = crop_edit;
        self.text_edit = text_edit;
    }
}

impl eframe::App for SnapEdit {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_capture(&ctx);
        self.ensure_textures(&ctx);
        self.handle_shortcuts(&ctx);

        self.top_toolbar(ui);
        if self.active.is_some() {
            self.tabs_bar(ui);
        }
        self.status_bar(ui);
        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.canvas_ui(ui);
        });
        self.window_picker(&ctx);
        self.monitor_picker_ui(&ctx);
    }
}

// ---- free helpers -----------------------------------------------------------

const ICON_SIZE: f32 = 18.0;
const BTN_SIZE: Vec2 = egui::vec2(38.0, 34.0);

/// A synthetic gradient+grid image used by the `--demo` flag for manual testing.
fn demo_image() -> capture::Captured {
    let (w, h) = (900u32, 600u32);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            if x % 60 == 0 || y % 60 == 0 {
                rgba[i..i + 4].copy_from_slice(&[235, 235, 235, 255]);
            } else {
                rgba[i] = (x * 255 / w) as u8;
                rgba[i + 1] = (y * 255 / h) as u8;
                rgba[i + 2] = 130;
                rgba[i + 3] = 255;
            }
        }
    }
    capture::Captured {
        width: w,
        height: h,
        rgba,
    }
}

fn big_icon(s: &str) -> egui::RichText {
    egui::RichText::new(s).size(ICON_SIZE)
}

/// A fixed-size icon button with a tooltip.
fn icon_button(ui: &mut egui::Ui, icon: &str, tip: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(enabled, egui::Button::new(big_icon(icon)).min_size(BTN_SIZE))
        .on_hover_text(tip)
}

/// A fixed-size, toggleable (selectable) icon button with a tooltip.
fn tool_button(ui: &mut egui::Ui, selected: bool, icon: &str, tip: &str) -> egui::Response {
    ui.add(egui::Button::selectable(selected, big_icon(icon)).min_size(BTN_SIZE))
        .on_hover_text(tip)
}

/// A row of color swatches. Sets `color` and returns true if a swatch was clicked.
fn color_swatches(ui: &mut egui::Ui, color: &mut Color32) -> bool {
    let mut clicked = false;
    ui.horizontal_wrapped(|ui| {
        for &c in PALETTE {
            let (rect, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::click());
            ui.painter().rect_filled(rect, 5.0, c);
            if *color == c {
                ui.painter().rect_stroke(
                    rect.expand(1.5),
                    5.0,
                    Stroke::new(2.0, Color32::WHITE),
                    StrokeKind::Outside,
                );
            }
            if resp.on_hover_text("Set color").clicked() {
                *color = c;
                clicked = true;
            }
        }
    });
    clicked
}

fn handle_select(
    doc: &mut Document,
    resp: &egui::Response,
    drag: &mut Option<Drag>,
    to_img: impl Fn(Pos2) -> Pos2,
) {
    let tol_for = |doc: &Document| 8.0 / doc.view.zoom.max(0.01);

    // Plain click: select the topmost object under the cursor, or deselect on empty space.
    if resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos().map(&to_img) {
            let tol = tol_for(doc);
            doc.selection = doc
                .annotations
                .iter()
                .enumerate()
                .rev()
                .find(|(_, a)| a.hit(p, tol))
                .map(|(i, _)| i);
        }
    }

    if resp.drag_started() {
        if let Some(p) = resp.interact_pointer_pos().map(&to_img) {
            let tol = tol_for(doc);
            // Resize handle of the current selection takes priority.
            if let Some(sel) = doc.selection {
                for (hi, hp) in doc.annotations[sel].handles().iter().enumerate() {
                    if (p - *hp).length() <= tol * 1.6 {
                        *drag = Some(Drag {
                            gesture: Gesture::Resizing { handle: hi },
                            start: p,
                            preview: None,
                            moved: false,
                        });
                        return;
                    }
                }
            }
            // Otherwise pick the topmost annotation under the cursor.
            let hit = doc
                .annotations
                .iter()
                .enumerate()
                .rev()
                .find(|(_, a)| a.hit(p, tol))
                .map(|(i, _)| i);
            doc.selection = hit;
            if hit.is_some() {
                *drag = Some(Drag {
                    gesture: Gesture::Moving { last: p },
                    start: p,
                    preview: None,
                    moved: false,
                });
            }
        }
    }
    if resp.dragged() {
        if let (Some(d), Some(p)) = (drag.as_mut(), resp.interact_pointer_pos().map(&to_img)) {
            match &mut d.gesture {
                Gesture::Moving { last } => {
                    let delta = p - *last;
                    if delta != Vec2::ZERO {
                        if !d.moved {
                            doc.push_undo();
                            d.moved = true;
                        }
                        if let Some(sel) = doc.selection {
                            doc.annotations[sel].translate(delta);
                        }
                        *last = p;
                    }
                }
                Gesture::Resizing { handle } => {
                    if !d.moved {
                        doc.push_undo();
                        d.moved = true;
                    }
                    if let Some(sel) = doc.selection {
                        doc.annotations[sel].move_handle(*handle, p);
                    }
                }
                _ => {}
            }
        }
    }
    if resp.drag_stopped() {
        *drag = None;
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_draw(
    doc: &mut Document,
    tool: Tool,
    color: Color32,
    width: f32,
    fill: bool,
    font_size: f32,
    shift: bool,
    resp: &egui::Response,
    drag: &mut Option<Drag>,
    to_img: impl Fn(Pos2) -> Pos2,
) {
    if resp.drag_started() {
        if let Some(start) = resp.interact_pointer_pos().map(&to_img) {
            let kind = match tool {
                Tool::Arrow => Kind::Arrow { a: start, b: start },
                Tool::Rect => Kind::Rect {
                    rect: Rect::from_two_pos(start, start),
                },
                Tool::Ellipse => Kind::Ellipse {
                    rect: Rect::from_two_pos(start, start),
                },
                Tool::Highlight => Kind::Highlight {
                    points: vec![start],
                },
                Tool::Pen => Kind::Pen {
                    points: vec![start],
                },
                _ => return,
            };
            let gesture = if matches!(tool, Tool::Pen | Tool::Highlight) {
                Gesture::DrawingStroke
            } else {
                Gesture::CreatingShape
            };
            *drag = Some(Drag {
                gesture,
                start,
                preview: Some(Annotation {
                    kind,
                    color,
                    width,
                    fill,
                    font_size,
                    bg: None,
                }),
                moved: false,
            });
        }
    }
    if resp.dragged() {
        if let (Some(d), Some(cur)) = (drag.as_mut(), resp.interact_pointer_pos().map(&to_img)) {
            let start = d.start;
            if let Some(prev) = d.preview.as_mut() {
                match &mut prev.kind {
                    Kind::Arrow { b, .. } => *b = cur,
                    Kind::Rect { rect } | Kind::Ellipse { rect } => {
                        *rect = Rect::from_two_pos(start, cur);
                    }
                    Kind::Pen { points } | Kind::Highlight { points } => {
                        // Hold Shift for a straight line (start -> current).
                        if shift {
                            *points = vec![start, cur];
                        } else {
                            points.push(cur);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    if resp.drag_stopped() {
        if let Some(d) = drag.take() {
            if let Some(prev) = d.preview {
                if is_meaningful(&prev) {
                    doc.push_undo();
                    doc.annotations.push(prev);
                    doc.selection = Some(doc.annotations.len() - 1);
                }
            }
        }
    }
}

/// The 8 grab points of a crop rect in image space:
/// corners 0..4 (LT, RT, RB, LB), edges 4..8 (top, right, bottom, left).
fn crop_handles(rect: Rect) -> [Pos2; 8] {
    let r = Rect::from_two_pos(rect.min, rect.max);
    [
        r.left_top(),
        r.right_top(),
        r.right_bottom(),
        r.left_bottom(),
        r.center_top(),
        r.right_center(),
        r.center_bottom(),
        r.left_center(),
    ]
}

/// Move the given handle of `rect` to `p` (image space).
fn resize_crop(rect: Rect, which: u8, p: Pos2) -> Rect {
    let r = Rect::from_two_pos(rect.min, rect.max);
    let (mut l, mut t, mut ri, mut b) = (r.left(), r.top(), r.right(), r.bottom());
    match which {
        0 => {
            l = p.x;
            t = p.y;
        }
        1 => {
            ri = p.x;
            t = p.y;
        }
        2 => {
            ri = p.x;
            b = p.y;
        }
        3 => {
            l = p.x;
            b = p.y;
        }
        4 => t = p.y,
        5 => ri = p.x,
        6 => b = p.y,
        _ => l = p.x,
    }
    Rect::from_two_pos(pos2(l, t), pos2(ri, b))
}

/// Crop tool: adjust the working `rect` via handles (resize), interior drag (move),
/// or an outside drag (draw a fresh rect). Nothing is committed until "Apply".
fn handle_crop(
    resp: &egui::Response,
    drag: &mut Option<Drag>,
    rect: &mut Rect,
    full: Rect,
    zoom: f32,
    to_img: impl Fn(Pos2) -> Pos2,
) {
    let tol = 9.0 / zoom.max(0.01);
    if resp.drag_started() {
        if let Some(p) = resp.interact_pointer_pos().map(&to_img) {
            let handles = crop_handles(*rect);
            let near = handles
                .iter()
                .enumerate()
                .find(|(_, h)| (p - **h).length() <= tol * 1.5)
                .map(|(i, _)| i as u8);
            let gesture = if let Some(which) = near {
                Gesture::CropHandle { which }
            } else if Rect::from_two_pos(rect.min, rect.max).contains(p) {
                Gesture::CropMove { grab: p - rect.min }
            } else {
                *rect = Rect::from_two_pos(p, p);
                Gesture::CropNew
            };
            *drag = Some(Drag {
                gesture,
                start: p,
                preview: None,
                moved: false,
            });
        }
    }
    if resp.dragged() {
        if let (Some(d), Some(p)) = (drag.as_ref(), resp.interact_pointer_pos().map(&to_img)) {
            match &d.gesture {
                Gesture::CropNew => *rect = Rect::from_two_pos(d.start, p).intersect(full),
                Gesture::CropMove { grab } => {
                    let size = rect.size();
                    let mut min = p - *grab;
                    min.x = min.x.clamp(full.min.x, (full.max.x - size.x).max(full.min.x));
                    min.y = min.y.clamp(full.min.y, (full.max.y - size.y).max(full.min.y));
                    *rect = Rect::from_min_size(min, size);
                }
                Gesture::CropHandle { which } => {
                    *rect = resize_crop(*rect, *which, p).intersect(full)
                }
                _ => {}
            }
        }
    }
    if resp.drag_stopped() {
        *drag = None;
    }
}

fn is_meaningful(ann: &Annotation) -> bool {
    match &ann.kind {
        Kind::Arrow { a, b } => (*b - *a).length() > 3.0,
        Kind::Rect { rect } | Kind::Ellipse { rect } => {
            rect.width().abs() > 3.0 && rect.height().abs() > 3.0
        }
        Kind::Pen { points } | Kind::Highlight { points } => points.len() > 1,
        Kind::Text { text, .. } => !text.trim().is_empty(),
    }
}

fn screen_rect(view: &View, cmin: Pos2, r: Rect) -> Rect {
    Rect::from_two_pos(view.to_screen(cmin, r.min), view.to_screen(cmin, r.max))
}

fn paint_annotation(
    painter: &egui::Painter,
    view: &View,
    cmin: Pos2,
    ann: &Annotation,
    selected: bool,
    editing: bool,
) {
    let z = view.zoom;
    let col = ann.color;
    let w = (ann.width * z).max(1.0);
    match &ann.kind {
        Kind::Arrow { a, b } => {
            let sa = view.to_screen(cmin, *a);
            let sb = view.to_screen(cmin, *b);
            painter.arrow(sa, sb - sa, Stroke::new(w, col));
        }
        Kind::Pen { points } => {
            let pts: Vec<Pos2> = points.iter().map(|p| view.to_screen(cmin, *p)).collect();
            if pts.len() >= 2 {
                painter.add(egui::Shape::line(pts, Stroke::new(w, col)));
            }
        }
        Kind::Highlight { points } => {
            let pts: Vec<Pos2> = points.iter().map(|p| view.to_screen(cmin, *p)).collect();
            let hl = Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), 96);
            if pts.len() >= 2 {
                painter.add(egui::Shape::line(pts, Stroke::new(w, hl)));
            }
        }
        Kind::Rect { rect } => {
            let r = screen_rect(view, cmin, *rect);
            if ann.fill {
                painter.rect_filled(r, 0.0, col);
            } else {
                painter.rect_stroke(r, 0.0, Stroke::new(w, col), StrokeKind::Inside);
            }
        }
        Kind::Ellipse { rect } => {
            let r = screen_rect(view, cmin, *rect);
            let c = r.center();
            let rad = r.size() * 0.5;
            if ann.fill {
                painter.add(egui::Shape::ellipse_filled(c, rad, col));
            } else {
                painter.add(egui::Shape::ellipse_stroke(c, rad, Stroke::new(w, col)));
            }
        }
        Kind::Text { pos, text } => {
            let sp = view.to_screen(cmin, *pos);
            let fid = FontId::proportional((ann.font_size * z).max(8.0));
            // Background fill behind the text block.
            if let Some(bg) = ann.bg {
                let galley = painter.layout_no_wrap(text.clone(), fid.clone(), col);
                let pad = (ann.font_size * z * 0.18).max(1.0);
                let bg_rect = Rect::from_min_size(sp, galley.size()).expand(pad);
                painter.rect_filled(bg_rect, 3.0, bg);
            }
            // Glyphs (skipped while the in-place editor is active for this object).
            if !editing {
                painter.text(sp, Align2::LEFT_TOP, text, fid, col);
            }
        }
    }

    if selected {
        let b = screen_rect(view, cmin, ann.bounds());
        painter.rect_stroke(b.expand(2.0), 0.0, Stroke::new(1.0, ACCENT), StrokeKind::Outside);
        for hp in ann.handles() {
            let s = view.to_screen(cmin, hp);
            let hr = Rect::from_center_size(s, vec2(8.0, 8.0));
            painter.rect_filled(hr, 1.0, Color32::WHITE);
            painter.rect_stroke(hr, 1.0, Stroke::new(1.0, ACCENT), StrokeKind::Outside);
        }
    }
}

/// Dim the area outside the working crop rect, outline it, and draw grab handles.
fn draw_crop_overlay(painter: &egui::Painter, view: &View, canvas: Rect, full: Rect, sel_img: Rect) {
    let img = screen_rect(view, canvas.min, full).intersect(canvas);
    let s = screen_rect(view, canvas.min, Rect::from_two_pos(sel_img.min, sel_img.max))
        .intersect(img);
    let dim = Color32::from_black_alpha(140);
    // Four dim bands around the bright selection.
    painter.rect_filled(
        Rect::from_min_max(img.left_top(), pos2(img.right(), s.top())),
        0.0,
        dim,
    );
    painter.rect_filled(
        Rect::from_min_max(pos2(img.left(), s.bottom()), img.right_bottom()),
        0.0,
        dim,
    );
    painter.rect_filled(
        Rect::from_min_max(pos2(img.left(), s.top()), pos2(s.left(), s.bottom())),
        0.0,
        dim,
    );
    painter.rect_filled(
        Rect::from_min_max(pos2(s.right(), s.top()), pos2(img.right(), s.bottom())),
        0.0,
        dim,
    );
    painter.rect_stroke(s, 0.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Outside);

    // Grab handles (white squares with a dark border).
    for h in crop_handles(sel_img) {
        let c = view.to_screen(canvas.min, h);
        let hr = Rect::from_center_size(c, vec2(11.0, 11.0));
        painter.rect_filled(hr, 2.0, Color32::WHITE);
        painter.rect_stroke(hr, 2.0, Stroke::new(1.0, Color32::from_gray(70)), StrokeKind::Outside);
    }
}

fn crop_captured(st: &RegionState, r: Rect) -> Option<capture::Captured> {
    let x0 = r.min.x.floor().max(0.0) as i64;
    let y0 = r.min.y.floor().max(0.0) as i64;
    let x1 = (r.max.x.ceil() as i64).clamp(0, st.width as i64);
    let y1 = (r.max.y.ceil() as i64).clamp(0, st.height as i64);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let (x0, y0, cw, ch) = (x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32);
    let mut out = vec![0u8; (cw * ch * 4) as usize];
    for y in 0..ch {
        let src = (((y0 + y) * st.width + x0) * 4) as usize;
        let dst = ((y * cw) * 4) as usize;
        let len = (cw * 4) as usize;
        out[dst..dst + len].copy_from_slice(&st.rgba[src..src + len]);
    }
    Some(capture::Captured {
        width: cw,
        height: ch,
        rgba: out,
    })
}

/// Pin the region overlay to exactly cover the virtual desktop in physical pixels.
///
/// `OuterPosition`/`InnerSize` are applied as `pixels_per_point * value`, so dividing
/// the physical bounds by the live ppp lands the window precisely — correcting any
/// mismatch from the builder's logical guess (e.g. mixed-DPI placement). Only nudged
/// when noticeably off, to avoid resize churn.
fn place_overlay(ctx: &egui::Context, st: &RegionState) {
    let ppp = ctx.pixels_per_point().max(0.1);
    let target_pos = pos2(st.origin_x as f32 / ppp, st.origin_y as f32 / ppp);
    let target_size = vec2(st.width as f32 / ppp, st.height as f32 / ppp);
    let cur = ctx.input(|i| i.viewport().outer_rect);
    let off = cur.map_or(true, |r| {
        (r.min.x - target_pos.x).abs() > 1.0
            || (r.min.y - target_pos.y).abs() > 1.0
            || (r.width() - target_size.x).abs() > 1.0
            || (r.height() - target_size.y).abs() > 1.0
    });
    if off {
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(target_pos));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(target_size));
        ctx.request_repaint();
    }
}

fn render_region(ui: &mut egui::Ui, st: &mut RegionState) -> RegionOutcome {
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        return RegionOutcome::Cancel;
    }
    let mut outcome = RegionOutcome::Continue;
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show_inside(ui, |ui| {
            let (resp, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
            let rect = resp.rect;
            let uv_full = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            painter.image(st.texture.id(), rect, uv_full, Color32::WHITE);
            painter.rect_filled(rect, 0.0, Color32::from_black_alpha(120));

            if resp.drag_started() {
                st.start = resp.interact_pointer_pos();
                st.current = st.start;
            }
            if resp.dragged() {
                st.current = resp.interact_pointer_pos();
            }

            if let (Some(a), Some(b)) = (st.start, st.current) {
                let sel = Rect::from_two_pos(a, b).intersect(rect);
                if sel.width() > 0.0 && sel.height() > 0.0 {
                    let uv = Rect::from_min_max(
                        pos2(
                            (sel.min.x - rect.min.x) / rect.width(),
                            (sel.min.y - rect.min.y) / rect.height(),
                        ),
                        pos2(
                            (sel.max.x - rect.min.x) / rect.width(),
                            (sel.max.y - rect.min.y) / rect.height(),
                        ),
                    );
                    painter.image(st.texture.id(), sel, uv, Color32::WHITE);
                    painter.rect_stroke(sel, 0.0, Stroke::new(2.0, ACCENT), StrokeKind::Outside);
                    let sx = st.width as f32 / rect.width();
                    let sy = st.height as f32 / rect.height();
                    let wpx = (sel.width() * sx).round() as i32;
                    let hpx = (sel.height() * sy).round() as i32;
                    painter.text(
                        sel.left_top() + vec2(2.0, -4.0),
                        Align2::LEFT_BOTTOM,
                        format!("{wpx} × {hpx}"),
                        FontId::monospace(13.0),
                        Color32::WHITE,
                    );
                }
            }

            painter.text(
                rect.center_top() + vec2(0.0, 14.0),
                Align2::CENTER_TOP,
                "Drag to select   •   release to capture   •   Esc to cancel",
                FontId::proportional(15.0),
                Color32::from_white_alpha(220),
            );

            if resp.drag_stopped() {
                if let (Some(a), Some(b)) = (st.start, st.current) {
                    let sel = Rect::from_two_pos(a, b).intersect(rect);
                    if sel.width() > 4.0 && sel.height() > 4.0 {
                        let sx = st.width as f32 / rect.width();
                        let sy = st.height as f32 / rect.height();
                        let img_rect = Rect::from_min_max(
                            pos2((sel.min.x - rect.min.x) * sx, (sel.min.y - rect.min.y) * sy),
                            pos2((sel.max.x - rect.min.x) * sx, (sel.max.y - rect.min.y) * sy),
                        );
                        outcome = RegionOutcome::Commit(img_rect);
                    } else {
                        st.start = None;
                        st.current = None;
                    }
                }
            }
        });
    outcome
}
