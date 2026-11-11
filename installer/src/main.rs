//! HolmBar-Setup.exe: installer satu file untuk HolmBar.
//!
//! - Memasang per pengguna ke `%LOCALAPPDATA%\Programs\HolmBar` (tanpa hak administrator / UAC).
//! - Membuat shortcut Start Menu, entri "Installed apps" (uninstall), dan opsional jalan otomatis saat login.
//! - Menutup HolmBar yang sedang berjalan dengan rapi (taskbar Windows dipulihkan) sebelum mengganti file.
//! - Salinan installer disimpan sebagai `Uninstall.exe`; `/uninstall` menghapus semuanya kecuali konfigurasi
//!   pengguna di `%APPDATA%\HolmBar`.
//!
//! Argumen: `/S` (senyap, tanpa dialog), `/uninstall`, `/noautostart`, `/norun`.

#![windows_subsystem = "windows"]

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use windows::core::{w, Interface, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Aplikasi yang dipasang, disematkan saat build (lihat build.rs).
const APP: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../target/release/holm-bar.exe"));
const VERSION: &str = env!("CARGO_PKG_VERSION");
const APP_NAME: &str = "HolmBar";
const UNINSTALL_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\HolmBar";
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
/// Pesan "keluar dengan rapi" untuk jendela IPC HolmBar (WM_APP + 14, lihat src/main.rs).
const WM_QUIT_APP: u32 = WM_APP + 14;

/// Jejak langkah ke file bila HOLM_SETUP_LOG diset (diagnosa).
fn trace(msg: &str) {
    if let Some(p) = std::env::var_os("HOLM_SETUP_LOG") {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
            let _ = writeln!(f, "{msg}");
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Teks sesuai bahasa tampilan Windows (Indonesia atau Inggris).
fn tr(id: &'static str, en: &'static str) -> &'static str {
    let lang = unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() };
    if lang & 0x3FF == 0x21 {
        id
    } else {
        en
    }
}

fn install_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("Programs").join(APP_NAME)
}

fn start_menu_link() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("Microsoft\\Windows\\Start Menu\\Programs").join("HolmBar.lnk"))
}

// ---------- Dialog ----------

/// Aktifkan Common Controls v6 (resource manifest #2, lihat build.rs) agar TaskDialog tersedia.
fn activate_visual_styles() {
    use windows::Win32::System::ApplicationInstallationAndServicing::{ActivateActCtx, CreateActCtxW, ACTCTXW};
    const ACTCTX_FLAG_RESOURCE_NAME_VALID: u32 = 0x8;
    const ACTCTX_FLAG_HMODULE_VALID: u32 = 0x80;
    unsafe {
        let Ok(hmod) = GetModuleHandleW(None) else { return };
        let ctx = ACTCTXW { cbSize: std::mem::size_of::<ACTCTXW>() as u32, dwFlags: ACTCTX_FLAG_HMODULE_VALID | ACTCTX_FLAG_RESOURCE_NAME_VALID, hModule: hmod, lpResourceName: PCWSTR(2 as *const u16), ..Default::default() };
        if let Ok(h) = CreateActCtxW(&ctx) {
            let mut cookie = 0usize;
            let _ = ActivateActCtx(Some(h), &mut cookie); // aktif sepanjang umur proses
        }
    }
}

type TaskDialogFn = unsafe extern "system" fn(*const TASKDIALOGCONFIG, *mut i32, *mut i32, *mut windows::core::BOOL) -> windows::core::HRESULT;

/// `TaskDialogIndirect` dicari saat jalan, BUKAN diimpor statis: fungsi itu hanya ada di comctl32 v6, dan
/// impor statis membuat exe gagal dimuat ("Entry Point Not Found") bila manifest v6 tidak aktif.
fn task_dialog_fn() -> Option<TaskDialogFn> {
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    unsafe {
        let lib = LoadLibraryW(w!("comctl32.dll")).ok()?;
        let f = GetProcAddress(lib, windows::core::s!("TaskDialogIndirect"))?;
        Some(std::mem::transmute::<unsafe extern "system" fn() -> isize, TaskDialogFn>(f))
    }
}

/// Dialog bergaya Windows modern. Mengembalikan (tombol utama ditekan, kotak centang dicentang).
/// `ok` kosong = hanya tombol tutup; `check` kosong = tanpa kotak centang.
fn dialog(title: &str, heading: &str, text: &str, ok: &str, check: &str, warn: bool) -> (bool, bool) {
    let (t, h, c, o, v) = (wide(title), wide(heading), wide(text), wide(ok), wide(check));
    let cancel = wide(tr("Batal", "Cancel"));
    let close = wide(tr("Tutup", "Close"));
    unsafe {
        let buttons = [TASKDIALOG_BUTTON { nButtonID: 100, pszButtonText: PCWSTR(o.as_ptr()) }, TASKDIALOG_BUTTON { nButtonID: 2, pszButtonText: PCWSTR(cancel.as_ptr()) }];
        let only_close = [TASKDIALOG_BUTTON { nButtonID: 2, pszButtonText: PCWSTR(close.as_ptr()) }];
        let list: &[TASKDIALOG_BUTTON] = if ok.is_empty() { &only_close } else { &buttons };
        let hmod = GetModuleHandleW(None).unwrap_or_default();
        let mut cfg = TASKDIALOGCONFIG {
            cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
            hInstance: hmod.into(),
            dwFlags: TDF_ALLOW_DIALOG_CANCELLATION | TDF_POSITION_RELATIVE_TO_WINDOW | TDF_SIZE_TO_CONTENT | if check.is_empty() { TASKDIALOG_FLAGS(0) } else { TDF_VERIFICATION_FLAG_CHECKED },
            pszWindowTitle: PCWSTR(t.as_ptr()),
            pszMainInstruction: PCWSTR(h.as_ptr()),
            pszContent: PCWSTR(c.as_ptr()),
            cButtons: list.len() as u32,
            pButtons: list.as_ptr(),
            nDefaultButton: if ok.is_empty() { 2 } else { 100 },
            ..Default::default()
        };
        // Ikon: ikon HolmBar sendiri (resource #1), atau ikon peringatan sistem.
        cfg.Anonymous1.pszMainIcon = if warn { TD_WARNING_ICON } else { PCWSTR(1 as *const u16) };
        if !check.is_empty() {
            cfg.pszVerificationText = PCWSTR(v.as_ptr());
        }
        let (mut pressed, mut checked) = (0i32, windows::core::BOOL(0));
        if let Some(task_dialog) = task_dialog_fn() {
            if task_dialog(&cfg, &mut pressed, std::ptr::null_mut(), &mut checked).is_ok() {
                return (pressed == 100, checked.as_bool());
            }
        }
        // Common Controls v6 tidak tersedia: kotak pesan klasik (kotak centang dianggap dicentang).
        let body = wide(&format!("{heading}\n\n{text}"));
        let style = if ok.is_empty() { MB_OK } else { MB_OKCANCEL } | if warn { MB_ICONWARNING } else { MB_ICONINFORMATION };
        (MessageBoxW(None, PCWSTR(body.as_ptr()), PCWSTR(t.as_ptr()), style) == IDOK && !ok.is_empty(), true)
    }
}

// ---------- Registry ----------

fn reg_set_str(subkey: &str, name: &str, value: &str) -> bool {
    let (k, n, v) = (wide(subkey), wide(name), wide(value));
    unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, PCWSTR(k.as_ptr()), PCWSTR(n.as_ptr()), REG_SZ.0, Some(v.as_ptr() as *const _), (v.len() * 2) as u32).is_ok() }
}

fn reg_set_dword(subkey: &str, name: &str, value: u32) -> bool {
    let (k, n) = (wide(subkey), wide(name));
    unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, PCWSTR(k.as_ptr()), PCWSTR(n.as_ptr()), REG_DWORD.0, Some(&value as *const u32 as *const _), 4).is_ok() }
}

fn reg_delete_value(subkey: &str, name: &str) {
    let (k, n) = (wide(subkey), wide(name));
    unsafe {
        let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, PCWSTR(k.as_ptr()), PCWSTR(n.as_ptr()));
    }
}

fn reg_delete_tree(subkey: &str) {
    let k = wide(subkey);
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(k.as_ptr()));
    }
}

// ---------- Langkah ----------

/// Tutup HolmBar yang sedang berjalan: minta keluar dengan rapi (taskbar dipulihkan), paksa bila tidak merespons.
fn stop_running_app() {
    unsafe {
        let Ok(ipc) = FindWindowExW(Some(HWND_MESSAGE), None, w!("HolmBarIpcClass"), PCWSTR::null()) else { return };
        let mut pid = 0u32;
        GetWindowThreadProcessId(ipc, Some(&mut pid));
        if pid == 0 {
            return;
        }
        let proc = OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, false, pid).ok();
        let _ = PostMessageW(Some(ipc), WM_QUIT_APP, WPARAM(0), LPARAM(0));
        if let Some(h) = proc {
            if WaitForSingleObject(h, 6000) != WAIT_OBJECT_0 {
                // Versi lama tanpa pesan keluar: HolmBar yang baru memulihkan taskbar saat dijalankan.
                let _ = TerminateProcess(h, 0);
                let _ = WaitForSingleObject(h, 2000);
            }
            let _ = CloseHandle(h);
        }
    }
}

/// Tulis file; diulang sebentar bila masih terkunci (proses lama sedang berakhir).
fn write_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut last = None;
    for _ in 0..20 {
        match std::fs::write(path, bytes) {
            Ok(()) => return Ok(()),
            Err(e) => last = Some(e),
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
    Err(last.unwrap())
}

fn create_shortcut(link: &Path, target: &Path) -> bool {
    unsafe {
        let Ok(sl) = CoCreateInstance::<_, IShellLinkW>(&ShellLink, None, CLSCTX_INPROC_SERVER) else { return false };
        let (t, d, dir) = (wide(&target.to_string_lossy()), wide("Menu bar, notch, dan Dock bergaya macOS"), wide(&target.parent().unwrap_or(Path::new(".")).to_string_lossy()));
        let _ = sl.SetPath(PCWSTR(t.as_ptr()));
        let _ = sl.SetDescription(PCWSTR(d.as_ptr()));
        let _ = sl.SetWorkingDirectory(PCWSTR(dir.as_ptr()));
        let Ok(pf) = sl.cast::<IPersistFile>() else { return false };
        let l = wide(&link.to_string_lossy());
        pf.Save(PCWSTR(l.as_ptr()), true).is_ok()
    }
}

fn launch(exe: &Path) {
    let (e, d) = (wide(&exe.to_string_lossy()), wide(&exe.parent().unwrap_or(Path::new(".")).to_string_lossy()));
    unsafe {
        ShellExecuteW(None, w!("open"), PCWSTR(e.as_ptr()), PCWSTR::null(), PCWSTR(d.as_ptr()), SW_SHOWNORMAL);
    }
}

fn install(silent: bool, mut autostart: bool, run: bool) -> i32 {
    let dir = install_dir();
    let title = format!("{APP_NAME} Setup");
    if !silent {
        let text = format!(
            "{}\n{}\n\n{}",
            tr("HolmBar akan dipasang untuk akun Anda di:", "HolmBar will be installed for your account in:"),
            dir.display(),
            tr("Tidak perlu hak administrator. Bila HolmBar sedang berjalan, ia ditutup dulu lalu dijalankan lagi.", "No administrator rights needed. If HolmBar is running it will be closed and started again.")
        );
        let (go, checked) = dialog(
            &title,
            &format!("{} HolmBar {VERSION}", tr("Pasang", "Install")),
            &text,
            tr("Pasang", "Install"),
            tr("Jalankan HolmBar otomatis saat Windows dinyalakan", "Start HolmBar automatically when Windows starts"),
            false,
        );
        if !go {
            return 1;
        }
        autostart = checked;
    }

    trace("install: stop app");
    stop_running_app();
    trace("install: write files");
    let exe = dir.join("HolmBar.exe");
    let uninstaller = dir.join("Uninstall.exe");
    let result = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(&dir)?;
        write_file(&exe, APP)?;
        let me = std::env::current_exe()?;
        if !me.eq(&uninstaller) {
            std::fs::copy(&me, &uninstaller)?;
        }
        Ok(())
    })();
    if let Err(e) = result {
        if !silent {
            dialog(&title, tr("Pemasangan gagal", "Installation failed"), &format!("{}\n\n{e}", dir.display()), "", "", true);
        }
        return 2;
    }

    trace("install: shortcut");
    if let Some(link) = start_menu_link() {
        create_shortcut(&link, &exe);
    }
    trace("install: registry");
    let q = |p: &Path| format!("\"{}\"", p.display());
    reg_set_str(UNINSTALL_KEY, "DisplayName", APP_NAME);
    reg_set_str(UNINSTALL_KEY, "DisplayVersion", VERSION);
    reg_set_str(UNINSTALL_KEY, "Publisher", "HolmBar");
    reg_set_str(UNINSTALL_KEY, "DisplayIcon", &exe.to_string_lossy());
    reg_set_str(UNINSTALL_KEY, "InstallLocation", &dir.to_string_lossy());
    reg_set_str(UNINSTALL_KEY, "UninstallString", &format!("{} /uninstall", q(&uninstaller)));
    reg_set_str(UNINSTALL_KEY, "QuietUninstallString", &format!("{} /uninstall /S", q(&uninstaller)));
    reg_set_str(UNINSTALL_KEY, "URLInfoAbout", "https://github.com/fahrilshaputraa/holm");
    reg_set_dword(UNINSTALL_KEY, "NoModify", 1);
    reg_set_dword(UNINSTALL_KEY, "NoRepair", 1);
    reg_set_dword(UNINSTALL_KEY, "EstimatedSize", ((APP.len() as u64 * 2) / 1024) as u32);
    if autostart {
        reg_set_str(RUN_KEY, APP_NAME, &q(&exe));
    } else {
        reg_delete_value(RUN_KEY, APP_NAME);
    }

    trace("install: launch");
    if run {
        launch(&exe);
    }
    trace("install: done");
    if !silent {
        dialog(
            &title,
            tr("HolmBar terpasang", "HolmBar is installed"),
            tr(
                "HolmBar sudah berjalan. Buka Pengaturan lewat klik kanan pada menu bar, atau jalankan HolmBar sekali lagi dari Start Menu.\n\nUntuk menghapusnya: Settings Windows > Apps > Installed apps > HolmBar.",
                "HolmBar is now running. Open Settings by right-clicking the menu bar, or launch HolmBar again from the Start Menu.\n\nTo remove it: Windows Settings > Apps > Installed apps > HolmBar.",
            ),
            "",
            "",
            false,
        );
    }
    0
}

fn uninstall(silent: bool) -> i32 {
    let dir = install_dir();
    let title = format!("{APP_NAME} Setup");
    if !silent {
        let (go, _) = dialog(
            &title,
            tr("Hapus HolmBar?", "Uninstall HolmBar?"),
            tr("HolmBar akan ditutup dan dihapus dari komputer ini. Taskbar Windows kembali seperti semula.", "HolmBar will be closed and removed from this computer. The Windows taskbar comes back as before."),
            tr("Hapus", "Uninstall"),
            "",
            true,
        );
        if !go {
            return 1;
        }
    }
    stop_running_app();
    reg_delete_value(RUN_KEY, APP_NAME);
    reg_delete_tree(UNINSTALL_KEY);
    if let Some(link) = start_menu_link() {
        let _ = std::fs::remove_file(link);
    }
    // Exe bisa masih terkunci sesaat setelah prosesnya berakhir: ulangi sebentar.
    let exe = dir.join("HolmBar.exe");
    for _ in 0..20 {
        if !exe.exists() || std::fs::remove_file(&exe).is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
    if !silent {
        dialog(
            &title,
            tr("HolmBar sudah dihapus", "HolmBar was removed"),
            tr("Pengaturan Anda di %APPDATA%\\HolmBar tidak dihapus; hapus folder itu sendiri bila tidak diperlukan lagi.", "Your settings in %APPDATA%\\HolmBar were kept; delete that folder yourself if you no longer need them."),
            "",
            "",
            false,
        );
    }
    // Uninstall.exe (file ini) masih terkunci selama berjalan: folder dihapus sesaat setelah proses ini keluar.
    // Hanya folder pemasangan HolmBar sendiri yang boleh dihapus.
    let me = std::env::current_exe().unwrap_or_default();
    if me.parent() == Some(dir.as_path()) && dir.ends_with(Path::new("Programs").join(APP_NAME)) {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = std::process::Command::new("cmd.exe")
            .raw_arg(format!("/c ping -n 3 127.0.0.1 >nul & rmdir /s /q \"{}\"", dir.display()))
            .current_dir(std::env::temp_dir())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
    0
}

fn main() {
    trace("main");
    let args: Vec<String> = std::env::args().skip(1).map(|a| a.to_lowercase()).collect();
    let has = |f: &str| args.iter().any(|a| a == f || a.trim_start_matches(['/', '-']) == f.trim_start_matches('/'));
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    trace("com ok");
    activate_visual_styles();
    let silent = has("/s");
    let code = if has("/uninstall") { uninstall(silent) } else { install(silent, !has("/noautostart"), !has("/norun")) };
    std::process::exit(code);
}
