//! Canonical export renderer: flatten a `Document` (base image + annotations) into
//! a single opaque RGBA8 buffer using `tiny-skia`. This buffer feeds both
//! "copy to clipboard" and "save to file" so display == copy == save.

use crate::document::{Annotation, Document, Kind, norm};
use ab_glyph::{Font, FontVec, PxScale, ScaleFont, point};
use egui::Color32;
use tiny_skia::{
    FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Rect as SkRect, Stroke, Transform,
};

/// A flattened, fully-opaque image. `rgba` is straight (un-premultiplied) RGBA8;
/// because the base is forced opaque and we only composite on top, the final
/// alpha is 255 everywhere, so tiny-skia's premultiplied output equals straight.
pub struct Flattened {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Highlight fill is always translucent regardless of the picked color's alpha.
fn highlight_alpha(c: Color32) -> [u8; 4] {
    [c.r(), c.g(), c.b(), 96]
}

fn solid_paint(c: Color32) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(c.r(), c.g(), c.b(), c.a());
    p.anti_alias = true;
    p
}

fn round_stroke(width: f32) -> Stroke {
    Stroke {
        width: width.max(1.0),
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Default::default()
    }
}

pub fn flatten(doc: &Document) -> Result<Flattened, String> {
    let (w, h) = (doc.size[0] as u32, doc.size[1] as u32);
    let mut pixmap = Pixmap::new(w, h).ok_or("failed to allocate export pixmap")?;

    // 1. Base image: copy RGBA, forcing alpha opaque (premultiplied == straight at a=255).
    {
        let data = pixmap.data_mut();
        let n = (w as usize * h as usize) * 4;
        let src = &doc.base_rgba;
        for i in (0..n).step_by(4) {
            if i + 3 < src.len() {
                data[i] = src[i];
                data[i + 1] = src[i + 1];
                data[i + 2] = src[i + 2];
                data[i + 3] = 255;
            }
        }
    }

    // 2. Replay annotations (in full-image coordinates).
    let font = load_font();
    for ann in &doc.annotations {
        draw_annotation(&mut pixmap, ann, font.as_ref());
    }

    // 3. Apply non-destructive crop, if any, by extracting the sub-rectangle.
    if let Some(crop) = doc.crop {
        let x0 = (crop.min.x.floor().max(0.0) as u32).min(w);
        let y0 = (crop.min.y.floor().max(0.0) as u32).min(h);
        let x1 = (crop.max.x.ceil().max(0.0) as u32).clamp(x0, w);
        let y1 = (crop.max.y.ceil().max(0.0) as u32).clamp(y0, h);
        let (cw, ch) = (x1 - x0, y1 - y0);
        if cw > 0 && ch > 0 {
            let src = pixmap.data();
            let mut out = vec![0u8; (cw * ch * 4) as usize];
            for row in 0..ch {
                let s = (((y0 + row) * w + x0) * 4) as usize;
                let d = (row * cw * 4) as usize;
                let len = (cw * 4) as usize;
                out[d..d + len].copy_from_slice(&src[s..s + len]);
            }
            return Ok(Flattened {
                width: cw,
                height: ch,
                rgba: out,
            });
        }
    }

    let rgba = pixmap.data().to_vec();
    Ok(Flattened {
        width: w,
        height: h,
        rgba,
    })
}

fn draw_annotation(pixmap: &mut Pixmap, ann: &Annotation, font: Option<&FontVec>) {
    match &ann.kind {
        Kind::Arrow { a, b } => {
            let mut pb = PathBuilder::new();
            pb.move_to(a.x, a.y);
            pb.line_to(b.x, b.y);
            // Arrowhead.
            let dir = *b - *a;
            let len = dir.length().max(1.0);
            let ux = dir.x / len;
            let uy = dir.y / len;
            let head = (ann.width * 4.0).max(12.0);
            let (cos, sin) = (0.92_f32, 0.39_f32); // ~23 degrees
            // rotate -unit by +/- angle
            for s in [1.0_f32, -1.0] {
                let rx = -ux * cos + (-uy) * (s * sin);
                let ry = (-ux) * (-(s * sin)) + (-uy) * cos;
                pb.move_to(b.x, b.y);
                pb.line_to(b.x + rx * head, b.y + ry * head);
            }
            if let Some(path) = pb.finish() {
                pixmap.stroke_path(
                    &path,
                    &solid_paint(ann.color),
                    &round_stroke(ann.width),
                    Transform::identity(),
                    None,
                );
            }
        }
        Kind::Pen { points } => {
            if points.len() >= 2 {
                let mut pb = PathBuilder::new();
                pb.move_to(points[0].x, points[0].y);
                for p in &points[1..] {
                    pb.line_to(p.x, p.y);
                }
                if let Some(path) = pb.finish() {
                    pixmap.stroke_path(
                        &path,
                        &solid_paint(ann.color),
                        &round_stroke(ann.width),
                        Transform::identity(),
                        None,
                    );
                }
            }
        }
        Kind::Highlight { points } => {
            if points.len() >= 2 {
                let mut pb = PathBuilder::new();
                pb.move_to(points[0].x, points[0].y);
                for p in &points[1..] {
                    pb.line_to(p.x, p.y);
                }
                if let Some(path) = pb.finish() {
                    let [rr, gg, bb, aa] = highlight_alpha(ann.color);
                    let mut paint = Paint::default();
                    paint.set_color_rgba8(rr, gg, bb, aa);
                    paint.anti_alias = true;
                    pixmap.stroke_path(
                        &path,
                        &paint,
                        &round_stroke(ann.width),
                        Transform::identity(),
                        None,
                    );
                }
            }
        }
        Kind::Rect { rect } => {
            let r = norm(*rect);
            if let Some(sr) = SkRect::from_ltrb(r.min.x, r.min.y, r.max.x, r.max.y) {
                let path = PathBuilder::from_rect(sr);
                if ann.fill {
                    pixmap.fill_path(
                        &path,
                        &solid_paint(ann.color),
                        FillRule::Winding,
                        Transform::identity(),
                        None,
                    );
                } else {
                    pixmap.stroke_path(
                        &path,
                        &solid_paint(ann.color),
                        &round_stroke(ann.width),
                        Transform::identity(),
                        None,
                    );
                }
            }
        }
        Kind::Ellipse { rect } => {
            let r = norm(*rect);
            if let Some(sr) = SkRect::from_ltrb(r.min.x, r.min.y, r.max.x, r.max.y) {
                if let Some(path) = PathBuilder::from_oval(sr) {
                    if ann.fill {
                        pixmap.fill_path(
                            &path,
                            &solid_paint(ann.color),
                            FillRule::Winding,
                            Transform::identity(),
                            None,
                        );
                    } else {
                        pixmap.stroke_path(
                            &path,
                            &solid_paint(ann.color),
                            &round_stroke(ann.width),
                            Transform::identity(),
                            None,
                        );
                    }
                }
            }
        }
        Kind::Text { pos, text } => {
            if let Some(font) = font {
                draw_text(pixmap, font, text, pos.x, pos.y, ann.font_size, ann.color, ann.bg);
            }
        }
    }
}

/// Rasterize `text` with `ab_glyph` and alpha-blend it onto the (opaque) pixmap.
fn draw_text(
    pixmap: &mut Pixmap,
    font: &FontVec,
    text: &str,
    x: f32,
    y: f32,
    size: f32,
    color: Color32,
    bg: Option<Color32>,
) {
    let scaled = font.as_scaled(PxScale::from(size));
    let ascent = scaled.ascent();
    let line_h = size * 1.3;

    // Optional background fill behind the text block.
    if let Some(bg) = bg {
        let mut max_w = 0.0f32;
        let mut nlines = 0usize;
        for line in text.split('\n') {
            nlines += 1;
            let mut w = 0.0;
            let mut prev = None;
            for ch in line.chars() {
                let gid = font.glyph_id(ch);
                if let Some(p) = prev {
                    w += scaled.kern(p, gid);
                }
                w += scaled.h_advance(gid);
                prev = Some(gid);
            }
            max_w = max_w.max(w);
        }
        let pad = size * 0.18;
        if let Some(r) = SkRect::from_xywh(
            x - pad,
            y - pad,
            max_w + pad * 2.0,
            line_h * nlines.max(1) as f32 + pad * 2.0,
        ) {
            let mut p = Paint::default();
            p.set_color_rgba8(bg.r(), bg.g(), bg.b(), bg.a());
            p.anti_alias = true;
            pixmap.fill_rect(r, &p, Transform::identity(), None);
        }
    }

    let (pw, ph) = (pixmap.width() as i32, pixmap.height() as i32);
    let data = pixmap.data_mut();

    for (line_idx, line) in text.split('\n').enumerate() {
        let baseline_y = y + ascent + line_idx as f32 * line_h;
        let mut caret = x;
        let mut prev = None;
        for ch in line.chars() {
            let gid = font.glyph_id(ch);
            if let Some(p) = prev {
                caret += scaled.kern(p, gid);
            }
            let glyph = gid.with_scale_and_position(size, point(caret, baseline_y));
            if let Some(outline) = font.outline_glyph(glyph) {
                let bb = outline.px_bounds();
                outline.draw(|gx, gy, cov| {
                    let px = bb.min.x as i32 + gx as i32;
                    let py = bb.min.y as i32 + gy as i32;
                    if px < 0 || py < 0 || px >= pw || py >= ph {
                        return;
                    }
                    let idx = ((py * pw + px) as usize) * 4;
                    let a = cov.clamp(0.0, 1.0);
                    blend(&mut data[idx..idx + 4], color, a);
                });
            }
            caret += scaled.h_advance(gid);
            prev = Some(gid);
        }
    }
}

/// Alpha-blend straight `color` at coverage `a` over an opaque destination pixel.
fn blend(dst: &mut [u8], color: Color32, a: f32) {
    let inv = 1.0 - a;
    dst[0] = (color.r() as f32 * a + dst[0] as f32 * inv).round() as u8;
    dst[1] = (color.g() as f32 * a + dst[1] as f32 * inv).round() as u8;
    dst[2] = (color.b() as f32 * a + dst[2] as f32 * inv).round() as u8;
    dst[3] = 255;
}

/// Load a system font for export text. Display uses egui's bundled font; this only
/// affects the flattened output. Tries common Windows fonts; returns None if none load.
fn load_font() -> Option<FontVec> {
    const CANDIDATES: &[&str] = &[
        "C:\\Windows\\Fonts\\segoeui.ttf",
        "C:\\Windows\\Fonts\\arial.ttf",
        "C:\\Windows\\Fonts\\tahoma.ttf",
        "C:\\Windows\\Fonts\\verdana.ttf",
    ];
    for path in CANDIDATES {
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(font) = FontVec::try_from_vec(bytes) {
                return Some(font);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Annotation, Document, Kind};
    use crate::output;
    use egui::{Rect, pos2};

    fn sample_doc() -> Document {
        let (w, h) = (240u32, 120u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for px in rgba.chunks_mut(4) {
            px.copy_from_slice(&[30, 60, 90, 255]);
        }
        let mut doc = Document::from_rgba("test", w, h, rgba);
        let mk = |kind, color, fill| Annotation {
            kind,
            color,
            width: 4.0,
            fill,
            font_size: 22.0,
            bg: None,
        };
        doc.annotations.push(mk(
            Kind::Arrow {
                a: pos2(10.0, 10.0),
                b: pos2(100.0, 80.0),
            },
            Color32::RED,
            false,
        ));
        doc.annotations.push(mk(
            Kind::Rect {
                rect: Rect::from_min_max(pos2(120.0, 20.0), pos2(200.0, 90.0)),
            },
            Color32::GREEN,
            false,
        ));
        doc.annotations.push(mk(
            Kind::Ellipse {
                rect: Rect::from_min_max(pos2(40.0, 40.0), pos2(110.0, 100.0)),
            },
            Color32::YELLOW,
            true,
        ));
        doc.annotations.push(mk(
            Kind::Highlight {
                points: vec![pos2(10.0, 105.0), pos2(80.0, 105.0), pos2(150.0, 108.0)],
            },
            Color32::from_rgb(255, 230, 0),
            false,
        ));
        doc.annotations.push(mk(
            Kind::Text {
                pos: pos2(12.0, 12.0),
                text: "Hello AI".into(),
            },
            Color32::WHITE,
            false,
        ));
        doc.annotations.push(mk(
            Kind::Pen {
                points: vec![pos2(150.0, 30.0), pos2(160.0, 42.0), pos2(176.0, 34.0)],
            },
            Color32::from_rgb(0, 200, 255),
            false,
        ));
        doc
    }

    #[test]
    fn flatten_produces_opaque_annotated_image() {
        let doc = sample_doc();
        let f = flatten(&doc).expect("flatten failed");
        assert_eq!((f.width, f.height), (240, 120));
        assert_eq!(f.rgba.len(), 240 * 120 * 4);
        assert!(f.rgba.chunks(4).all(|p| p[3] == 255), "output must be opaque");
        let changed = f
            .rgba
            .chunks(4)
            .any(|p| p[..3] != [30, 60, 90]);
        assert!(changed, "annotations should have modified some pixels");
    }

    // Renders a rich demo to TEMP/snapedit_demo.png for manual visual inspection.
    #[test]
    #[ignore]
    fn render_demo_png() {
        let mut doc = sample_doc();
        // A highlighter stroke (now a translucent pen-style line).
        doc.annotations.push(Annotation {
            kind: Kind::Highlight {
                points: vec![pos2(20.0, 60.0), pos2(90.0, 58.0), pos2(160.0, 62.0)],
            },
            color: Color32::from_rgb(255, 220, 0),
            width: 16.0,
            fill: false,
            font_size: 22.0,
            bg: None,
        });
        // Text with a background.
        doc.annotations.push(Annotation {
            kind: Kind::Text {
                pos: pos2(20.0, 78.0),
                text: "label".into(),
            },
            color: Color32::WHITE,
            width: 1.0,
            fill: false,
            font_size: 20.0,
            bg: Some(Color32::from_rgba_unmultiplied(0, 0, 0, 180)),
        });
        let f = flatten(&doc).unwrap();
        let path = std::env::temp_dir().join("snapedit_demo.png");
        output::write_image(&f, &path).unwrap();
        eprintln!("wrote {}", path.display());
    }

    #[test]
    fn crop_changes_output_dimensions() {
        let mut doc = sample_doc();
        doc.crop = Some(Rect::from_min_max(pos2(20.0, 10.0), pos2(120.0, 70.0)));
        let f = flatten(&doc).expect("flatten failed");
        assert_eq!((f.width, f.height), (100, 60));
        assert_eq!(f.rgba.len(), 100 * 60 * 4);
    }

    #[test]
    fn write_and_reread_png_roundtrips() {
        let f = flatten(&sample_doc()).unwrap();
        let path = std::env::temp_dir().join("snapedit_export_test.png");
        output::write_image(&f, &path).expect("write png failed");
        let img = image::open(&path).expect("could not reopen written png");
        assert_eq!((img.width(), img.height()), (240, 120));
        let _ = std::fs::remove_file(&path);
    }
}
