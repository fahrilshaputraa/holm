#![windows_subsystem = "windows"]

mod bar;
mod canvas;
mod config;
mod dock;
mod lyrics;
mod media;
mod menus;
mod monitors;
mod notch;
mod notifications;
mod popup;
mod radios;
mod status;
mod taskbar;

use canvas::wide;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentThreadId};
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT};
use windows::Win32::UI::Shell::{ShellExecuteW, ABN_FULLSCREENAPP, ABN_POSCHANGED};
use windows::Win32::UI::WindowsAndMessaging::*;

pub const IDM_OPEN_CONFIG: usize = 1001;
pub const IDM_RELOAD: usize = 1002;
pub const IDM_EXIT: usize = 1003;
const IDM_PREVIEW_NOTIF: usize = 1004;

const TIMER_TICK: usize = 1;
const TICK_MS: u32 = 15_000;

/// Pesan thread (hwnd kosong) yang ditangani di message loop.
const WM_FOREGROUND: u32 = WM_APP + 2;
const WM_REBUILD: u32 = WM_APP + 10;

static MAIN_TID: AtomicU32 = AtomicU32::new(0);
static REBUILD_PENDING: AtomicBool = AtomicBool::new(false);
static QUITTING: AtomicBool = AtomicBool::new(false);
static REBUILDING: AtomicBool = AtomicBool::new(false);
static CONFIG_STAMP: std::sync::Mutex<Option<std::time::SystemTime>> = std::sync::Mutex::new(None);

#[derive(Default)]
struct Windows {
    bars: Vec<HWND>,
    notches: Vec<HWND>,
    docks: Vec<HWND>,
}

thread_local! {
    static WINDOWS: RefCell<Windows> = RefCell::new(Windows::default());
}

fn docks() -> Vec<HWND> {
    WINDOWS.with(|w| w.borrow().docks.clone())
}

fn bars() -> Vec<HWND> {
    WINDOWS.with(|w| w.borrow().bars.clone())
}

fn notches() -> Vec<HWND> {
    WINDOWS.with(|w| w.borrow().notches.clone())
}

pub fn show_menu(owner: HWND) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let _ = AppendMenuW(menu, MF_STRING, IDM_OPEN_CONFIG, w!("Open config"));
        let _ = AppendMenuW(menu, MF_STRING, IDM_RELOAD, w!("Reload config"));
        let _ = AppendMenuW(menu, MF_STRING, IDM_PREVIEW_NOTIF, w!("Preview notification"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, IDM_EXIT, w!("Quit HormBar"));
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(owner);
        let _ = TrackPopupMenuEx(menu, TPM_RIGHTBUTTON.0, pt.x, pt.y, owner, None);
        let _ = DestroyMenu(menu);
    }
}

fn open_uri(uri: &str) {
    let u = wide(uri);
    unsafe {
        ShellExecuteW(None, w!("open"), PCWSTR(u.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

fn open_config() {
    let e = wide("notepad.exe");
    let a = wide(&format!("\"{}\"", config::path().to_string_lossy()));
    unsafe {
        ShellExecuteW(None, w!("open"), PCWSTR(e.as_ptr()), PCWSTR(a.as_ptr()), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

pub fn set_z(z: HWND) {
    let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
    unsafe {
        for h in bars().into_iter().chain(notches()).chain(docks()) {
            let _ = SetWindowPos(h, Some(z), 0, 0, 0, 0, flags);
        }
    }
}

/// Notch harus selalu di atas bar; registrasi AppBar membuat bar naik ke atas.
fn raise_notches() {
    let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
    for n in notches() {
        unsafe {
            let _ = SetWindowPos(n, Some(HWND_TOPMOST), 0, 0, 0, 0, flags);
        }
    }
}

fn destroy_all() {
    popup::destroy();
    let (b, n, d) = WINDOWS.with(|w| {
        let mut w = w.borrow_mut();
        (std::mem::take(&mut w.bars), std::mem::take(&mut w.notches), std::mem::take(&mut w.docks))
    });
    unsafe {
        for h in d {
            dock::detach(h);
            let _ = DestroyWindow(h);
        }
        for h in b {
            bar::detach(h);
            let _ = DestroyWindow(h);
        }
        for h in n {
            notch::detach(h);
            let _ = DestroyWindow(h);
        }
    }
}

fn create(class: &str, title: PCWSTR) -> Option<HWND> {
    let ex = WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE;
    unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None).ok()?.into();
        let c = wide(class);
        CreateWindowExW(ex, PCWSTR(c.as_ptr()), title, WS_POPUP, 0, 0, 10, 10, None, None, Some(hinst), None).ok()
    }
}

/// Bangun ulang jendela: satu bar dan satu notch per monitor terpilih (sesuai config & layar saat ini).
fn rebuild() {
    REBUILD_PENDING.store(false, Ordering::SeqCst);
    REBUILDING.store(true, Ordering::SeqCst);
    destroy_all();
    let cfg = config::current();
    lyrics::ENABLED.store(cfg.lyrics.enabled, Ordering::Relaxed);

    let mut new_bars = Vec::new();
    if cfg.bar.enabled {
        for mon in monitors::selected(&cfg.bar.monitors) {
            if let Some(h) = create("HormBarClass", w!("HormBar")) {
                bar::attach(h, mon);
                unsafe {
                    let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
                    SetCoalescableTimer(Some(h), TIMER_TICK, TICK_MS, None, 5_000);
                }
                new_bars.push(h);
            }
        }
    }
    let mut new_notches = Vec::new();
    if cfg.notch.enabled {
        for mon in monitors::selected(&cfg.notch.monitors) {
            if let Some(h) = create("HormNotchClass", w!("HormBar Notch")) {
                notch::attach(h, mon);
                unsafe {
                    let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
                }
                new_notches.push(h);
            }
        }
    }
    let mut new_docks = Vec::new();
    if cfg.dock.enabled {
        for mon in monitors::selected(&cfg.dock.monitors) {
            if let Some(h) = create("HormDockClass", w!("HormBar Dock")) {
                dock::attach(h, mon);
                unsafe {
                    let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
                }
                new_docks.push(h);
            }
        }
    }
    WINDOWS.with(|w| {
        let mut w = w.borrow_mut();
        w.bars = new_bars.clone();
        w.notches = new_notches.clone();
        w.docks = new_docks.clone();
    });
    for h in &new_bars {
        bar::layout(*h);
    }
    for h in &new_notches {
        notch::redraw(*h);
    }
    dock::reset_pins();
    dock::refresh(&new_docks, None);
    raise_notches();
    if cfg.dock.enabled && cfg.dock.replace_taskbar {
        taskbar::hide();
    } else {
        taskbar::restore();
    }
    REBUILDING.store(false, Ordering::SeqCst);
}

/// Jadwalkan rebuild; banyak pemicu (satu per bar) digabung menjadi satu.
fn request_rebuild() {
    if !REBUILD_PENDING.swap(true, Ordering::SeqCst) {
        unsafe {
            let _ = PostThreadMessageW(MAIN_TID.load(Ordering::Relaxed), WM_REBUILD, WPARAM(0), LPARAM(0));
        }
    }
}

/// (Re)daftarkan pintasan peluncur dari config; gagal (dipakai program lain) diabaikan.
fn register_hotkey() {
    unsafe {
        let _ = UnregisterHotKey(None, 1);
        if let Some((mods, key)) = config::parse_hotkey(&config::current().launcher.hotkey) {
            let _ = RegisterHotKey(None, 1, HOT_KEY_MODIFIERS(mods) | MOD_NOREPEAT, key);
        }
    }
}

fn reload_config() {
    if config::reload().is_ok() {
        rebuild();
        register_hotkey();
    }
    if let Ok(mut s) = CONFIG_STAMP.lock() {
        *s = config::mtime();
    }
}

fn quit() {
    QUITTING.store(true, Ordering::SeqCst);
    taskbar::restore();
    destroy_all();
    unsafe { PostQuitMessage(0) };
}

unsafe extern "system" fn fg_hook(_h: HWINEVENTHOOK, _ev: u32, hwnd: HWND, _id: i32, _child: i32, _tid: u32, _ms: u32) {
    unsafe {
        let _ = PostThreadMessageW(MAIN_TID.load(Ordering::Relaxed), WM_FOREGROUND, WPARAM(0), LPARAM(hwnd.0 as isize));
    }
}

static DOCK_PENDING: AtomicBool = AtomicBool::new(false);

/// Jendela dibuat/ditampilkan/disembunyikan/dihancurkan/di-cloak: gabungkan menjadi satu refresh dock.
unsafe extern "system" fn dock_hook(_h: HWINEVENTHOOK, _ev: u32, hwnd: HWND, id_object: i32, id_child: i32, _tid: u32, _ms: u32) {
    if id_object != 0 || id_child != 0 || hwnd.0.is_null() {
        return; // hanya jendela top-level, bukan objek/anak di dalamnya
    }
    // Host banner toast (XAML/CoreWindow milik shell) bisa baru muncul: minta pemindaian cepat.
    let mut cls = [0u16; 64];
    let n = unsafe { GetClassNameW(hwnd, &mut cls) }.max(0) as usize;
    let cls = String::from_utf16_lossy(&cls[..n]);
    if cls.contains("CoreWindow") || cls.contains("Xaml") || cls.contains("Toast") || cls.contains("Notification") {
        notifications::kick();
    }
    if !DOCK_PENDING.swap(true, Ordering::SeqCst) {
        unsafe {
            let _ = PostThreadMessageW(MAIN_TID.load(Ordering::Relaxed), dock::WM_DOCK_REFRESH, WPARAM(0), LPARAM(0));
        }
    }
}

unsafe extern "system" fn bar_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER if wparam.0 == TIMER_TICK => {
            let stamp = config::mtime();
            let changed = CONFIG_STAMP.lock().map(|s| *s != stamp).unwrap_or(false);
            if changed {
                reload_config(); // menghancurkan jendela ini; jangan disentuh lagi
            } else {
                bar::redraw(hwnd);
                dock::refresh(&docks(), None); // jaring pengaman bila ada event yang terlewat
                let cfg = config::current();
                if cfg.dock.enabled && cfg.dock.replace_taskbar {
                    taskbar::hide();
                }
            }
            LRESULT(0)
        }
        bar::WM_APPBAR => {
            match wparam.0 as u32 {
                ABN_FULLSCREENAPP => set_z(if lparam.0 != 0 { HWND_BOTTOM } else { HWND_TOPMOST }),
                ABN_POSCHANGED => {
                    bar::layout(hwnd);
                    raise_notches();
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE | WM_DPICHANGED => {
            request_rebuild();
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let x = (lparam.0 & 0xFFFF) as i16 as f32;
            if let Some((action, x0, x1, mon, bar_h)) = bar::hit_test(hwnd, x) {
                let below = mon.y + bar_h;
                match action {
                    bar::Action::StartMenu => menus::show_start_menu(hwnd, x0 as i32, below),
                    bar::Action::AppMenu(i) => {
                        let title = config::current().bar.menus.get(i).cloned().unwrap_or_default();
                        menus::show_app_menu(hwnd, &title, x0 as i32, below, bar::last_app_window());
                    }
                    bar::Action::Popup(kind) => popup::toggle(kind, (x0 + x1) / 2.0, mon),
                    bar::Action::Uri(u) if !u.is_empty() => open_uri(u),
                    bar::Action::Uri(_) => {}
                }
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            show_menu(hwnd);
            LRESULT(0)
        }
        WM_COMMAND => {
            match wparam.0 & 0xFFFF {
                IDM_OPEN_CONFIG => open_config(),
                IDM_RELOAD => reload_config(),
                IDM_PREVIEW_NOTIF => {
                    let t = notifications::Toast {
                        app: "HormBar".into(),
                        title: "Contoh notifikasi".into(),
                        body: "Begini tampilan banner Windows di notch.".into(),
                        icon: None,
                    };
                    notches().into_iter().for_each(|h| notch::notify(h, t.clone()));
                }
                IDM_EXIT => quit(),
                _ => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_TICK);
            }
            bar::remove(hwnd);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn register_classes() {
    unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None).unwrap().into();
        let cursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
        let class_bar = wide("HormBarClass");
        let class_notch = wide("HormNotchClass");
        let class_dock = wide("HormDockClass");
        let class_popup = wide("HormPopupClass");
        for (name, proc) in [
            (&class_popup, Some(popup::wndproc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT)),
            (&class_dock, Some(dock::wndproc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT)),
            (&class_bar, Some(bar_wndproc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT)),
            (&class_notch, Some(notch::wndproc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT)),
        ] {
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: proc,
                hInstance: hinst,
                hCursor: cursor,
                lpszClassName: PCWSTR(name.as_ptr()),
                ..Default::default()
            };
            RegisterClassExW(&wc);
        }
    }
}

/// Pesan thread dari hook/thread latar: dibagikan ke semua jendela.
fn handle_thread_message(msg: &MSG) {
    match msg.message {
        media::WM_MEDIA_CHANGED => notches().into_iter().for_each(notch::on_media),
        status::WM_STATUS_CHANGED => {
            bars().into_iter().for_each(bar::redraw);
            popup::on_status_changed();
        }
        WM_FOREGROUND => {
            let fg = HWND(msg.lParam.0 as *mut _);
            if bar::on_foreground(fg) {
                bars().into_iter().for_each(bar::redraw);
            }
            dock::refresh(&docks(), Some(fg));
        }
        dock::WM_DOCK_REFRESH => {
            DOCK_PENDING.store(false, Ordering::SeqCst);
            dock::refresh(&docks(), None);
        }
        notifications::WM_NOTIFY_TOAST => {
            let t = unsafe { Box::from_raw(msg.lParam.0 as *mut notifications::Toast) };
            notches().into_iter().for_each(|h| notch::notify(h, (*t).clone()));
        }
        WM_REBUILD => rebuild(),
        WM_HOTKEY => popup::open_launcher(),
        _ => {}
    }
}

fn main() {
    unsafe {
        // Satu instance saja.
        let _guard = CreateMutexW(None, false, w!("Local\\HormBarSingleton"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        MAIN_TID.store(GetCurrentThreadId(), Ordering::Relaxed);

        taskbar::install_panic_guard();
        config::init();
        canvas::startup();
        register_classes();

        *CONFIG_STAMP.lock().unwrap() = config::mtime();
        rebuild();

        // Event-driven: jendela aktif, media, volume, Wi-Fi. Timer 15 dtk hanya untuk jam/baterai/config.
        let _hook = SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, None, Some(fg_hook), 0, 0, WINEVENT_OUTOFCONTEXT);
        // DESTROY(0x8001)..HIDE(0x8003) dan CLOAKED(0x8017)..UNCLOAKED(0x8018)
        let _h2 = SetWinEventHook(0x8001, 0x8003, None, Some(dock_hook), 0, 0, WINEVENT_OUTOFCONTEXT);
        let _h3 = SetWinEventHook(0x8017, 0x8018, None, Some(dock_hook), 0, 0, WINEVENT_OUTOFCONTEXT);
        let tid = GetCurrentThreadId();
        register_hotkey();
        media::start(tid);
        status::start(tid);
        {
            let n = &config::current().notifications;
            if n.enabled {
                notifications::start(tid, n.poll_ms);
            }
        }
        if bar::on_foreground(GetForegroundWindow()) {
            bars().into_iter().for_each(bar::redraw);
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if msg.hwnd.0.is_null() {
                handle_thread_message(&msg);
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
            if QUITTING.load(Ordering::Relaxed) {
                break;
            }
        }
    }
    taskbar::restore();
    canvas::shutdown();
}
