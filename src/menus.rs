//! Menu native: menu logo Windows (pencarian, pengaturan, daya) dan menu aplikasi
//! File/Edit/View/Window/Help.
//!
//! Aplikasi Windows tidak punya struktur menu seragam seperti macOS, jadi menu aplikasi di sini
//! mengirim pintasan keyboard standar (Ctrl+S, Ctrl+C, F5, ...) ke jendela aplikasi yang aktif.
//! Aplikasi yang tidak mendukung pintasan itu tidak akan bereaksi.

use crate::canvas::wide;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone, Copy)]
enum Act {
    Sep,
    /// Tekan kombinasi tombol di jendela aplikasi aktif.
    Keys(&'static [VIRTUAL_KEY]),
    Show(SHOW_WINDOW_CMD),
    CloseWindow,
    Uri(&'static str),
    Launcher,
    Exec(&'static str, &'static str),
    /// Tanya konfirmasi sebelum menjalankan perintah daya.
    Confirm(&'static str, &'static str, &'static str),
    Settings,
    OpenConfig,
    ReloadConfig,
    QuitApp,
}

struct Item {
    label: &'static str,
    hint: &'static str,
    act: Act,
}

const fn it(label: &'static str, hint: &'static str, act: Act) -> Item {
    Item { label, hint, act }
}

const SEP: Item = Item { label: "", hint: "", act: Act::Sep };

const C: VIRTUAL_KEY = VK_CONTROL;
const SH: VIRTUAL_KEY = VK_SHIFT;
const ALT: VIRTUAL_KEY = VK_MENU;
const PLUS: VIRTUAL_KEY = VK_OEM_PLUS;
const MINUS: VIRTUAL_KEY = VK_OEM_MINUS;

fn start_items() -> Vec<Item> {
    vec![
        it("About This PC", "", Act::Uri("ms-settings:about")),
        SEP,
        it("Search Apps...", "", Act::Launcher),
        it("Settings", "", Act::Uri("ms-settings:")),
        it("File Explorer", "", Act::Exec("explorer.exe", "")),
        it("Task Manager", "", Act::Exec("taskmgr.exe", "")),
        it("Run...", "Win+R", Act::Keys(&[VK_LWIN, VK_R])),
        SEP,
        it("Lock Screen", "", Act::Exec("rundll32.exe", "user32.dll,LockWorkStation")),
        it("Sleep", "", Act::Exec("rundll32.exe", "powrprof.dll,SetSuspendState 0,1,0")),
        it("Sign Out...", "", Act::Confirm("Keluar dari akun Windows sekarang?", "shutdown.exe", "/l")),
        it("Restart...", "", Act::Confirm("Restart komputer sekarang? Pekerjaan yang belum disimpan bisa hilang.", "shutdown.exe", "/r /t 0")),
        it("Shut Down...", "", Act::Confirm("Matikan komputer sekarang? Pekerjaan yang belum disimpan bisa hilang.", "shutdown.exe", "/s /t 0")),
        SEP,
        it("HolmBar Settings...", "", Act::Settings),
        it("Open HolmBar Config", "", Act::OpenConfig),
        it("Reload Config", "", Act::ReloadConfig),
        it("Quit HolmBar", "", Act::QuitApp),
    ]
}

fn app_items(title: &str) -> Vec<Item> {
    match title.to_lowercase().as_str() {
        "file" => vec![
            it("New", "Ctrl+N", Act::Keys(&[C, VK_N])),
            it("Open...", "Ctrl+O", Act::Keys(&[C, VK_O])),
            SEP,
            it("Save", "Ctrl+S", Act::Keys(&[C, VK_S])),
            it("Save As...", "Ctrl+Shift+S", Act::Keys(&[C, SH, VK_S])),
            it("Print...", "Ctrl+P", Act::Keys(&[C, VK_P])),
            SEP,
            it("Close Tab/Window", "Ctrl+W", Act::Keys(&[C, VK_W])),
            it("Quit App", "Alt+F4", Act::Keys(&[ALT, VK_F4])),
        ],
        "edit" => vec![
            it("Undo", "Ctrl+Z", Act::Keys(&[C, VK_Z])),
            it("Redo", "Ctrl+Y", Act::Keys(&[C, VK_Y])),
            SEP,
            it("Cut", "Ctrl+X", Act::Keys(&[C, VK_X])),
            it("Copy", "Ctrl+C", Act::Keys(&[C, VK_C])),
            it("Paste", "Ctrl+V", Act::Keys(&[C, VK_V])),
            it("Select All", "Ctrl+A", Act::Keys(&[C, VK_A])),
            SEP,
            it("Find...", "Ctrl+F", Act::Keys(&[C, VK_F])),
        ],
        "view" => vec![
            it("Reload", "F5", Act::Keys(&[VK_F5])),
            SEP,
            it("Zoom In", "Ctrl++", Act::Keys(&[C, PLUS])),
            it("Zoom Out", "Ctrl+-", Act::Keys(&[C, MINUS])),
            it("Actual Size", "Ctrl+0", Act::Keys(&[C, VK_0])),
            SEP,
            it("Enter Full Screen", "F11", Act::Keys(&[VK_F11])),
        ],
        "window" => vec![
            it("Minimize", "", Act::Show(SW_MINIMIZE)),
            it("Maximize / Restore", "", Act::Show(SW_MAXIMIZE)),
            it("Close Window", "", Act::CloseWindow),
            SEP,
            it("Task View", "Win+Tab", Act::Keys(&[VK_LWIN, VK_TAB])),
            it("Snap Left", "Win+Left", Act::Keys(&[VK_LWIN, VK_LEFT])),
            it("Snap Right", "Win+Right", Act::Keys(&[VK_LWIN, VK_RIGHT])),
        ],
        "help" => vec![
            it("App Help", "F1", Act::Keys(&[VK_F1])),
            SEP,
            it("Windows Support", "", Act::Uri("https://support.microsoft.com/windows")),
        ],
        _ => vec![],
    }
}

fn press(keys: &[VIRTUAL_KEY]) {
    let mk = |vk: VIRTUAL_KEY, up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) }, time: 0, dwExtraInfo: 0 },
        },
    };
    let mut v: Vec<INPUT> = keys.iter().map(|&k| mk(k, false)).collect();
    v.extend(keys.iter().rev().map(|&k| mk(k, true)));
    unsafe {
        SendInput(&v, std::mem::size_of::<INPUT>() as i32);
    }
}

fn shell(file: &str, args: &str, show: SHOW_WINDOW_CMD) {
    let (f, a) = (wide(file), wide(args));
    unsafe {
        ShellExecuteW(None, w!("open"), PCWSTR(f.as_ptr()), PCWSTR(a.as_ptr()), PCWSTR::null(), show);
    }
}

fn run(act: Act, owner: HWND, target: Option<HWND>) {
    // Pastikan jendela aplikasi (bukan bar) yang menerima aksi.
    let focus_target = |t: Option<HWND>| {
        if let Some(t) = t {
            unsafe {
                let _ = SetForegroundWindow(t);
            }
            std::thread::sleep(std::time::Duration::from_millis(90));
        }
    };
    match act {
        Act::Sep => {}
        Act::Keys(k) => {
            focus_target(target);
            press(k);
        }
        Act::Show(cmd) => {
            if let Some(t) = target {
                unsafe {
                    let maxed = IsZoomed(t).as_bool();
                    let c = if cmd == SW_MAXIMIZE && maxed { SW_RESTORE } else { cmd };
                    let _ = ShowWindow(t, c);
                }
            }
        }
        Act::CloseWindow => {
            if let Some(t) = target {
                unsafe {
                    let _ = PostMessageW(Some(t), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            }
        }
        Act::Uri(u) => shell(u, "", SW_SHOWNORMAL),
        Act::Launcher => crate::popup::open_launcher(),
        Act::Exec(f, a) => shell(f, a, SW_HIDE),
        Act::Confirm(q, f, a) => {
            let (qq, cap) = (wide(q), wide("HolmBar"));
            let r = unsafe { MessageBoxW(Some(owner), PCWSTR(qq.as_ptr()), PCWSTR(cap.as_ptr()), MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2 | MB_TOPMOST | MB_SETFOREGROUND) };
            if r == IDYES {
                shell(f, a, SW_HIDE);
            }
        }
        Act::Settings => unsafe {
            let _ = PostMessageW(Some(owner), WM_COMMAND, WPARAM(crate::IDM_SETTINGS), LPARAM(0));
        },
        Act::OpenConfig => unsafe {
            let _ = PostMessageW(Some(owner), WM_COMMAND, WPARAM(crate::IDM_OPEN_CONFIG), LPARAM(0));
        },
        Act::ReloadConfig => unsafe {
            let _ = PostMessageW(Some(owner), WM_COMMAND, WPARAM(crate::IDM_RELOAD), LPARAM(0));
        },
        Act::QuitApp => unsafe {
            let _ = PostMessageW(Some(owner), WM_COMMAND, WPARAM(crate::IDM_EXIT), LPARAM(0));
        },
    }
}

/// Tampilkan menu di (x, y) layar dan jalankan pilihan. `target` = jendela aplikasi aktif.
fn show(owner: HWND, items: Vec<Item>, x: i32, y: i32, target: Option<HWND>, up: bool) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        if items.is_empty() {
            let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, w!("(Tidak ada item)"));
        }
        for (i, item) in items.iter().enumerate() {
            match item.act {
                Act::Sep => {
                    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                }
                _ => {
                    let text = if item.hint.is_empty() { item.label.to_string() } else { format!("{}\t{}", item.label, item.hint) };
                    let t = wide(&text);
                    let _ = AppendMenuW(menu, MF_STRING, i + 1, PCWSTR(t.as_ptr()));
                }
            }
        }
        let _ = SetForegroundWindow(owner);
        let cmd = TrackPopupMenuEx(menu, (TPM_LEFTALIGN | if up { TPM_BOTTOMALIGN } else { TPM_TOPALIGN } | TPM_RETURNCMD).0, x, y, owner, None).0 as usize;
        let _ = DestroyMenu(menu);
        if cmd > 0 && cmd <= items.len() {
            run(items[cmd - 1].act, owner, target);
        }
    }
}

pub fn show_start_menu(owner: HWND, x: i32, y: i32) {
    show(owner, start_items(), x, y, None, false);
}

/// Menu Windows yang terbuka ke atas (dari Dock di tepi bawah layar).
pub fn show_start_menu_up(owner: HWND, x: i32, y: i32) {
    show(owner, start_items(), x, y, None, true);
}

pub fn show_app_menu(owner: HWND, title: &str, x: i32, y: i32, target: Option<HWND>) {
    show(owner, app_items(title), x, y, target, false);
}
