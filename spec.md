# SnapEdit — Project Spec

> A lightweight, modern screenshot + annotation tool for Windows, written in Rust.
> Built for one job: capture a screenshot, mark it up, and get it into an LLM/agent
> conversation fast — via clipboard or a saved file.

---

## 1. Overview & Goals

SnapEdit is a Snipping-Tool-style desktop app focused on **preparing images as context
for LLMs and AI agents**. The core loop is:

```
capture  →  annotate / crop  →  copy or save  →  paste into an AI chat
```

Design priorities, in order:

1. **Fast** — capture-to-clipboard in a couple of clicks.
2. **Lightweight** — single portable `.exe`, no installer, no runtime deps.
3. **Modern & simple** — clean UI in the spirit of the Windows Snipping Tool.
4. **AI-oriented** — features that matter for sharing context (clean markup, OCR text
   extraction, "copy as" workflows).

### Primary use cases
- Snip a region of the screen, draw an arrow at the thing that's broken, copy, paste into Claude.
- Capture a window, redact/highlight, save as PNG to attach to an agent task.
- Extract text from a screenshot (OCR) so it can be pasted as text instead of an image.

---

## 2. Non-Goals (initial scope)

- No video / GIF capture (images only).
- No cloud sync, accounts, or sign-in (explicit requirement).
- No multi-OS support in v1 (Windows-first; crates chosen to keep Linux/macOS viable later).
- No full raster image editor (no layers, filters, brushes beyond annotation tools).
- No scrolling capture / stitched screenshots in v1.

---

## 3. Target Platform & Portability

- **OS:** Windows 10 / 11 (x64).
- **Artifact:** a single `snapedit.exe`, fully portable (copy-and-run, no install).
- **No external runtime:** statically link the MSVC CRT (`+crt-static`) so the VC++
  redistributable is not required. OCR (Phase 4) uses the OS-provided Windows Runtime —
  still no bundled binaries.
- **No console window:** `#![windows_subsystem = "windows"]`.
- All assets (fonts, icons) embedded into the binary via `include_bytes!`.

---

## 4. Tech Stack

| Concern | Choice | Why |
|---|---|---|
| GUI framework | **`egui`** via **`eframe`** | Immediate-mode GUI is ideal for an interactive canvas (drag-to-draw, live previews, selection handles). Pure-Rust, compiles to a single portable exe, small footprint, great `Painter` API. |
| Render backend | **`glow`** (OpenGL) | More compatible across machines/VMs than wgpu; lighter. (wgpu is a fallback option.) |
| Screen capture | **`xcap`** | Cross-platform capture of **monitors and individual windows**; actively maintained. |
| Image I/O & ops | **`image`** | Decode/encode PNG/JPEG, crop, resize. PNG is the default export format. |
| 2D compositing (export) | **`tiny-skia`** | Pure-Rust, high-quality anti-aliased rendering of the flattened image for export. Deterministic, no GPU readback. |
| Text rasterization | **`ab_glyph`** | Lightweight glyph rasterization for text annotations during export. Annotation text is short, so a simple layout is enough. |
| Clipboard | **`arboard`** | Cross-platform clipboard **with image support** (copy full image). |
| File dialogs | **`rfd`** | Native open/save dialogs for "Save as…". |
| OCR (Phase 4) | **Windows.Media.Ocr** via the **`windows`** crate | Built into Windows 10/11 — no bundled Tesseract, keeps the exe portable. |
| Global hotkey (optional, Phase 4) | **`global-hotkey`** | `Win+Shift+S`-style trigger to start a snip from anywhere. |

### Rendering strategy (important design decision)

Annotations are stored as **data in image-space coordinates**, and there are **two thin
renderers** driven by the same data:

- **Display renderer (egui):** draws the base-image texture plus annotations every frame
  using `egui::Painter`. Handles zoom/pan, live drag previews, and selection handles.
  This is what the user sees and interacts with.
- **Export renderer (tiny-skia):** draws the same annotation data onto a CPU `Pixmap` to
  produce the final flattened PNG for copy/save.

Because the primitive set is small (line/arrow, rect, ellipse, text, highlight, freehand),
keeping two small renderers in sync is cheap, and it gives us crisp interactive display
**and** deterministic, GPU-independent export. The export pixmap is the single source of
truth for "copy" and "save".

---

## 5. Architecture / Module Layout

```
src/
  main.rs            # eframe entry point, window/viewport setup, embedded assets
  app.rs             # App struct: tab list, active tab, top-level UI orchestration
  ui/
    mod.rs
    toolbar.rs       # capture buttons, tool buttons, color/stroke pickers, copy/save
    tabs.rs          # tab strip (open documents, close, switch)
    canvas.rs        # the image canvas: zoom/pan, hit-testing, drag interactions
    statusbar.rs     # zoom %, image dimensions, hints
  capture/
    mod.rs           # public capture API: fullscreen / window / region
    monitors.rs      # enumerate + grab monitors (xcap)
    windows.rs       # enumerate + grab windows (xcap), window picker
    region.rs        # frozen-overlay region selector
  document.rs        # Document: base image, annotations, crop, view transform, undo/redo
  annotation.rs      # Annotation enum + shared props (color, stroke, etc.)
  tools.rs           # Tool enum + active tool state, current color/stroke
  render/
    mod.rs
    display.rs       # egui Painter rendering of base + annotations
    export.rs        # tiny-skia flattening -> Pixmap -> PNG bytes
  clipboard.rs       # copy image to clipboard (arboard)
  io.rs              # save to path (rfd dialog + image encode)
  ocr.rs             # Phase 4: Windows.Media.Ocr text extraction
  assets/            # embedded font(s), app icon, toolbar glyphs
```

---

## 6. Data Model

```rust
// One open screenshot = one tab.
struct Document {
    title: String,                 // e.g. "Screenshot 3" or source window title
    base: ImageBuffer,             // the captured pixels (RGBA8)
    annotations: Vec<Annotation>,  // ordered; later = on top
    selection: Option<usize>,      // index of selected annotation
    crop: Option<Rect>,            // non-destructive crop in image space (None = full)
    view: ViewTransform,           // zoom + pan for display only
    history: History,              // undo/redo stack
    dirty: bool,                   // unsaved changes
    saved_path: Option<PathBuf>,
}

struct Annotation {
    kind: AnnotationKind,
    color: Color32,
    stroke_width: f32,
    // geometry stored in image-space coordinates
}

enum AnnotationKind {
    Arrow   { from: Pos2, to: Pos2 },          // line w/ arrowhead ("marker")
    Highlight { rect: Rect },                  // semi-transparent fill (yellow default)
    Rect    { rect: Rect, filled: bool },
    Ellipse { rect: Rect, filled: bool },
    Text    { pos: Pos2, content: String, size: f32 },
    Freehand{ points: Vec<Pos2> },             // pen
}

enum Tool { Select, Arrow, Highlight, Rect, Ellipse, Text, Pen, Crop }

struct ViewTransform { zoom: f32, pan: Vec2 }  // image-space <-> screen-space mapping

struct History {           // simple snapshot-based undo for v1
    past: Vec<Snapshot>,   // capped depth (e.g. 50)
    future: Vec<Snapshot>,
}
```

- **Coordinates:** all geometry is in **image space**, so annotations stay locked to the
  image under zoom/pan and survive export at full resolution.
- **Undo/redo (v1):** snapshot the annotation list + crop on each committed edit. Simple
  and correct; optimize to a command pattern later if memory matters.

---

## 7. Capture Subsystem

Three capture modes, exposed in the toolbar (mirrors Snipping Tool):

### 7.1 Full screen
- Enumerate monitors via `xcap`. If multiple monitors, capture the one under the cursor
  (or offer a quick monitor picker). Produce one `Document`.

### 7.2 Window / app
- Enumerate top-level windows via `xcap` (title + thumbnail).
- Show a lightweight **window picker** (list with titles; optionally small thumbnails).
- Capture the chosen window's pixels into a `Document`.

### 7.3 Region (selected area)
**Approach: "freeze and select."**
1. Capture the full virtual desktop (all monitors) to an in-memory image first.
2. Open a **borderless, always-on-top, fullscreen overlay** window that displays this
   frozen capture with a dimmed scrim.
3. User drags a selection rectangle; show live dimensions + a clear "hole" over the
   selected area.
4. On release (or Enter), crop the selected rect from the frozen capture → new `Document`.
   `Esc` cancels.

This is robust, multi-monitor friendly, and avoids fragile transparent click-through
overlays.

### Capture-then-edit flow
- The main window may **hide itself** briefly before capture so it isn't in the shot
  (configurable; with a short delay).
- After capture, a new tab is created, the main window is shown/focused, and the canvas
  loads the new image.

---

## 8. Editing & Rendering

### Canvas (display)
- The base image is uploaded as an `egui::TextureHandle` (re-uploaded only when the base
  changes, e.g. after crop).
- Each frame: draw the texture, then draw annotations via `Painter`, then draw selection
  handles and any in-progress drag preview.
- **Zoom:** Ctrl+scroll / buttons; **Pan:** space-drag or middle-drag. Fit-to-window and
  100% presets.

### Interaction model
- **Tool selected → drag on canvas** creates the annotation (preview while dragging,
  committed on release; pushes an undo snapshot).
- **Select tool** → click to select, drag to move, handles to resize, `Delete` to remove.
- The active **color** and **stroke width** in the toolbar apply to new annotations and to
  the currently selected one (live recolor — key for clean AI-ready markup).
- **Text:** click to place, type inline; Enter/click-away commits.

### Export rendering (flatten)
- `render::export::flatten(doc) -> Pixmap`:
  1. Start from the base image (cropped to `doc.crop` if set).
  2. Replay annotations onto the `Pixmap` with `tiny-skia` (paths, fills, strokes;
     text via `ab_glyph`).
  3. Return the `Pixmap`, convertible to PNG bytes (`image`) or an `arboard` image.
- This single function feeds **both** copy and save, guaranteeing what you copy ==
  what you save == what you saw.

---

## 9. Annotation Tools (detail)

| Tool | Gesture | Notes |
|---|---|---|
| **Arrow / Marker** | drag from→to | Line with arrowhead; the default "point at this" tool. |
| **Highlight** | drag rect | Semi-transparent fill (default translucent yellow); good for emphasis without hiding content. |
| **Rectangle** | drag rect | Outline by default; toggle filled. Filled opaque = redaction. |
| **Ellipse** | drag rect (bounding) | Outline/filled. |
| **Text** | click + type | Color + size from toolbar. |
| **Pen / Freehand** | drag | Smooth polyline. |
| **Crop** | drag rect → apply | Sets `doc.crop`; non-destructive, re-adjustable. |

**Shared controls:** color picker (swatches + custom), stroke width slider, fill toggle
where relevant. Sensible defaults: red arrows, yellow highlight, ~3px stroke.

---

## 10. Crop

- Crop tool sets a non-destructive `crop: Rect` in image space.
- Display and export both honor the crop. Annotations outside the crop are clipped on
  export.
- Re-selecting the crop tool lets you re-adjust; "Reset crop" restores full image.
  (Non-destructive keeps undo trivial and avoids resampling the base.)

---

## 11. Clipboard & Save

- **Copy full image** (`Ctrl+C`): `flatten()` → RGBA → `arboard::Clipboard::set_image`.
  This is the primary "send to AI" action.
- **Save to path** (`Ctrl+S` / `Ctrl+Shift+S` for Save As): `rfd` save dialog →
  encode PNG (default) or JPEG via `image`. Remember last directory.
- **Copy text** (Phase 4, after OCR): copies extracted text instead of the image.

---

## 12. Tabs / Multi-Document

- `App` holds `Vec<Document>` + `active: usize`.
- A tab strip above the canvas: tab per screenshot, title, close button, unsaved `•` mark.
- New capture → new tab, auto-focused. Closing a dirty tab prompts to save/discard.
- `Ctrl+Tab` / `Ctrl+W` to switch / close. (A simple custom tab bar; `egui_dock` only if
  we later want split/drag-out panes — not needed for v1.)

---

## 13. OCR — Text Extraction (Phase 4)

- **Engine:** `Windows.Media.Ocr.OcrEngine` via the `windows` crate. Pros: built into the
  OS, no bundled binaries, multi-language, keeps the exe portable.
- **Flow:** "Extract text" button → run OCR on the flattened (or base) image → show the
  recognized text in a side panel → **Copy text** button.
- **Fallback / future:** feature-flag a `tesseract`/`rusty-tesseract` path for non-Windows
  builds. A local-LLM option is explicitly out of scope for v1 (binary size + complexity);
  revisit only if structured extraction is needed.

This is the headline "AI context" feature beyond images — deferred to Phase 4 because the
core capture/annotate/copy loop delivers value first.

---

## 14. UI Layout

```
┌──────────────────────────────────────────────────────────────┐
│  [ + New ▾ ]  [Full] [Window] [Region]   |  ⟲ ⟳   |   ⧉ Copy  💾 Save │  ← toolbar
├──────────────────────────────────────────────────────────────┤
│  [Screenshot 1 •] [Screenshot 2] [ + ]                          │  ← tabs
├──────────────────────────────────────────────────────────────┤
│  Tools: ▣ Select  ↘ Arrow  ▥ Highlight  ▭ Rect  ◯ Ellipse      │
│         T Text  ✎ Pen  ⌗ Crop     Color ■  Width ──●──          │  ← tool bar
├──────────────────────────────────────────────────────────────┤
│                                                                │
│                      [ image canvas ]                          │
│                                                                │
├──────────────────────────────────────────────────────────────┤
│  1920×1080   ·   100%   ·   Arrow tool                          │  ← status bar
└──────────────────────────────────────────────────────────────┘
```

- Dark theme by default (egui dark visuals), rounded controls, generous spacing.
- Icon set: a small embedded glyph font or vector glyphs drawn with egui shapes.

---

## 15. Keyboard Shortcuts

| Key | Action |
|---|---|
| `Ctrl+N` | New capture (last mode) |
| `Ctrl+C` | Copy flattened image |
| `Ctrl+S` / `Ctrl+Shift+S` | Save / Save As |
| `Ctrl+Z` / `Ctrl+Y` | Undo / Redo |
| `Ctrl+W` / `Ctrl+Tab` | Close / switch tab |
| `Delete` | Delete selected annotation |
| `Esc` | Cancel region select / deselect / cancel text edit |
| `V A H R E T P` | Select / Arrow / Highlight / Rect / Ellipse / Text / Pen tools |
| `+ / -` , `Ctrl+0` | Zoom in/out, fit |

---

## 16. Build & Packaging

- `cargo build --release` → `target/release/snapedit.exe`.
- `.cargo/config.toml`:
  ```toml
  [target.x86_64-pc-windows-msvc]
  rustflags = ["-C", "target-feature=+crt-static"]
  ```
- `main.rs`: `#![windows_subsystem = "windows"]` (no console).
- App icon embedded via `embed-resource`/`winres` build script.
- Release profile tuned for size:
  ```toml
  [profile.release]
  opt-level = "z"     # or "s"
  lto = true
  codegen-units = 1
  strip = true
  panic = "abort"
  ```
- Optional final size pass with `upx` (kept optional; not required for "portable").

---

## 17. Dependencies (initial `Cargo.toml` sketch)

```toml
[package]
name = "snapedit"
version = "0.1.0"
edition = "2021"

[dependencies]
eframe   = { version = "0.x", default-features = false, features = ["glow"] }
egui      = "0.x"
egui_extras = { version = "0.x", features = ["image"] }   # texture helpers
xcap      = "0.x"          # monitor + window capture
image     = { version = "0.25", default-features = false, features = ["png", "jpeg"] }
tiny-skia = "0.11"         # export compositing
ab_glyph  = "0.2"          # text rasterization for export
arboard   = "3"            # clipboard image
rfd       = "0.x"          # native file dialogs

[target.'cfg(windows)'.dependencies]
windows   = { version = "0.x", features = [/* Media_Ocr, Graphics_Imaging, ... */] }  # Phase 4 OCR

[build-dependencies]
embed-resource = "2"       # app icon
```
(Exact versions pinned during scaffolding; egui/eframe versions must match.)

---

## 18. Milestones / Phases

### Phase 0 — Scaffolding
- [ ] `cargo init`, dependencies, `.cargo/config.toml`, release profile, no-console.
- [ ] eframe window opens with dark theme, empty canvas, toolbar/tab stubs.
- [ ] Embedded font + app icon.

### Phase 1 — Capture (MVP core)
- [ ] Full-screen capture → new tab shows the image on the canvas.
- [ ] Window capture with a window picker.
- [ ] Region capture via frozen overlay selector.
- [ ] Zoom/pan/fit on the canvas.

### Phase 2 — Annotate + export
- [ ] Arrow, Rect, Ellipse, Highlight, Text, Pen tools (display via egui).
- [ ] Select / move / resize / delete; color + stroke width controls (incl. recolor).
- [ ] Undo/redo.
- [ ] tiny-skia export renderer (flatten).
- [ ] **Copy to clipboard** and **Save as PNG/JPEG**.

> End of Phase 2 = a genuinely useful tool: capture → annotate → copy/paste into an AI.

### Phase 3 — Polish & tabs
- [ ] Multi-tab document management, dirty/close prompts.
- [ ] Crop tool (non-destructive).
- [ ] Keyboard shortcuts, status bar, settings (capture delay, default save dir).
- [ ] Size-optimized portable release build verified on a clean machine.

### Phase 4 — AI features
- [ ] OCR via Windows.Media.Ocr; text side panel + Copy text.
- [ ] Optional global hotkey to start a snip.
- [ ] (Stretch) "Copy as Markdown" / quick-share helpers for agent workflows.

---

## 19. Risks & Open Questions

- **Display vs export parity:** two renderers must match visually. Mitigation: shared
  annotation data + a small golden-image test comparing egui-rendered vs tiny-skia-rendered
  output for each primitive.
- **Text rendering quality** with `ab_glyph` (no shaping). Fine for short Latin labels;
  revisit `cosmic-text` if richer text/RTL is ever needed.
- **Multi-monitor / DPI** for region overlay (mixed-DPI setups). Needs careful virtual-desktop
  bounds + scale-factor handling; test on multi-monitor.
- **`xcap` window capture** edge cases (occluded/minimized/elevated windows). Validate; fall
  back to full-screen + region if a window can't be grabbed.
- **OCR availability:** language packs vary per machine; handle "engine unavailable" gracefully.
- **Window self-capture:** ensure SnapEdit hides before fullscreen/region capture.

## 20. Future Ideas (post-v1)
- Scrolling / stitched capture.
- Numbered step badges (1,2,3…) for tutorials.
- Blur/pixelate redaction tool.
- Direct "send to Claude/agent" integration.
- Cross-platform builds (Linux/macOS) — stack is largely portable already.

---

## Decisions locked for v1
- **GUI:** egui/eframe + glow. **Capture:** xcap. **Export:** tiny-skia. **Clipboard:** arboard.
- **Format:** PNG default (JPEG optional). **Platform:** Windows-first, portable single exe.
- **OCR:** Windows.Media.Ocr, deferred to Phase 4. **No sign-in, no installer.**
