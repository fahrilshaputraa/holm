//! Mengganti taskbar Windows dengan Dock: taskbar asli disembunyikan (bukan diubah setting-nya)
//! dan SELALU dikembalikan saat aplikasi keluar, panik, atau konfigurasi dimatikan.
//! Jika proses dimatikan paksa, taskbar tetap tersembunyi sampai aplikasi dijalankan lagi lalu
//! di-Quit, atau explorer di-restart (`taskkill /f /im explorer.exe` lalu `start explorer`).

use std::sync::atomic::{AtomicBool, Ordering};
use windows::Win32::Foundation::*;
use windows::Win32::UI::WindowsAndMessaging::*;

static WE_HID: AtomicBool = AtomicBool::new(false);

/// Jendela taskbar milik PROSES SHELL saja (pemilik jendela desktop). Bisa ada jendela
/// `Shell_TrayWnd` lain milik proses lain; `FindWindow` akan memilih yang salah.
fn taskbars() -> Vec<HWND> {
    struct Ctx {
        shell_pid: u32,
        out: Vec<HWND>,
    }
    unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> windows::core::BOOL {
        let ctx = unsafe { &mut *(l.0 as *mut Ctx) };
        let mut buf = [0u16; 64];
        let n = unsafe { GetClassNameW(h, &mut buf) };
        let class = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
        if class == "Shell_TrayWnd" || class == "Shell_SecondaryTrayWnd" {
            let mut pid = 0u32;
            unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
            if ctx.shell_pid == 0 || pid == ctx.shell_pid {
                ctx.out.push(h);
            }
        }
        windows::core::BOOL(1)
    }
    let mut shell_pid = 0u32;
    unsafe {
        let shell = GetShellWindow();
        if !shell.0.is_null() {
            GetWindowThreadProcessId(shell, Some(&mut shell_pid));
        }
    }
    let mut ctx = Ctx { shell_pid, out: Vec::new() };
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut ctx as *mut _ as isize));
    }
    ctx.out
}

/// Tebal taskbar bawaan Windows (px) bila menempel di tepi bawah monitor utama, selain itu 0.
/// Taskbar yang disembunyikan tetap menyisakan ruangnya di area kerja, jadi appbar Dock hanya
/// perlu menambah selisihnya (lihat dock::layout).
pub fn thickness() -> i32 {
    for h in taskbars() {
        let mut r = RECT::default();
        if unsafe { GetWindowRect(h, &mut r) }.is_err() {
            continue;
        }
        let (w, hh) = (r.right - r.left, r.bottom - r.top);
        if w > hh && r.top > 0 {
            return hh;
        }
    }
    0
}

/// Sembunyikan taskbar (idempoten; dipanggil ulang berkala karena explorer bisa menampilkannya lagi).
pub fn hide() {
    for h in taskbars() {
        unsafe {
            if IsWindowVisible(h).as_bool() {
                let _ = ShowWindow(h, SW_HIDE);
                WE_HID.store(true, Ordering::SeqCst);
            }
        }
    }
}

/// Tampilkan kembali taskbar bila kita yang menyembunyikannya.
pub fn restore() {
    if WE_HID.swap(false, Ordering::SeqCst) {
        for h in taskbars() {
            unsafe {
                let _ = ShowWindow(h, SW_SHOWNA);
            }
        }
    }
}

/// Pasang hook panik agar taskbar tidak tertinggal tersembunyi bila aplikasi crash.
pub fn install_panic_guard() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        prev(info);
    }));
}