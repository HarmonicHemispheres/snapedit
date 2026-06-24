//! Screen capture via `xcap` (monitors + windows).
//!
//! All capture functions return raw straight-RGBA8 bytes plus dimensions, so the
//! rest of the app never depends on `xcap`'s re-exported `image` types.

use xcap::{Monitor, Window};

/// A captured image as raw RGBA8 (straight alpha).
pub struct Captured {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Lightweight window metadata for the picker (no pixels captured yet).
pub struct WindowInfo {
    pub id: u32,
    pub title: String,
    pub app: String,
}

/// Lightweight monitor metadata for the full-screen display picker.
pub struct MonitorInfo {
    pub id: u32,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
}

/// All monitors stitched into a single virtual-desktop image, plus the physical
/// top-left origin of the virtual desktop and the primary scale factor. The origin
/// and scale are used to place the region-select overlay across every monitor.
pub struct VirtualDesktop {
    pub captured: Captured,
    pub origin_x: i32,
    pub origin_y: i32,
    pub scale_factor: f32,
}

fn primary_monitor() -> Result<Monitor, String> {
    let monitors = Monitor::all().map_err(|e| format!("enumerating monitors: {e}"))?;
    monitors
        .iter()
        .find(|m| m.is_primary().unwrap_or(false))
        .or_else(|| monitors.first())
        .cloned()
        .ok_or_else(|| "no monitors found".to_string())
}

fn to_captured(img: xcap::image::RgbaImage) -> Captured {
    Captured {
        width: img.width(),
        height: img.height(),
        rgba: img.into_raw(),
    }
}

/// Capture the full primary monitor (the keyboard/quick-capture default).
pub fn capture_full() -> Result<Captured, String> {
    let mon = primary_monitor()?;
    let img = mon
        .capture_image()
        .map_err(|e| format!("capturing monitor: {e}"))?;
    Ok(to_captured(img))
}

/// List the connected monitors for the full-screen display picker.
pub fn list_monitors() -> Vec<MonitorInfo> {
    let Ok(monitors) = Monitor::all() else {
        return Vec::new();
    };
    monitors
        .iter()
        .map(|m| MonitorInfo {
            id: m.id().unwrap_or(0),
            name: m
                .friendly_name()
                .ok()
                .filter(|s| !s.trim().is_empty())
                .or_else(|| m.name().ok())
                .unwrap_or_default(),
            width: m.width().unwrap_or(0),
            height: m.height().unwrap_or(0),
            is_primary: m.is_primary().unwrap_or(false),
        })
        .collect()
}

/// Capture a single monitor by id (re-enumerated at capture time).
pub fn capture_monitor(id: u32) -> Result<Captured, String> {
    let monitors = Monitor::all().map_err(|e| format!("enumerating monitors: {e}"))?;
    let mon = monitors
        .into_iter()
        .find(|m| m.id().map(|mid| mid == id).unwrap_or(false))
        .ok_or_else(|| "monitor no longer available".to_string())?;
    let img = mon
        .capture_image()
        .map_err(|e| format!("capturing monitor: {e}"))?;
    Ok(to_captured(img))
}

/// Copy `img` (RGBA8) into the `vw`×`vh` destination buffer at integer offset
/// (`ox`, `oy`), clipping anything that falls outside the destination bounds.
fn blit(buf: &mut [u8], vw: u32, vh: u32, img: &xcap::image::RgbaImage, ox: i32, oy: i32) {
    let (iw, ih) = (img.width() as i32, img.height() as i32);
    let src = img.as_raw();
    let dx0 = ox.max(0);
    let dx1 = (ox + iw).min(vw as i32);
    if dx1 <= dx0 {
        return;
    }
    let row_bytes = ((dx1 - dx0) as usize) * 4;
    for row in 0..ih {
        let dy = oy + row;
        if dy < 0 || dy >= vh as i32 {
            continue;
        }
        let sx0 = (dx0 - ox) as usize;
        let s = ((row as usize) * iw as usize + sx0) * 4;
        let d = ((dy as usize) * vw as usize + dx0 as usize) * 4;
        buf[d..d + row_bytes].copy_from_slice(&src[s..s + row_bytes]);
    }
}

/// Capture every monitor and stitch them into a single virtual-desktop image.
/// Gaps in a non-rectangular arrangement are left opaque black. A monitor that
/// fails to capture is skipped (left black) rather than failing the whole grab.
pub fn capture_all_monitors() -> Result<VirtualDesktop, String> {
    let monitors = Monitor::all().map_err(|e| format!("enumerating monitors: {e}"))?;
    if monitors.is_empty() {
        return Err("no monitors found".to_string());
    }

    // Monitor geometry in physical pixels (xcap reports x/y/width/height from the
    // same display mode, so they share one consistent virtual-desktop pixel space).
    let mut rects = Vec::with_capacity(monitors.len());
    for m in &monitors {
        let x = m.x().map_err(|e| format!("monitor position: {e}"))?;
        let y = m.y().map_err(|e| format!("monitor position: {e}"))?;
        let w = m.width().map_err(|e| format!("monitor size: {e}"))?;
        let h = m.height().map_err(|e| format!("monitor size: {e}"))?;
        rects.push((x, y, w, h));
    }
    let min_x = rects.iter().map(|r| r.0).min().unwrap();
    let min_y = rects.iter().map(|r| r.1).min().unwrap();
    let max_x = rects.iter().map(|r| r.0 + r.2 as i32).max().unwrap();
    let max_y = rects.iter().map(|r| r.1 + r.3 as i32).max().unwrap();
    let vw = (max_x - min_x).max(1) as u32;
    let vh = (max_y - min_y).max(1) as u32;

    // Opaque-black canvas; uncovered gaps stay black behind the dimming scrim.
    let mut buf = vec![0u8; (vw as usize) * (vh as usize) * 4];
    for px in buf.chunks_exact_mut(4) {
        px[3] = 255;
    }

    let mut captured_any = false;
    for (m, &(mx, my, _, _)) in monitors.iter().zip(rects.iter()) {
        match m.capture_image() {
            Ok(img) => {
                blit(&mut buf, vw, vh, &img, mx - min_x, my - min_y);
                captured_any = true;
            }
            Err(_) => continue,
        }
    }
    if !captured_any {
        return Err("failed to capture any monitor".to_string());
    }

    let scale_factor = monitors
        .iter()
        .find(|m| m.is_primary().unwrap_or(false))
        .or_else(|| monitors.first())
        .and_then(|m| m.scale_factor().ok())
        .filter(|s| s.is_finite() && *s > 0.0)
        .unwrap_or(1.0);

    Ok(VirtualDesktop {
        captured: Captured {
            width: vw,
            height: vh,
            rgba: buf,
        },
        origin_x: min_x,
        origin_y: min_y,
        scale_factor,
    })
}

/// Capture the whole virtual desktop for the region overlay (freeze-and-select
/// across every monitor).
pub fn capture_for_region() -> Result<VirtualDesktop, String> {
    capture_all_monitors()
}

/// List candidate windows for the picker (visible, titled, not minimized).
pub fn list_windows() -> Vec<WindowInfo> {
    let Ok(windows) = Window::all() else {
        return Vec::new();
    };
    windows
        .into_iter()
        .filter_map(|w| {
            if w.is_minimized().unwrap_or(false) {
                return None;
            }
            let title = w.title().unwrap_or_default();
            let app = w.app_name().unwrap_or_default();
            // Skip our own window and untitled/zero-size shells.
            if title.trim().is_empty() && app.trim().is_empty() {
                return None;
            }
            if app.eq_ignore_ascii_case("snapedit") || title.eq_ignore_ascii_case("SnapEdit") {
                return None;
            }
            if w.width().unwrap_or(0) == 0 || w.height().unwrap_or(0) == 0 {
                return None;
            }
            Some(WindowInfo {
                id: w.id().unwrap_or(0),
                title,
                app,
            })
        })
        .collect()
}

/// Capture a specific window by id (re-enumerated at capture time).
pub fn capture_window(id: u32) -> Result<Captured, String> {
    let windows = Window::all().map_err(|e| format!("enumerating windows: {e}"))?;
    let win = windows
        .into_iter()
        .find(|w| w.id().map(|wid| wid == id).unwrap_or(false))
        .ok_or_else(|| "window no longer available".to_string())?;
    let img = win
        .capture_image()
        .map_err(|e| format!("capturing window: {e}"))?;
    Ok(to_captured(img))
}
