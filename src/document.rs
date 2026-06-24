//! Core data model: a `Document` is one open screenshot (one tab).
//!
//! All annotation geometry is stored in **image-space** pixel coordinates so that
//! markup stays locked to the image under zoom/pan and exports at full resolution.

use egui::{Color32, Pos2, Rect, TextureHandle, Vec2};
use std::path::PathBuf;

/// Normalize a rect so `min <= max` on both axes (egui 0.34 has no `Rect::normalized`).
pub fn norm(r: Rect) -> Rect {
    Rect::from_two_pos(r.min, r.max)
}

/// The editing tools available in the toolbar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Select,
    Arrow,
    Highlight,
    Rect,
    Ellipse,
    Text,
    Pen,
    Crop,
}

impl Tool {
    pub fn label(self) -> &'static str {
        match self {
            Tool::Select => "Select",
            Tool::Arrow => "Arrow",
            Tool::Highlight => "Highlight",
            Tool::Rect => "Rectangle",
            Tool::Ellipse => "Ellipse",
            Tool::Text => "Text",
            Tool::Pen => "Pen",
            Tool::Crop => "Crop",
        }
    }
}

/// The geometric content of an annotation, in image-space coordinates.
#[derive(Clone, Debug)]
pub enum Kind {
    Arrow { a: Pos2, b: Pos2 },
    Highlight { points: Vec<Pos2> }, // pen-style translucent stroke
    Rect { rect: Rect },
    Ellipse { rect: Rect },
    Text { pos: Pos2, text: String },
    Pen { points: Vec<Pos2> },
}

/// A single drawn object on top of the base image.
#[derive(Clone, Debug)]
pub struct Annotation {
    pub kind: Kind,
    pub color: Color32,
    pub width: f32,
    pub fill: bool,
    pub font_size: f32,
    pub bg: Option<Color32>, // text background fill (Text only)
}

impl Annotation {
    /// Translate the whole annotation by `delta` (image-space pixels).
    pub fn translate(&mut self, delta: Vec2) {
        match &mut self.kind {
            Kind::Arrow { a, b } => {
                *a += delta;
                *b += delta;
            }
            Kind::Rect { rect } | Kind::Ellipse { rect } => {
                *rect = rect.translate(delta);
            }
            Kind::Text { pos, .. } => *pos += delta,
            Kind::Pen { points } | Kind::Highlight { points } => {
                for p in points.iter_mut() {
                    *p += delta;
                }
            }
        }
    }

    /// Axis-aligned bounding box in image space (used for hit-testing & handles).
    pub fn bounds(&self) -> Rect {
        match &self.kind {
            Kind::Arrow { a, b } => Rect::from_two_pos(*a, *b),
            Kind::Rect { rect } | Kind::Ellipse { rect } => norm(*rect),
            Kind::Text { pos, text } => {
                // Rough estimate; good enough for selection hit-testing.
                let longest = text.split('\n').map(|l| l.chars().count()).max().unwrap_or(0);
                let lines = text.split('\n').count().max(1) as f32;
                let w = (longest.max(1) as f32) * self.font_size * 0.55;
                Rect::from_min_size(*pos, Vec2::new(w, self.font_size * 1.3 * lines))
            }
            Kind::Pen { points } | Kind::Highlight { points } => {
                if points.is_empty() {
                    Rect::NOTHING
                } else {
                    let mut r = Rect::from_min_max(points[0], points[0]);
                    for p in points {
                        r.extend_with(*p);
                    }
                    r
                }
            }
        }
    }

    /// Hit-test against a point in image space. `tol` is the pick tolerance
    /// (image-space pixels) for thin objects.
    pub fn hit(&self, p: Pos2, tol: f32) -> bool {
        match &self.kind {
            Kind::Arrow { a, b } => dist_to_segment(p, *a, *b) <= tol.max(self.width),
            Kind::Pen { points } | Kind::Highlight { points } => {
                let reach = tol.max(self.width * 0.5);
                points
                    .windows(2)
                    .any(|w| dist_to_segment(p, w[0], w[1]) <= reach)
                    || points.iter().any(|&q| (p - q).length() <= reach)
            }
            Kind::Rect { rect } | Kind::Ellipse { rect } => norm(*rect).expand(tol).contains(p),
            Kind::Text { .. } => self.bounds().expand(tol).contains(p),
        }
    }

    /// Resize handles for this annotation in image space, or empty if not resizable.
    /// Returns (handle index, position). The interpretation of the index is
    /// kind-specific and consumed by [`Annotation::move_handle`].
    pub fn handles(&self) -> Vec<Pos2> {
        match &self.kind {
            Kind::Arrow { a, b } => vec![*a, *b],
            Kind::Rect { rect } | Kind::Ellipse { rect } => {
                let r = norm(*rect);
                vec![r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()]
            }
            // Text, Pen and Highlight support move-only (no resize handles).
            Kind::Text { .. } | Kind::Pen { .. } | Kind::Highlight { .. } => Vec::new(),
        }
    }

    /// Move resize handle `idx` to `p` (image space).
    pub fn move_handle(&mut self, idx: usize, p: Pos2) {
        match &mut self.kind {
            Kind::Arrow { a, b } => {
                if idx == 0 {
                    *a = p;
                } else {
                    *b = p;
                }
            }
            Kind::Rect { rect } | Kind::Ellipse { rect } => {
                let r = norm(*rect);
                let (min, max) = (r.min, r.max);
                let new = match idx {
                    0 => Rect::from_two_pos(p, max),                              // left-top
                    1 => Rect::from_two_pos(Pos2::new(min.x, p.y), Pos2::new(p.x, max.y)), // right-top
                    2 => Rect::from_two_pos(min, p),                              // right-bottom
                    _ => Rect::from_two_pos(Pos2::new(p.x, min.y), Pos2::new(max.x, p.y)), // left-bottom
                };
                *rect = new;
            }
            _ => {}
        }
    }
}

/// Distance from point `p` to segment `a`-`b`.
fn dist_to_segment(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let len_sq = ab.length_sq();
    if len_sq <= f32::EPSILON {
        return (p - a).length();
    }
    let t = ((p - a).dot(ab) / len_sq).clamp(0.0, 1.0);
    let proj = a + ab * t;
    (p - proj).length()
}

/// View transform mapping image space <-> screen space for display only.
#[derive(Clone, Copy)]
pub struct View {
    pub zoom: f32,
    pub offset: Vec2, // screen-space offset of image origin from the canvas top-left
    pub initialized: bool,
}

impl Default for View {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            offset: Vec2::ZERO,
            initialized: false,
        }
    }
}

impl View {
    pub fn to_screen(&self, canvas_min: Pos2, p: Pos2) -> Pos2 {
        canvas_min + self.offset + (p.to_vec2() * self.zoom)
    }

    pub fn to_image(&self, canvas_min: Pos2, p: Pos2) -> Pos2 {
        ((p - canvas_min - self.offset) / self.zoom).to_pos2()
    }

    /// Fit the effective image region `eff` (image-space rect) centered within `canvas`.
    pub fn fit(&mut self, canvas: Rect, eff: Rect) {
        let (iw, ih) = (eff.width().max(1.0), eff.height().max(1.0));
        let scale = (canvas.width() / iw).min(canvas.height() / ih).min(1.0);
        self.zoom = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
        let shown = Vec2::new(iw * self.zoom, ih * self.zoom);
        // Center the region, accounting for its image-space origin.
        self.offset = (canvas.size() - shown) * 0.5 - eff.min.to_vec2() * self.zoom;
        self.initialized = true;
    }
}

/// A point-in-time snapshot for undo/redo (annotations + crop).
type Snapshot = (Vec<Annotation>, Option<Rect>);

/// Undo/redo via snapshots (simple & correct for v1).
#[derive(Default)]
pub struct History {
    past: Vec<Snapshot>,
    future: Vec<Snapshot>,
}

const HISTORY_LIMIT: usize = 64;

/// One open screenshot.
pub struct Document {
    pub uid: u64, // stable id for texture naming
    pub title: String,
    pub size: [usize; 2],
    pub base_rgba: Vec<u8>, // straight RGBA8, len = w*h*4
    pub texture: Option<TextureHandle>,
    pub annotations: Vec<Annotation>,
    pub selection: Option<usize>,
    pub crop: Option<Rect>, // non-destructive crop, image-space
    pub view: View,
    pub history: History,
    pub saved_path: Option<PathBuf>,
    pub dirty: bool,
}

impl Document {
    pub fn from_rgba(title: impl Into<String>, width: u32, height: u32, rgba: Vec<u8>) -> Self {
        Self {
            uid: 0,
            title: title.into(),
            size: [width as usize, height as usize],
            base_rgba: rgba,
            texture: None,
            annotations: Vec::new(),
            selection: None,
            crop: None,
            view: View::default(),
            history: History::default(),
            saved_path: None,
            dirty: false,
        }
    }

    /// The full image rect in image space.
    pub fn full_rect(&self) -> Rect {
        Rect::from_min_size(
            Pos2::ZERO,
            Vec2::new(self.size[0] as f32, self.size[1] as f32),
        )
    }

    /// Record current state for undo. Call immediately *before* a mutation.
    pub fn push_undo(&mut self) {
        self.history.past.push((self.annotations.clone(), self.crop));
        if self.history.past.len() > HISTORY_LIMIT {
            self.history.past.remove(0);
        }
        self.history.future.clear();
        self.dirty = true;
    }

    pub fn can_undo(&self) -> bool {
        !self.history.past.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.history.future.is_empty()
    }

    pub fn undo(&mut self) {
        if let Some((anns, crop)) = self.history.past.pop() {
            let cur = (std::mem::take(&mut self.annotations), self.crop);
            self.annotations = anns;
            self.crop = crop;
            self.history.future.push(cur);
            self.selection = None;
            self.view.initialized = false; // re-fit (crop may have changed)
            self.dirty = true;
        }
    }

    pub fn redo(&mut self) {
        if let Some((anns, crop)) = self.history.future.pop() {
            let cur = (std::mem::take(&mut self.annotations), self.crop);
            self.annotations = anns;
            self.crop = crop;
            self.history.past.push(cur);
            self.selection = None;
            self.view.initialized = false;
            self.dirty = true;
        }
    }
}
