#![windows_subsystem = "windows"]

mod apps;
mod bar;
mod canvas;
mod config;
mod weather;
mod idle;
mod control;
mod dock;
mod icons;
mod lyrics;
mod media;
mod menu;
mod menus;
mod monitors;
mod notch;
mod notifications;
mod popup;
mod preview;
mod radios;
mod settings;
mod status;
mod taskbar;
mod tray;

use canvas::wide;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentProcess, GetCurrentThreadId, TerminateProcess};
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT};
use windows::Win32::UI::Shell::{ShellExecuteW, ABN_FULLSCREENAPP, ABN_POSCHANGED};
use windows::Win32::UI::WindowsAndMessaging::*;

pub const IDM_OPEN_CONFIG: usize = 1001;
pub const IDM_RELOAD: usize = 1002;
pub const IDM_EXIT: usize = 1003;
const IDM_PREVIEW_NOTIF: usize = 1004;
pub const IDM_SETTINGS: usize = 1005;

const TIMER_TICK: usize = 1;
const TICK_MS: u32 = 15_000;

/// Pesan thread (hwnd kosong) yang ditangani di message loop.
const WM_FOREGROUND: u32 = WM_APP + 2;
const WM_REBUILD: u32 = WM_APP + 10;
/// Dikirim instance kedua ke jendela IPC: buka Pengaturan.
const WM_OPEN_SETTINGS: u32 = WM_APP + 11;
/// `holm-bar.exe --preview`: instance kedua meminta kartu contoh berikutnya di notch.
const WM_PREVIEW: u32 = WM_APP + 13;
/// Id timer thread untuk pemeriksaan layar penuh.
static FS_TIMER: AtomicUsize = AtomicUsize::new(0);

/// Jendela di depan menutupi seluruh monitornya (video/game/F11): bar, notch, dan Dock di monitor itu
/// disembunyikan, dan ditampilkan lagi begitu tidak lagi layar penuh.
fn update_fullscreen() {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return;
        }
        let mut cls = [0u16; 64];
        let n = GetClassNameW(fg, &mut cls).max(0) as usize;
        let cls = String::from_utf16_lossy(&cls[..n]);
        // Jendela milik HolmBar / shell (menu Start, desktop) tidak mengubah keadaan.
        if cls.starts_with("Holm") || matches!(cls.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "XamlExplorerHostIslandWindow" | "Windows.UI.Core.CoreWindow") {
            return;
        }
        let hmon = MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let mut r = RECT::default();
        let full = IsWindowVisible(fg).as_bool()
            && !IsIconic(fg).as_bool()
            && GetMonitorInfoW(hmon, &mut mi).as_bool()
            && GetWindowRect(fg, &mut r).is_ok()
            && r.left <= mi.rcMonitor.left
            && r.top <= mi.rcMonitor.top
            && r.right >= mi.rcMonitor.right
            && r.bottom >= mi.rcMonitor.bottom;
        for h in bars().into_iter().chain(notches()).chain(docks()) {
            let same = MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST) == hmon;
            let want_hidden = full && same;
            let shown = IsWindowVisible(h).as_bool();
            if want_hidden && shown {
                let _ = ShowWindow(h, SW_HIDE);
            } else if !want_hidden && !shown && (same || !full) {
                let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
            }
        }
    }
}

/// Instance kedua dengan `--quit` (dipakai installer/uninstaller): keluar dengan rapi (taskbar dipulihkan).
const WM_QUIT_APP: u32 = WM_APP + 14;
/// Instance kedua dengan `--shots`: simpan gambar tiap komponen ke `%APPDATA%\HolmBar\shots` (untuk dokumentasi).
const WM_SHOTS: u32 = WM_APP + 15;
const IPC_CLASS: PCWSTR = w!("HolmBarIpcClass");

static MAIN_TID: AtomicU32 = AtomicU32::new(0);
static REBUILD_PENDING: AtomicBool = AtomicBool::new(false);
/// Ada aplikasi layar penuh: bar/notch/Dock turun ke dasar dan tidak boleh dirampas kembali.
pub static FULLSCREEN: AtomicBool = AtomicBool::new(false);
static QUITTING: AtomicBool = AtomicBool::new(false);
static REBUILDING: AtomicBool = AtomicBool::new(false);
static NOTIF_STARTED: AtomicBool = AtomicBool::new(false);
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

pub(crate) fn bars() -> Vec<HWND> {
    WINDOWS.with(|w| w.borrow().bars.clone())
}

fn notches() -> Vec<HWND> {
    WINDOWS.with(|w| w.borrow().notches.clone())
}

/// Menu klik kanan HolmBar (bar, Dock, notch); digambar sendiri, bukan menu Win32.
pub fn show_menu(_owner: HWND) {
    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    let items = vec![
        menu::MenuItem::new("Settings...", "", IDM_SETTINGS),
        menu::MenuItem::new("Open config file", "", IDM_OPEN_CONFIG),
        menu::MenuItem::new("Reload config", "", IDM_RELOAD),
        menu::MenuItem::new("Preview notification", "", IDM_PREVIEW_NOTIF),
        menu::MenuItem::sep(),
        menu::MenuItem::new("Quit HolmBar", "", IDM_EXIT),
    ];
    menu::show(items, pt.x, pt.y, handle_command);
}

/// Perintah menu HolmBar (dari menu klik kanan atau menu logo Windows).
pub fn handle_command(id: usize) {
    match id {
        IDM_SETTINGS => settings::open(),
        IDM_OPEN_CONFIG => open_config(),
        IDM_RELOAD => reload_config(),
        IDM_PREVIEW_NOTIF => {
            // Tiap klik menampilkan jenis kartu berikutnya: pesan, panggilan, Bluetooth, mengisi daya.
            static N: AtomicUsize = AtomicUsize::new(0);
            use notifications::{Kind, Toast};
            let t = match N.fetch_add(1, Ordering::Relaxed) % 4 {
                0 => Toast::message("WhatsApp", "STI & DDMS", "~ Ulfah: Sticker · 511 pesan belum dibaca"),
                1 => {
                    let mut t = Toast::message("WhatsApp", "Prince", "Panggilan suara masuk");
                    t.actions = vec!["Tolak".into(), "Terima".into()];
                    t.kind = Kind::Call;
                    t
                }
                2 => {
                    let mut t = Toast::message("Bluetooth", "AirBuds Pro", "");
                    t.kind = Kind::Bluetooth { battery: Some(70), audio: true };
                    t
                }
                _ => {
                    let pct = bar::battery().map(|b| b.0).unwrap_or(70);
                    let mut t = Toast::message("", "Mengisi daya", "Pengisi daya tersambung");
                    t.kind = Kind::Charging { pct };
                    t
                }
            };
            notches().into_iter().for_each(|h| notch::notify(h, t.clone()));
        }
        IDM_EXIT => quit(),
        _ => {}
    }
}

fn open_uri(uri: &str) {
    let u = wide(uri);
    unsafe {
        ShellExecuteW(None, w!("open"), PCWSTR(u.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

pub(crate) fn open_config() {
    let e = wide("notepad.exe");
    let a = wide(&format!("\"{}\"", config::path().to_string_lossy()));
    unsafe {
        ShellExecuteW(None, w!("open"), PCWSTR(e.as_ptr()), PCWSTR(a.as_ptr()), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

pub fn set_fullscreen(on: bool) {
    FULLSCREEN.store(on, Ordering::SeqCst);
    set_z(if on { HWND_BOTTOM } else { HWND_TOPMOST });
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
            if let Some(h) = create("HolmBarClass", w!("HolmBar")) {
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
            if let Some(h) = create("HolmNotchClass", w!("HolmBar Notch")) {
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
            if let Some(h) = create("HolmDockClass", w!("HolmBar Dock")) {
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
    if cfg.dock.enabled && cfg.dock.replace_taskbar {
        taskbar::hide();
        taskbar::reclaim_space(cfg.dock.reclaim_space);
    } else {
        taskbar::restore();
    }
    dock::reset_pins();
    dock::refresh(&new_docks, None);
    raise_notches();
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

/// Mulai pembaca notifikasi bila diaktifkan dan belum berjalan (sekali per proses).
fn start_notifications() {
    let n = &config::current().notifications;
    if n.enabled && !NOTIF_STARTED.swap(true, Ordering::SeqCst) {
        notifications::start(MAIN_TID.load(Ordering::Relaxed), n.poll_ms);
    }
}

pub(crate) fn reload_config() {
    if config::reload().is_ok() {
        rebuild();
        register_hotkey();
        start_notifications();
        tray::sync();
        weather::set_city(&config::current().weather.city);
    }
    if let Ok(mut s) = CONFIG_STAMP.lock() {
        *s = config::mtime();
    }
}

/// Simpan gambar bar, Dock, dan tiap keadaan notch sebagai PNG (latar transparan).
fn shots() {
    let dir = config::dir().join("shots");
    let _ = std::fs::create_dir_all(&dir);
    // Mode demo (ada demo-art.png di folder shots): media contoh + nama aplikasi netral, tanpa data pribadi.
    let demo = std::fs::read(dir.join("demo-art.png")).ok();
    if demo.is_some() {
        media::set_demo(Some(("Midnight Drive", "Neon Harbor", demo.clone())));
        bar::set_app_name(&config::current().bar.desktop_label);
    }
    for (i, h) in bars().into_iter().enumerate() {
        canvas::capture_next(dir.join(format!("bar-{i}.png")));
        bar::force_redraw(h);
    }
    for (i, h) in docks().into_iter().enumerate() {
        canvas::capture_next(dir.join(format!("dock-{i}.png")));
        dock::redraw(h);
    }
    if let Some(h) = notches().into_iter().next() {
        notch::shots(h, &dir, None, "notch");
        notch::shots(h, &dir, Some(2.0), "notch2x");
    }
    if let Some(mut mon) = monitors::all().into_iter().next() {
        mon.scale = 2.0;
        for (kind, name) in [(popup::Kind::Control, "control"), (popup::Kind::Calendar, "calendar"), (popup::Kind::Volume, "volume")] {
            popup::shot(kind, mon, dir.join(format!("popup2x-{name}.png")), demo.is_some());
        }
    }
    canvas::capture_cancel();
    if demo.is_some() {
        media::set_demo(None);
        unsafe {
            bar::on_foreground(GetForegroundWindow());
        }
        bars().into_iter().for_each(bar::force_redraw);
        notches().into_iter().for_each(notch::redraw);
    }
}

fn quit() {
    QUITTING.store(true, Ordering::SeqCst);
    tray::stop();
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
    // Taskbar Windows (auto-hide) dimunculkan Explorer saat kursor di tepi bawah: sembunyikan seketika.
    if (cls == "Shell_TrayWnd" || cls == "Shell_SecondaryTrayWnd") && taskbar::autohid() {
        taskbar::enforce();
    }
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
                ABN_FULLSCREENAPP => set_fullscreen(lparam.0 != 0),
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
                    bar::Action::Tray(id) => tray::click(id, false, false, (((x0 + x1) / 2.0) as i32, below)),
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => {
            let x = (lparam.0 & 0xFFFF) as i16 as f32;
            if let Some((bar::Action::Tray(id), x0, x1, mon, bar_h)) = bar::hit_test(hwnd, x) {
                tray::click(id, false, true, (((x0 + x1) / 2.0) as i32, mon.y + bar_h));
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            // Klik kanan ikon tray = menu aplikasinya; di tempat lain = menu HolmBar.
            let x = (lparam.0 & 0xFFFF) as i16 as f32;
            match bar::hit_test(hwnd, x) {
                Some((bar::Action::Tray(id), x0, x1, mon, bar_h)) => tray::click(id, true, false, (((x0 + x1) / 2.0) as i32, mon.y + bar_h)),
                _ => show_menu(hwnd),
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            handle_command(wparam.0 & 0xFFFF);
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

/// Jendela pesan tersembunyi agar instance kedua bisa meminta jendela Pengaturan dibuka.
unsafe extern "system" fn ipc_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_PREVIEW {
        handle_command(IDM_PREVIEW_NOTIF);
        return LRESULT(0);
    }
    if msg == WM_QUIT_APP {
        quit();
        return LRESULT(0);
    }
    if msg == WM_SHOTS {
        shots();
        return LRESULT(0);
    }
    if msg == WM_OPEN_SETTINGS {
        settings::open();
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn create_ipc_window() {
    unsafe {
        let Ok(hmod) = GetModuleHandleW(None) else { return };
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(ipc_wndproc),
            hInstance: hmod.into(),
            lpszClassName: IPC_CLASS,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let _ = CreateWindowExW(WINDOW_EX_STYLE(0), IPC_CLASS, w!("HolmBar IPC"), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(hmod.into()), None);
    }
}

fn register_classes() {
    unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None).unwrap().into();
        let cursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
        let class_bar = wide("HolmBarClass");
        let class_notch = wide("HolmNotchClass");
        let class_dock = wide("HolmDockClass");
        let class_popup = wide("HolmPopupClass");
        for (name, proc) in [
            (&class_popup, Some(popup::wndproc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT)),
            (&class_dock, Some(dock::wndproc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT)),
            (&class_bar, Some(bar_wndproc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT)),
            (&class_notch, Some(notch::wndproc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT)),
        ] {
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                // Klik ganda di bar diteruskan ke ikon tray (beberapa aplikasi membuka jendela dengan klik ganda).
                style: if name == &class_bar { CS_DBLCLKS } else { WNDCLASS_STYLES(0) },
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
        media::WM_MEDIA_CHANGED => {
            notches().into_iter().for_each(notch::on_media);
            popup::on_status_changed();
        }
        status::WM_STATUS_CHANGED => {
            bars().into_iter().for_each(bar::redraw);
            notches().into_iter().for_each(notch::redraw); // cuaca untuk animasi idle
            popup::on_status_changed();
        }
        WM_TIMER if msg.wParam.0 == FS_TIMER.load(Ordering::Relaxed) => update_fullscreen(),
        WM_FOREGROUND => {
            update_fullscreen();
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
        notifications::WM_NOTIFY_TOAST | radios::WM_BT_CONNECTED => {
            let t = unsafe { Box::from_raw(msg.lParam.0 as *mut notifications::Toast) };
            notches().into_iter().for_each(|h| notch::notify(h, (*t).clone()));
        }
        WM_REBUILD => rebuild(),
        tray::WM_TRAY_CHANGED => {
            bars().into_iter().for_each(bar::redraw);
            popup::on_tray_changed();
        }
        WM_HOTKEY => popup::open_launcher(),
        _ => {}
    }
}

fn main() {
    unsafe {
        // Satu instance saja.
        // HOLM_INSTANCE (khusus pengembangan/uji): nama instance terpisah agar uji tidak bentrok dengan instance lain.
        let mutex_name = wide(&format!("Local\\HolmBarSingleton{}", std::env::var("HOLM_INSTANCE").unwrap_or_default()));
        let _guard = CreateMutexW(None, false, PCWSTR(mutex_name.as_ptr()));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            // Sudah berjalan: menjalankan HolmBar lagi membuka jendela Pengaturan.
            if let Ok(h) = FindWindowExW(Some(HWND_MESSAGE), None, IPC_CLASS, PCWSTR::null()) {
                let _ = AllowSetForegroundWindow(ASFW_ANY);
                let has = |f: &str| std::env::args().any(|a| a == f);
                let m = if has("--preview") {
                    WM_PREVIEW
                } else if has("--quit") {
                    WM_QUIT_APP
                } else if has("--shots") {
                    WM_SHOTS
                } else {
                    WM_OPEN_SETTINGS
                };
                let _ = PostMessageW(Some(h), m, WPARAM(0), LPARAM(0));
            }
            return;
        }
        if std::env::args().any(|a| a == "--quit") {
            return; // tidak ada instance yang berjalan
        }
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        MAIN_TID.store(GetCurrentThreadId(), Ordering::Relaxed);

        taskbar::install_panic_guard();
        config::init();
        taskbar::recover();
        canvas::startup();
        register_classes();
        settings::register();
        preview::register();
        menu::register();
        create_ipc_window();

        *CONFIG_STAMP.lock().unwrap() = config::mtime();
        rebuild();

        // Event-driven: jendela aktif, media, volume, Wi-Fi. Timer 15 dtk hanya untuk jam/baterai/config.
        let _hook = SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, None, Some(fg_hook), 0, 0, WINEVENT_OUTOFCONTEXT);
        // DESTROY(0x8001)..HIDE(0x8003) dan CLOAKED(0x8017)..UNCLOAKED(0x8018)
        let _h2 = SetWinEventHook(0x8001, 0x8003, None, Some(dock_hook), 0, 0, WINEVENT_OUTOFCONTEXT);
        let _h3 = SetWinEventHook(0x8017, 0x8018, None, Some(dock_hook), 0, 0, WINEVENT_OUTOFCONTEXT);
        // MOVESIZEEND (0x000B): jendela dipindah ke monitor lain -> Dock monitor itu diperbarui.
        let _h4 = SetWinEventHook(0x000B, 0x000B, None, Some(dock_hook), 0, 0, WINEVENT_OUTOFCONTEXT);
        FS_TIMER.store(SetTimer(None, 0, 600, None), Ordering::Relaxed);
        let tid = GetCurrentThreadId();
        register_hotkey();
        media::start(tid);
        status::start(tid);
        start_notifications();
        radios::watch(tid);
        weather::set_city(&config::current().weather.city);
        weather::start(tid);
        tray::sync();
        apps::refresh();
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
    // Semua yang perlu dipulihkan sudah dipulihkan. Keluar seketika: penutupan normal menunggu thread latar
    // (UI Automation, WinRT) dilepas satu per satu dan bisa makan beberapa detik, sehingga installer/uninstaller
    // mengira HolmBar macet dan file exe masih terkunci.
    unsafe {
        let _ = TerminateProcess(GetCurrentProcess(), 0);
    }
}
