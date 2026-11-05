//! Mengganti taskbar Windows dengan Dock: taskbar asli disembunyikan (bukan diubah setting-nya)
//! dan SELALU dikembalikan saat aplikasi keluar, panik, atau konfigurasi dimatikan.
//! Jika proses dimatikan paksa, taskbar tetap tersembunyi sampai aplikasi dijalankan lagi lalu
//! di-Quit, atau explorer di-restart (`taskkill /f /im explorer.exe` lalu `start explorer`).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use windows::Win32::Foundation::*;
use windows::Win32::UI::Shell::{SHAppBarMessage, ABM_GETSTATE, ABM_SETSTATE, APPBARDATA};
use windows::Win32::UI::WindowsAndMessaging::*;

static WE_HID: AtomicBool = AtomicBool::new(false);
static WE_AUTOHID: AtomicBool = AtomicBool::new(false);
static ORIG_STATE: AtomicUsize = AtomicUsize::new(0);
const ABS_AUTOHIDE: usize = 1;

fn state_file() -> std::path::PathBuf {
    crate::config::dir().join("taskbar-state.txt")
}

fn get_state() -> usize {
    unsafe {
        let mut abd: APPBARDATA = std::mem::zeroed();
        abd.cbSize = std::mem::size_of::<APPBARDATA>() as u32;
        SHAppBarMessage(ABM_GETSTATE, &mut abd)
    }
}

fn set_state(state: usize) {
    unsafe {
        let mut abd: APPBARDATA = std::mem::zeroed();
        abd.cbSize = std::mem::size_of::<APPBARDATA>() as u32;
        abd.hWnd = shell_tray().unwrap_or_default();
        abd.lParam = LPARAM(state as isize);
        SHAppBarMessage(ABM_SETSTATE, &mut abd);
    }
}

/// Taskbar yang disembunyikan tetap menyisakan ruangnya di area kerja (jendela maksimal berhenti
/// ~48 px di atas tepi bawah). Menyalakan auto-hide taskbar Windows melepas ruang itu; pengaturan
/// asli dicatat ke file dan DIKEMBALIKAN saat keluar (atau saat HolmBar dijalankan lagi setelah mati paksa).
pub fn reclaim_space(on: bool) {
    if on {
        if WE_AUTOHID.load(Ordering::SeqCst) {
            return;
        }
        let orig = get_state();
        if orig & ABS_AUTOHIDE != 0 {
            return; // sudah auto-hide oleh pengguna: ruang sudah bebas, jangan diubah/dipulihkan
        }
        let _ = std::fs::create_dir_all(crate::config::dir());
        let _ = std::fs::write(state_file(), orig.to_string());
        ORIG_STATE.store(orig, Ordering::SeqCst);
        WE_AUTOHID.store(true, Ordering::SeqCst);
        set_state(orig | ABS_AUTOHIDE);
    } else {
        restore_state();
    }
}

/// HolmBar yang menyalakan auto-hide taskbar (ruang bawah dilepas, Dock perlu menyisakan ruangnya sendiri).
pub fn autohid() -> bool {
    WE_AUTOHID.load(Ordering::SeqCst)
}

fn restore_state() {
    if WE_AUTOHID.swap(false, Ordering::SeqCst) {
        set_state(ORIG_STATE.load(Ordering::SeqCst));
        let _ = std::fs::remove_file(state_file());
    }
}

/// Setelah mati paksa: kembalikan pengaturan taskbar yang tercatat di file (bila ada).
pub fn recover() {
    if let Some(orig) = std::fs::read_to_string(state_file()).ok().and_then(|t| t.trim().parse::<usize>().ok()) {
        set_state(orig);
        let _ = std::fs::remove_file(state_file());
    }
}

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
            // Host tray HolmBar (tray.rs) juga ber-class Shell_TrayWnd: jangan pernah ikut disentuh.
            let ours = pid == unsafe { windows::Win32::System::Threading::GetCurrentProcessId() };
            if !ours && (ctx.shell_pid == 0 || pid == ctx.shell_pid) {
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

/// `Shell_TrayWnd` milik Explorer (tujuan penerusan pesan tray/AppBar).
pub fn shell_tray() -> Option<HWND> {
    taskbars().into_iter().find(|h| {
        let mut buf = [0u16; 32];
        let n = unsafe { GetClassNameW(*h, &mut buf) };
        String::from_utf16_lossy(&buf[..n.max(0) as usize]) == "Shell_TrayWnd"
    })
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

thread_local! {
    static CACHE: std::cell::RefCell<(Vec<HWND>, Option<std::time::Instant>)> = const { std::cell::RefCell::new((Vec::new(), None)) };
}

/// Sembunyikan lagi taskbar yang dimunculkan Explorer (mode auto-hide menampilkannya saat kursor
/// menyentuh tepi bawah). Murah: memakai daftar jendela yang di-cache dan dipanggil sering.
pub fn enforce() {
    if !WE_HID.load(Ordering::SeqCst) {
        return;
    }
    let list = CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.1.map(|t| t.elapsed().as_secs() >= 3).unwrap_or(true) {
            c.0 = taskbars();
            c.1 = Some(std::time::Instant::now());
        }
        c.0.clone()
    });
    for h in list {
        unsafe {
            if IsWindowVisible(h).as_bool() {
                let _ = ShowWindow(h, SW_HIDE);
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
    restore_state();
}

/// Pasang hook panik agar taskbar tidak tertinggal tersembunyi bila aplikasi crash.
pub fn install_panic_guard() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        prev(info);
    }));
}