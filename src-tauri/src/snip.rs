// #34: built-in region screenshot. `snip_start` grabs the whole virtual screen first (so the overlay
// never ends up in the picture), then opens one transparent, borderless, topmost overlay per monitor
// (one per monitor because a single window spanning mixed-DPI monitors gets only one scale factor).
// The overlay (src/snip/SnipOverlay.tsx) reports the dragged rect in its own CSS pixels plus its
// devicePixelRatio; `snip_finish` maps it to physical pixels of the grab, writes
// `<Pictures>\Screenshots\Screenshot YYYY-MM-DD HHMMSS.png` (Windows' own naming) and puts that path
// on the clipboard as plain text. Esc / right click → `snip_cancel`.
#[cfg(any(windows, test))]
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::sync::Mutex;

use tauri::{AppHandle, WebviewWindow};
#[cfg(windows)]
use tauri::{Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder};

#[cfg(not(windows))]
pub(crate) const UNSUPPORTED: &str = "ekran alıntısı yalnız Windows'ta";

#[cfg(windows)]
const LABEL_PREFIX: &str = "snip-";

/// The grab taken at `snip_start`: top-down BGRA rows of the whole virtual screen.
#[cfg(any(windows, test))]
struct Grab {
    /// Virtual-screen position of pixel (0,0); negative when a monitor sits left of/above the primary.
    origin: (i32, i32),
    width: u32,
    height: u32,
    bgra: Vec<u8>,
}

#[cfg(windows)]
static GRAB: Mutex<Option<Grab>> = Mutex::new(None);

/// Windows' own screenshot file name: `Screenshot 2026-10-02 143015.png`.
#[cfg(any(windows, test))]
fn file_name(t: (u16, u16, u16, u16, u16, u16)) -> String {
    let (y, mo, d, h, mi, s) = t;
    format!("Screenshot {y:04}-{mo:02}-{d:02} {h:02}{mi:02}{s:02}.png")
}

/// `<Pictures>\Screenshots\<name>`; a second shot in the same second gets ` (2)`, ` (3)`, …
#[cfg(any(windows, test))]
fn target_path(pictures: &Path, name: &str, exists: impl Fn(&Path) -> bool) -> PathBuf {
    let dir = pictures.join("Screenshots");
    let first = dir.join(name);
    if !exists(&first) {
        return first;
    }
    let stem = name.trim_end_matches(".png");
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}).png")))
        .find(|p| !exists(p))
        .expect("unbounded")
}

/// Overlay CSS rect → pixel rect inside the grab buffer, clamped to it. `scale` is the overlay's
/// CSS→physical ratio (`css_scale`), `win_pos` its physical (virtual-screen) position. None for an
/// empty rect.
#[cfg(any(windows, test))]
fn to_pixels(
    css: (f64, f64, f64, f64),
    scale: f64,
    win_pos: (i32, i32),
    origin: (i32, i32),
    buf: (u32, u32),
) -> Option<(u32, u32, u32, u32)> {
    let (x, y, w, h) = css;
    let (x, w) = if w < 0.0 { (x + w, -w) } else { (x, w) };
    let (y, h) = if h < 0.0 { (y + h, -h) } else { (y, h) };
    let off_x = (win_pos.0 - origin.0) as f64;
    let off_y = (win_pos.1 - origin.1) as f64;
    let clamp = |v: f64, max: u32| v.round().clamp(0.0, max as f64) as u32;
    let x0 = clamp(x * scale + off_x, buf.0);
    let y0 = clamp(y * scale + off_y, buf.1);
    let x1 = clamp((x + w) * scale + off_x, buf.0);
    let y1 = clamp((y + h) * scale + off_y, buf.1);
    (x1 > x0 && y1 > y0).then(|| (x0, y0, x1 - x0, y1 - y0))
}

/// Crop a BGRA buffer to an RGB one (the grab's alpha is meaningless).
#[cfg(any(windows, test))]
fn crop_rgb(g: &Grab, (x, y, w, h): (u32, u32, u32, u32)) -> Vec<u8> {
    let mut out = Vec::with_capacity((w * h * 3) as usize);
    for row in y..y + h {
        let start = ((row * g.width + x) * 4) as usize;
        for px in g.bgra[start..start + (w * 4) as usize].chunks_exact(4) {
            out.extend_from_slice(&[px[2], px[1], px[0]]);
        }
    }
    out
}

#[cfg(windows)]
fn write_png(path: &Path, w: u32, h: u32, rgb: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("klasör açılamadı: {e}"))?;
    }
    let file = std::fs::File::create(path).map_err(|e| format!("dosya yazılamadı: {e}"))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(rgb).map_err(|e| e.to_string())
}

#[cfg(windows)]
fn close_overlays(app: &AppHandle) {
    for (label, w) in app.webview_windows() {
        if label.starts_with(LABEL_PREFIX) {
            let _ = w.destroy();
        }
    }
}

/// Grab the screen and open the selection overlays. A second call while one is open is a no-op.
#[tauri::command]
pub async fn snip_start(app: AppHandle) -> Result<(), String> {
    start(&app)
}

/// Trigger entry point: the global hotkey (`snip_hotkey`) calls this; the webview may use `snip_start`.
#[cfg(windows)]
pub fn start(app: &AppHandle) -> Result<(), String> {
    if app.webview_windows().keys().any(|l| l.starts_with(LABEL_PREFIX)) {
        return Ok(());
    }
    *GRAB.lock().unwrap() = Some(win::grab()?);
    let cursor = app.cursor_position().ok();
    let monitors = app.available_monitors().map_err(|e| e.to_string())?;
    for (i, m) in monitors.iter().enumerate() {
        let (pos, size) = (*m.position(), *m.size());
        let w = WebviewWindowBuilder::new(app, format!("{LABEL_PREFIX}{i}"), WebviewUrl::App("index.html?snip".into()))
            .data_directory(crate::app_paths::get(app).local_data_dir)
            .title("Dex ekran alıntısı")
            .decorations(false)
            .transparent(true)
            .shadow(false)
            .resizable(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .visible(false)
            .build()
            .map_err(|e| e.to_string())?;
        // Physical, after creation: a logical builder position would be scaled by the wrong monitor.
        let _ = w.set_position(PhysicalPosition::new(pos.x, pos.y));
        let _ = w.set_size(PhysicalSize::new(size.width, size.height));
        let _ = w.show();
        let under_cursor = cursor.is_some_and(|c| {
            let (cx, cy) = (c.x as i32, c.y as i32);
            cx >= pos.x && cy >= pos.y && cx < pos.x + size.width as i32 && cy < pos.y + size.height as i32
        });
        if under_cursor {
            let _ = w.set_focus();
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn start(_: &AppHandle) -> Result<(), String> {
    Err(UNSUPPORTED.into())
}

/// CSS px → physical px of the overlay: its `devicePixelRatio`. Not `scale_factor()` alone —
/// WebView2's rasterization scale is monitor DPI × Windows' text size (Accessibility → Text size),
/// so at 100% DPI with 151% text the overlay reports 1.51 and the window 1.0 (#57). A missing or
/// nonsense ratio falls back to the window's DPI scale.
#[cfg(any(windows, test))]
fn css_scale(dpr: Option<f64>, window_scale: f64) -> f64 {
    dpr.filter(|d| d.is_finite() && *d > 0.0).unwrap_or(window_scale)
}

/// The overlay's dragged rect (its CSS pixels) → PNG on disk + its path on the clipboard.
#[cfg(windows)]
#[tauri::command]
pub async fn snip_finish(
    app: AppHandle,
    window: WebviewWindow,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    dpr: Option<f64>,
) -> Result<String, String> {
    let scale = css_scale(dpr, window.scale_factor().map_err(|e| e.to_string())?);
    let pos = window.outer_position().map_err(|e| e.to_string())?;
    close_overlays(&app);
    let Some(grab) = GRAB.lock().unwrap().take() else {
        return Err("aktif ekran alıntısı yok".into());
    };
    let pictures = app.path().picture_dir().map_err(|e| e.to_string())?;
    let owner = app.get_webview_window("main").and_then(|m| m.hwnd().ok()).map(|h| h.0 as isize);
    tauri::async_runtime::spawn_blocking(move || {
        let rect = to_pixels((x, y, w, h), scale, (pos.x, pos.y), grab.origin, (grab.width, grab.height))
            .ok_or("boş seçim")?;
        let path = target_path(&pictures, &file_name(win::local_time()), |p| p.exists());
        write_png(&path, rect.2, rect.3, &crop_rgb(&grab, rect))?;
        let text = path.to_string_lossy().into_owned();
        win::set_clipboard_text(owner, &text)?;
        Ok(text)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(windows)]
#[tauri::command]
pub async fn snip_cancel(app: AppHandle) {
    close_overlays(&app);
    *GRAB.lock().unwrap() = None;
}

#[cfg(not(windows))]
#[tauri::command]
pub async fn snip_finish(
    app: AppHandle,
    window: WebviewWindow,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    dpr: Option<f64>,
) -> Result<String, String> {
    let _ = (app, window, x, y, w, h, dpr);
    Err(UNSUPPORTED.into())
}

#[cfg(not(windows))]
#[tauri::command]
pub async fn snip_cancel(app: AppHandle) -> Result<(), String> {
    let _ = app;
    Err(UNSUPPORTED.into())
}

#[cfg(windows)]
mod win {
    use super::Grab;
    use std::ffi::c_void;
    use windows_sys::Win32::Foundation::{GlobalFree, HWND, SYSTEMTIME};
    use windows_sys::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
        BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, SRCCOPY,
    };
    use windows_sys::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    };

    const CF_UNICODETEXT: u32 = 13;

    /// The whole virtual screen in physical pixels (tao makes the process per-monitor DPI aware).
    pub fn grab() -> Result<Grab, String> {
        let (x, y, w, h) = unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )
        };
        grab_rect(x, y, w, h)
    }

    /// A virtual-screen rect in physical pixels.
    pub fn grab_rect(x: i32, y: i32, w: i32, h: i32) -> Result<Grab, String> {
        if w <= 0 || h <= 0 {
            return Err("ekran boyutu okunamadı".into());
        }
        unsafe {
            let screen = GetDC(std::ptr::null_mut());
            let mem = CreateCompatibleDC(screen);
            let mut info: BITMAPINFO = std::mem::zeroed();
            info.bmiHeader = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down rows
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..std::mem::zeroed()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let bmp = CreateDIBSection(mem, &info, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
            let result = if bmp.is_null() || bits.is_null() {
                Err("ekran tamponu ayrılamadı".to_string())
            } else {
                let old = SelectObject(mem, bmp);
                // CAPTUREBLT: include layered (transparent/topmost) windows, as the user sees them.
                let ok = BitBlt(mem, 0, 0, w, h, screen, x, y, SRCCOPY | CAPTUREBLT);
                let len = (w as usize) * (h as usize) * 4;
                let bgra = std::slice::from_raw_parts(bits as *const u8, len).to_vec();
                SelectObject(mem, old);
                if ok == 0 {
                    Err("ekran kopyalanamadı".to_string())
                } else {
                    Ok(Grab { origin: (x, y), width: w as u32, height: h as u32, bgra })
                }
            };
            if !bmp.is_null() {
                DeleteObject(bmp);
            }
            DeleteDC(mem);
            ReleaseDC(std::ptr::null_mut(), screen);
            result
        }
    }

    pub fn local_time() -> (u16, u16, u16, u16, u16, u16) {
        let mut t: SYSTEMTIME = unsafe { std::mem::zeroed() };
        unsafe { GetLocalTime(&mut t) };
        (t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
    }

    /// Plain-text clipboard (CF_UNICODETEXT). `owner`: Dex's main window, so the data has an owner.
    pub fn set_clipboard_text(owner: Option<isize>, text: &str) -> Result<(), String> {
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let hwnd = owner.unwrap_or(0) as HWND;
        unsafe {
            // Another app may hold the clipboard for a moment.
            let mut opened = false;
            for _ in 0..10 {
                if OpenClipboard(hwnd) != 0 {
                    opened = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            if !opened {
                return Err("pano açılamadı".into());
            }
            let result = (|| {
                if EmptyClipboard() == 0 {
                    return Err("pano boşaltılamadı".to_string());
                }
                let mem = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2);
                if mem.is_null() {
                    return Err("pano belleği ayrılamadı".to_string());
                }
                let dst = GlobalLock(mem) as *mut u16;
                if dst.is_null() {
                    GlobalFree(mem);
                    return Err("pano belleği kilitlenemedi".to_string());
                }
                std::ptr::copy_nonoverlapping(wide.as_ptr(), dst, wide.len());
                GlobalUnlock(mem);
                // On success the system owns `mem`.
                if SetClipboardData(CF_UNICODETEXT, mem).is_null() {
                    GlobalFree(mem);
                    return Err("panoya yazılamadı".to_string());
                }
                Ok(())
            })();
            CloseClipboard();
            result
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_matches_windows_format() {
        assert_eq!(file_name((2026, 10, 2, 9, 5, 7)), "Screenshot 2026-10-02 090507.png");
    }

    #[test]
    fn path_is_under_pictures_screenshots_and_dedupes() {
        let pics = Path::new(r"C:\Users\x\Pictures");
        let name = "Screenshot 2026-10-02 090507.png";
        assert_eq!(target_path(pics, name, |_| false), pics.join("Screenshots").join(name));
        let taken = [pics.join("Screenshots").join(name), pics.join("Screenshots").join("Screenshot 2026-10-02 090507 (2).png")];
        assert_eq!(
            target_path(pics, name, |p| taken.iter().any(|t| t == p)),
            pics.join("Screenshots").join("Screenshot 2026-10-02 090507 (3).png")
        );
    }

    #[test]
    fn pixels_on_primary_at_100_percent() {
        assert_eq!(to_pixels((10.0, 20.0, 30.0, 40.0), 1.0, (0, 0), (0, 0), (1920, 1080)), Some((10, 20, 30, 40)));
    }

    #[test]
    fn pixels_scale_with_dpi() {
        // 150%: CSS 100x50 at (10,20) → 150x75 at (15,30).
        assert_eq!(to_pixels((10.0, 20.0, 100.0, 50.0), 1.5, (0, 0), (0, 0), (3840, 2160)), Some((15, 30, 150, 75)));
    }

    #[test]
    fn pixels_on_secondary_left_of_primary() {
        // Monitor at virtual x=-2560 (125%), primary at 0: the grab starts at -2560.
        let r = to_pixels((100.0, 100.0, 40.0, 40.0), 1.25, (-2560, 0), (-2560, 0), (4480, 1440));
        assert_eq!(r, Some((125, 125, 50, 50)));
        // Primary (100%) in the same grab is offset by 2560.
        let r = to_pixels((0.0, 0.0, 10.0, 10.0), 1.0, (0, 0), (-2560, 0), (4480, 1440));
        assert_eq!(r, Some((2560, 0, 10, 10)));
    }

    #[test]
    fn pixels_normalize_reverse_drag_and_clamp() {
        assert_eq!(to_pixels((50.0, 50.0, -20.0, -10.0), 1.0, (0, 0), (0, 0), (100, 100)), Some((30, 40, 20, 10)));
        assert_eq!(to_pixels((90.0, 90.0, 50.0, 50.0), 1.0, (0, 0), (0, 0), (100, 100)), Some((90, 90, 10, 10)));
        assert_eq!(to_pixels((5.0, 5.0, 0.0, 10.0), 1.0, (0, 0), (0, 0), (100, 100)), None);
    }

    #[test]
    fn pixels_at_each_common_scale() {
        // CSS (40,20) 80x60 on a primary monitor at 100/125/150/200%.
        for (scale, want) in [(1.0, (40, 20, 80, 60)), (1.25, (50, 25, 100, 75)), (1.5, (60, 30, 120, 90)), (2.0, (80, 40, 160, 120))] {
            assert_eq!(to_pixels((40.0, 20.0, 80.0, 60.0), scale, (0, 0), (0, 0), (7680, 4320)), Some(want), "{scale}");
        }
    }

    #[test]
    fn scale_is_the_overlays_device_pixel_ratio() {
        assert_eq!(css_scale(Some(1.51), 1.0), 1.51);
        assert_eq!(css_scale(None, 1.25), 1.25);
        assert_eq!(css_scale(Some(0.0), 1.5), 1.5);
        assert_eq!(css_scale(Some(f64::NAN), 1.5), 1.5);
    }

    #[test]
    fn text_size_on_top_of_dpi_maps_back_to_the_dragged_pixels() {
        // Two-monitor example: both monitors 100% DPI, Windows text size 151% → the overlay's
        // devicePixelRatio is 1.51. A drag over physical (1000,500)-(1300,700) on the primary
        // arrives as CSS /1.51; the left monitor sits at x=-2560.
        let css = |p: f64| p / 1.51;
        let rect = (css(1000.0), css(500.0), css(300.0), css(200.0));
        let grab = (6400, 2160);
        assert_eq!(to_pixels(rect, css_scale(Some(1.51), 1.0), (0, 0), (-2560, 0), grab), Some((3560, 500, 300, 200)));
        // Using window scale 1.0 gives a 2/3-size box: 199x133 at the wrong spot.
        assert_eq!(to_pixels(rect, 1.0, (0, 0), (-2560, 0), grab), Some((3222, 331, 199, 133)));
        // Left monitor, dragged right-to-left: physical x -100 → -400 (window-relative 2460 → 2160).
        let rev = (css(2460.0), css(300.0), -css(300.0), css(600.0));
        assert_eq!(to_pixels(rev, 1.51, (-2560, 0), (-2560, 0), grab), Some((2160, 300, 300, 600)));
    }

    /// Live: crops the real full-screen grab at a fixed virtual-screen rect on every monitor and
    /// compares it with an independent capture of just that rect (the path `snip_finish` takes,
    /// minus the overlay). `cargo test -p dex snip::tests::live -- --ignored --nocapture`
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn live_grab_crop_matches_direct_capture() {
        let grab = win::grab().unwrap();
        println!("virtual screen origin {:?}, {}x{}", grab.origin, grab.width, grab.height);
        let (ox, oy) = grab.origin;
        // 300x200 at 400 px in from the virtual screen's left/top edge, and at the primary's (0,0)+400.
        for (vx, vy) in [(ox + 400, oy + 400), (400, 400)] {
            let rect = to_pixels((vx as f64, vy as f64, 300.0, 200.0), 1.0, (0, 0), grab.origin, (grab.width, grab.height)).unwrap();
            assert_eq!((rect.2, rect.3), (300, 200));
            let direct = win::grab_rect(vx, vy, 300, 200).unwrap();
            let a = crop_rgb(&grab, rect);
            let b = crop_rgb(&direct, (0, 0, 300, 200));
            let same = a.chunks(3).zip(b.chunks(3)).filter(|(p, q)| p == q).count();
            println!("({vx},{vy}) 300x200: {same}/60000 px identical");
            assert!(same >= 59_000, "crop is not the requested region");
        }
    }

    #[test]
    fn crop_converts_bgra_to_rgb() {
        // 2x2: pixel (1,1) = B1 G2 R3.
        let mut bgra = vec![0u8; 16];
        bgra[12..16].copy_from_slice(&[1, 2, 3, 0]);
        let g = Grab { origin: (0, 0), width: 2, height: 2, bgra };
        assert_eq!(crop_rgb(&g, (1, 1, 1, 1)), vec![3, 2, 1]);
    }
}
