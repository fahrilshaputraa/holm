//! Enumerasi monitor beserta DPI efektif masing-masing.

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::HiDpi::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mon {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub scale: f32,
    pub primary: bool,
}

unsafe extern "system" fn collect(hmon: HMONITOR, _hdc: HDC, _rc: *mut RECT, lparam: LPARAM) -> windows::core::BOOL {
    let list = unsafe { &mut *(lparam.0 as *mut Vec<Mon>) };
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    if unsafe { GetMonitorInfoW(hmon, &mut info) }.as_bool() {
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = unsafe { GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) };
        let r = info.rcMonitor;
        list.push(Mon {
            x: r.left,
            y: r.top,
            w: r.right - r.left,
            h: r.bottom - r.top,
            scale: dx.max(96) as f32 / 96.0,
            primary: info.dwFlags & 1 != 0,
        });
    }
    windows::core::BOOL(1)
}

/// Semua monitor aktif; monitor utama selalu pertama.
pub fn all() -> Vec<Mon> {
    let mut v: Vec<Mon> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(collect), LPARAM(&mut v as *mut _ as isize));
    }
    v.sort_by_key(|m| !m.primary);
    v
}

/// Monitor yang dipakai sesuai pengaturan `"all"` / `"primary"`.
pub fn selected(mode: &str) -> Vec<Mon> {
    let v = all();
    if mode == "primary" {
        v.into_iter().take(1).collect()
    } else {
        v
    }
}
