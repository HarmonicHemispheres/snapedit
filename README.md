# SnapEdit

A lightweight, modern screenshot + annotation tool for Windows, written in Rust.
Capture a screenshot, mark it up, and copy or save it — built for preparing images
as context for LLMs and AI agents.

See [spec.md](spec.md) for the full design.

## Features (implemented)

- **Capture**: full screen (with a display picker when multiple monitors are
  connected — pick one or grab **All displays**), a specific window (picker), or a
  drag-selected region. The region overlay spans **every monitor**, so a snip can
  cross displays.
- **Annotate**: arrow/marker, pen-style highlighter, rectangle, ellipse, text, and
  freehand pen. Hold **Shift** while drawing with the pen or highlighter for a straight line.
- **Text**: type directly on the image (WYSIWYG); double-click any text with the Select
  tool to re-edit it; optional background fill.
- **Edit**: select / move / resize / delete objects; change color, stroke width, and
  fill; recolor selected objects from the palette. Each tool's settings live in a single
  Style popover.
- **Crop**: non-destructive, interactive box with drag handles, an Apply / Cancel /
  Remove bar, and a "cropped" indicator in the status bar. Re-adjust or remove any time
  (undoable).
- **Output**: copy the flattened image to the clipboard, or save as PNG / JPEG.
- **Tabs**: multiple screenshots open at once.
- **Undo / redo**, zoom & pan.
- No sign-in, no installer — a single portable `.exe`.

## Build

Requires a recent stable Rust toolchain (developed against 1.96).

```sh
# Debug (fast, shows a console for logs)
cargo run

# Portable release build -> target/release/snapedit.exe
cargo build --release

# Try the editor without taking a screenshot (loads a synthetic image)
cargo run -- --demo
```

The release profile statically links the MSVC CRT (see [.cargo/config.toml](.cargo/config.toml)),
so `snapedit.exe` runs on a clean Windows machine with no Visual C++ redistributable.

## Keyboard shortcuts

| Key | Action |
|---|---|
| `Ctrl+N` | New full-screen capture (display picker if multi-monitor) |
| `Ctrl+C` | Copy flattened image |
| `Ctrl+S` | Save (PNG/JPEG) |
| `Ctrl+Z` / `Ctrl+Y` (or `Ctrl+Shift+Z`) | Undo / Redo |
| `Ctrl+W` | Close current tab |
| `Delete` / `Backspace` | Delete selected object |
| `Esc` | Cancel region select / deselect |
| `V A H R E T P C` | Select / Arrow / Highlight / Rect / Ellipse / Text / Pen / Crop |
| `Shift` (while drawing) | Constrain pen/highlighter to a straight line |
| double-click text | Re-edit it (Select tool) |
| scroll / pinch | Zoom · middle-drag or Space+drag to pan |

## Tech stack

`egui`/`eframe` (glow) UI · `xcap` capture · `tiny-skia` + `ab_glyph` export rendering ·
`arboard` clipboard · `rfd` file dialogs · `image` PNG/JPEG.

## Status & known limitations (v1)

- Multi-monitor capture assumes all displays share one DPI; mixed-DPI setups may
  show a slightly soft overlay, but the captured pixels stay accurate.
- **Resize** handles work for arrows (endpoints) and rectangles/ellipses (corners);
  text, pen, and highlighter strokes are move-only.
- Display text uses egui's bundled font; exported text uses a system font (Segoe UI/Arial),
  so on-screen and exported text may differ slightly.
- **OCR / text extraction** is planned for Phase 4 (Windows.Media.Ocr).
