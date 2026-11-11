//! Dock ala macOS untuk aplikasi Windows: ikon aplikasi yang berjalan + yang disematkan,
//! magnifikasi halus saat kursor lewat, label nama, titik penanda "berjalan", dan pantulan
//! saat meluncurkan aplikasi. Perilakunya tetap perilaku Windows (klik = aktifkan/minimize
//! jendela asli); Windows sendiri tidak diubah.
//! Daftar diperbarui lewat event jendela; timer animasi hanya hidup saat ada gerakan.

use crate::canvas::{Align, Canvas, Font, Image};
use crate::config;
use crate::monitors::Mon;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::mem::zeroed;
use std::time::Instant;
use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod};
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const WM_DOCK_REFRESH: u32 = WM_APP + 20;
pub const WM_APPBAR_DOCK: u32 = WM_USER + 102;

const TIMER_ANIM: usize = 1;
const TIMER_POLL: usize = 2;
/// Jeda kursor diam di atas ikon sebelum daftar jendelanya muncul.
const TIMER_HOVER: usize = 3;
const HOVER_MS: u32 = 420;
const FRAME_MS: u32 = 16;
const POLL_MS: u32 = 40;
/// Konstanta waktu peluruhan magnifikasi (detik); lebih kecil = lebih tajam.
const MAG_TAU: f32 = 0.055;
/// Durasi geser muncul/sembunyi (detik).
const SLIDE_SECS: f32 = 0.2;
const BOUNCE_MS: u128 = 1800;
const BOUNCE_GIVEUP_MS: u128 = 8000;

#[derive(Clone, PartialEq)]
struct Item {
    key: String,
    path: String,
    name: String,
    windows: Vec<isize>,
    pinned: bool,
    /// Ikon dari jendela (aplikasi UWP), bukan dari exe.
    icon_hwnd: Option<isize>,
    /// AppUserModelID (cara Windows mencocokkan pin taskbar dengan jendela yang berjalan).
    aumid: String,
}

struct Win {
    mon: Mon,
    mouse_x: Option<f32>,
    cur: Vec<f32>,
    tracking: bool,
    /// Kotak ikon (x, y, w, h) hasil gambar terakhir, untuk hit-test.
    rects: Vec<(f32, f32, f32, f32)>,
    /// Item Dock ini: pin + aplikasi yang jendelanya ada di monitor ini.
    items: Vec<Item>,
    win_w: i32,
    win_h: i32,
    /// Kanvas dipakai ulang antar frame (membuat DIB + GDI+ tiap frame sangat mahal).
    canvas: Option<Canvas>,
    /// Perlu digambar ulang pada tick berikutnya (gerakan mouse hanya menandai, tidak menggambar).
    dirty: bool,
    last: Instant,
    hires: bool,
    /// Saat kursor masuk Dock (ikon masih dilembutkan sebentar) dan waktu gambar terakhir.
    enter_at: Option<Instant>,
    drawn: Instant,
    /// Auto-hide: 1 = tampil penuh, 0 = tersembunyi; `want` = tujuan.
    shown: f32,
    want: bool,
    away_since: Option<Instant>,
}

thread_local! {
    static WINS: RefCell<HashMap<isize, Win>> = RefCell::new(HashMap::new());
    static ITEMS: RefCell<Vec<Item>> = const { RefCell::new(Vec::new()) };
    static ORDER: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static ICONS: RefCell<HashMap<String, Option<Image>>> = RefCell::new(HashMap::new());
    static BOUNCE: RefCell<HashMap<String, Instant>> = RefCell::new(HashMap::new());
    static ACTIVE: Cell<isize> = const { Cell::new(0) };
}

fn with<R>(hwnd: HWND, f: impl FnOnce(&mut Win) -> R) -> Option<R> {
    WINS.with(|w| w.borrow_mut().get_mut(&(hwnd.0 as isize)).map(f))
}

pub fn attach(hwnd: HWND, mon: Mon) {
    WINS.with(|w| {
        w.borrow_mut().insert(hwnd.0 as isize, Win {
                mon,
                mouse_x: None,
                cur: Vec::new(),
                tracking: false,
                rects: Vec::new(),
                items: Vec::new(),
                win_w: 0,
                win_h: 0,
                canvas: None,
                dirty: false,
                last: Instant::now(),
                hires: false,
                enter_at: None,
                drawn: Instant::now(),
                shown: 1.0,
                want: true,
                away_since: None,
            },
        );
    });
}

pub fn detach(hwnd: HWND) {
    let hires = with(hwnd, |w| std::mem::replace(&mut w.hires, false)).unwrap_or(false);
    if hires {
        unsafe {
            timeEndPeriod(1);
        }
    }
    WINS.with(|w| w.borrow_mut().remove(&(hwnd.0 as isize)));
}

// ---------- Pengumpulan jendela / aplikasi ----------

fn wide260(s: &str) -> [u16; 260] {
    let mut a = [0u16; 260];
    for (i, c) in s.encode_utf16().take(259).enumerate() {
        a[i] = c;
    }
    a
}

thread_local! {
    static PATHS: RefCell<HashMap<u32, Option<String>>> = RefCell::new(HashMap::new());
    static AUMIDS: RefCell<HashMap<isize, String>> = RefCell::new(HashMap::new());
}

fn exe_path(hwnd: HWND) -> Option<String> {
    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    if pid == 0 || pid == unsafe { GetCurrentProcessId() } {
        return None;
    }
    if let Some(c) = PATHS.with(|p| p.borrow().get(&pid).cloned()) {
        return c;
    }
    let r = exe_path_uncached(pid);
    PATHS.with(|p| p.borrow_mut().insert(pid, r.clone()));
    r
}

const PKEY_APPUSERMODEL_ID: windows::Win32::Foundation::PROPERTYKEY = windows::Win32::Foundation::PROPERTYKEY { fmtid: windows::core::GUID::from_u128(0x9F4C2855_9F79_4B39_A8D0_E1D42DE1D5F3), pid: 5 };

fn aumid_from_store(store: &windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore) -> String {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
    unsafe {
        let Ok(v) = store.GetValue(&PKEY_APPUSERMODEL_ID) else { return String::new() };
        match PropVariantToStringAlloc(&v) {
            Ok(p) => {
                let s = p.to_string().unwrap_or_default();
                CoTaskMemFree(Some(p.0 as *const _));
                s
            }
            Err(_) => String::new(),
        }
    }
}

/// AppUserModelID jendela (kosong bila tidak ada), di-cache per jendela.
fn window_aumid(hwnd: HWND) -> String {
    let key = hwnd.0 as isize;
    if let Some(c) = AUMIDS.with(|a| a.borrow().get(&key).cloned()) {
        return c;
    }
    use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
    let mut s = unsafe { SHGetPropertyStoreForWindow::<IPropertyStore>(hwnd) }.map(|st| aumid_from_store(&st)).unwrap_or_default().to_lowercase();
    if s.is_empty() {
        // Aplikasi paket non-UWP (WhatsApp, Copilot, ...): AUMID milik prosesnya, agar cocok dengan pin taskbar.
        s = process_aumid(hwnd).to_lowercase();
    }
    AUMIDS.with(|a| a.borrow_mut().insert(key, s.clone()));
    s
}

/// AppUserModelID paket milik proses jendela ini (kosong untuk aplikasi biasa).
fn process_aumid(hwnd: HWND) -> String {
    use windows::Win32::Storage::Packaging::Appx::GetApplicationUserModelId;
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return String::new();
        }
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return String::new() };
        let mut buf = [0u16; 256];
        let mut len = buf.len() as u32;
        let r = GetApplicationUserModelId(h, &mut len, Some(windows::core::PWSTR(buf.as_mut_ptr())));
        let _ = CloseHandle(h);
        if r != ERROR_SUCCESS || len < 2 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..len as usize - 1])
    }
}

fn exe_path_uncached(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 520];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        ok.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

fn class_name(hwnd: HWND) -> String {
    let mut b = [0u16; 128];
    let n = unsafe { GetClassNameW(hwnd, &mut b) };
    String::from_utf16_lossy(&b[..n.max(0) as usize])
}

fn title(hwnd: HWND) -> String {
    let mut b = [0u16; 256];
    let n = unsafe { GetWindowTextW(hwnd, &mut b) };
    String::from_utf16_lossy(&b[..n.max(0) as usize])
}

/// Pengaturan Windows 11 "Pada taskbar, tampilkan semua jendela terbuka": semua desktop (bawaan) atau hanya desktop aktif.
fn all_desktops() -> bool {
    use windows::core::w;
    use windows::Win32::System::Registry::*;
    thread_local! {
        static CACHE: Cell<Option<(Instant, bool)>> = const { Cell::new(None) };
    }
    if let Some((t, v)) = CACHE.with(|c| c.get()) {
        if t.elapsed().as_secs() < 5 {
            return v;
        }
    }
    let v = unsafe {
        let (mut val, mut size) = (0u32, 4u32);
        let r = RegGetValueW(HKEY_CURRENT_USER, w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Advanced"), w!("VirtualDesktopTaskbarFilter"), RRF_RT_REG_DWORD, None, Some(&mut val as *mut u32 as *mut _), Some(&mut size));
        r.is_err() || val == 0
    };
    CACHE.with(|c| c.set(Some((Instant::now(), v))));
    v
}

/// Jendela ini benar-benar berada di desktop virtual lain (bukan jendela hantu aplikasi paket yang di-cloak)?
fn on_other_desktop(hwnd: HWND) -> bool {
    use windows::Win32::System::Com::*;
    thread_local! {
        static VDM: RefCell<Option<IVirtualDesktopManager>> = const { RefCell::new(None) };
    }
    VDM.with(|v| {
        let mut v = v.borrow_mut();
        if v.is_none() {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                *v = CoCreateInstance::<_, IVirtualDesktopManager>(&VirtualDesktopManager, None, CLSCTX_ALL).ok();
            }
        }
        match v.as_ref().map(|m| unsafe { m.IsWindowOnCurrentVirtualDesktop(hwnd) }) {
            Some(Ok(on_current)) => !on_current.as_bool(),
            _ => false,
        }
    })
}

/// Jendela yang akan muncul di taskbar Windows.
fn eligible(hwnd: HWND) -> bool {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || GetWindowTextLengthW(hwnd) == 0 {
            return false;
        }
        if GetWindow(hwnd, GW_OWNER).map(|h| !h.0.is_null()).unwrap_or(false) {
            return false;
        }
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TOOLWINDOW.0 != 0 && ex & WS_EX_APPWINDOW.0 == 0 {
            return false;
        }
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut _, 4);
        // DWM_CLOAKED_SHELL (2) = jendela di desktop virtual lain: taskbar Windows tetap menampilkannya
        // (kecuali "Hanya desktop yang saya pakai" dipilih). Alasan cloak lain (aplikasi/inherited) disembunyikan.
        if cloaked != 0 && !(cloaked == 2 && all_desktops() && on_other_desktop(hwnd)) {
            return false;
        }
        !matches!(class_name(hwnd).as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "HolmBarClass" | "HolmNotchClass" | "HolmDockClass")
    }
}

unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
    let list = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };
    if eligible(hwnd) {
        list.push(hwnd);
    }
    windows::core::BOOL(1)
}

fn pretty_name(path: &str) -> String {
    let stem = std::path::Path::new(path).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let mut c = stem.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or(stem)
}

fn expand_env(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        match rest[i + 1..].find('%') {
            Some(j) => {
                let var = &rest[i + 1..i + 1 + j];
                out.push_str(&std::env::var(var).unwrap_or_else(|_| format!("%{var}%")));
                rest = &rest[i + j + 2..];
            }
            None => {
                out.push_str(&rest[i..]);
                rest = "";
            }
        }
    }
    out + rest
}

const START_KEY: &str = "mb:start";

thread_local! {
    static TB_PINS: RefCell<Option<Vec<Pin>>> = const { RefCell::new(None) };
}

/// Buang cache pin taskbar (dibaca ulang saat Dock dibangun ulang / config dimuat ulang).
pub fn reset_pins() {
    TB_PINS.with(|p| *p.borrow_mut() = None);
}

/// (Target exe, AppUserModelID) sebuah shortcut .lnk. Target kosong untuk aplikasi Store.
fn resolve_lnk(lnk: &std::path::Path) -> (Option<String>, String) {
    use windows::core::Interface;
    use windows::Win32::System::Com::*;
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let Ok(link) = CoCreateInstance::<_, IShellLinkW>(&ShellLink, None, CLSCTX_INPROC_SERVER) else { return (None, String::new()) };
        let Ok(pf) = link.cast::<IPersistFile>() else { return (None, String::new()) };
        let w: Vec<u16> = lnk.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
        if pf.Load(PCWSTR(w.as_ptr()), STGM_READ).is_err() {
            return (None, String::new());
        }
        let aumid = link.cast::<windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore>().map(|st| aumid_from_store(&st)).unwrap_or_default().to_lowercase();
        let mut buf = [0u16; 520];
        let target = link.GetPath(&mut buf, std::ptr::null_mut(), 0).ok().and_then(|_| {
            let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            let s = String::from_utf16_lossy(&buf[..n]);
            (!s.is_empty()).then_some(s)
        });
        (target, aumid)
    }
}

/// Satu pin: path untuk dijalankan/dicocokkan, nama, dan AppUserModelID.
#[derive(Clone)]
struct Pin {
    path: String,
    label: Option<String>,
    aumid: String,
}

thread_local! {
    /// Sidik jari blob pin terakhir yang dibaca, untuk mendeteksi pin/unpin di taskbar Windows.
    static PINS_SIG: Cell<u64> = const { Cell::new(0) };
}

/// Isi `Taskband\Favorites`: daftar pin taskbar yang sebenarnya, termasuk aplikasi Store/paket
/// (yang tidak punya shortcut di folder "User Pinned\TaskBar") dan urutan tampilnya.
fn favorites_blob() -> Option<Vec<u8>> {
    use windows::core::w;
    use windows::Win32::System::Registry::*;
    unsafe {
        let key = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Taskband");
        let mut size = 0u32;
        RegGetValueW(HKEY_CURRENT_USER, key, w!("Favorites"), RRF_RT_REG_BINARY, None, None, Some(&mut size)).ok().ok()?;
        if size == 0 || size > 4 << 20 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        RegGetValueW(HKEY_CURRENT_USER, key, w!("Favorites"), RRF_RT_REG_BINARY, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut size)).ok().ok()?;
        buf.truncate(size as usize);
        Some(buf)
    }
}

fn favorites_sig() -> u64 {
    favorites_blob().map(|b| b.iter().fold(0xcbf29ce484222325u64, |h, x| (h ^ *x as u64).wrapping_mul(0x100000001b3))).unwrap_or(0)
}

/// True bila pin taskbar Windows berubah sejak terakhir dibaca (cache dibuang agar Dock ikut berubah).
fn pins_changed() -> bool {
    if !config::current().dock.import_taskbar_pins || TB_PINS.with(|p| p.borrow().is_none()) {
        return false;
    }
    let changed = favorites_sig() != PINS_SIG.with(|s| s.get());
    if changed {
        reset_pins();
    }
    changed
}

/// Potong blob Favorites menjadi PIDL absolut: tiap entri = 1 byte tipe + u32 panjang + PIDL; 0xFF = akhir.
fn favorites_entries(blob: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 5 <= blob.len() && blob[i] != 0xFF {
        let size = u32::from_le_bytes([blob[i + 1], blob[i + 2], blob[i + 3], blob[i + 4]]) as usize;
        let (start, end) = (i + 5, i + 5 + size);
        if size < 2 || end > blob.len() {
            break;
        }
        out.push(&blob[start..end]);
        i = end;
    }
    out
}

/// Item pertama PIDL = folder virtual shell:AppsFolder {4234d49b-0245-4df3-b780-3893943456e1}?
fn is_apps_folder_pidl(pidl: &[u8]) -> bool {
    const APPS_FOLDER: [u8; 16] = [0x9b, 0xd4, 0x34, 0x42, 0x45, 0x02, 0xf3, 0x4d, 0xb7, 0x80, 0x38, 0x93, 0x94, 0x34, 0x56, 0xe1];
    pidl.len() > 20 && pidl[4..20] == APPS_FOLDER
}

fn pins_from_favorites(blob: &[u8]) -> Vec<Pin> {
    use windows::Win32::System::Com::*;
    use windows::Win32::UI::Shell::Common::ITEMIDLIST;
    let mut out: Vec<Pin> = Vec::new();
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    for entry in favorites_entries(blob) {
        // Salin ke buffer sendiri dengan terminator ganda agar PIDL yang rusak tidak terbaca melewati batas.
        let mut buf = entry.to_vec();
        buf.extend_from_slice(&[0, 0, 0, 0]);
        let name = unsafe {
            match SHGetNameFromIDList(buf.as_ptr() as *const ITEMIDLIST, SIGDN_DESKTOPABSOLUTEPARSING) {
                Ok(p) => {
                    let s = p.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(p.0 as *const _));
                    s
                }
                Err(_) => continue,
            }
        };
        let pin = if name.to_lowercase().ends_with(".lnk") {
            let f = std::path::Path::new(&name);
            if !f.exists() {
                continue;
            }
            let label = f.file_stem().map(|s| s.to_string_lossy().to_string());
            let (target, aumid) = resolve_lnk(f);
            let mut target = target.filter(|t| std::path::Path::new(t).exists());
            if target.is_none() && label.as_deref() == Some("File Explorer") {
                target = std::env::var("SystemRoot").ok().map(|r| format!(r"{r}\explorer.exe"));
            }
            Pin { path: target.unwrap_or(name), label, aumid }
        } else if is_apps_folder_pidl(entry) {
            // Aplikasi Store/paket: nama parsing item shell:AppsFolder = AppUserModelID-nya.
            let id = name.rsplit('\\').next().unwrap_or("");
            if id.is_empty() {
                continue;
            }
            Pin { path: format!("shell:AppsFolder\\{id}"), label: appsfolder_name(id), aumid: id.to_lowercase() }
        } else if std::path::Path::new(&name).exists() {
            Pin { path: name, label: None, aumid: String::new() }
        } else {
            continue;
        };
        if !out.iter().any(|p| p.path.eq_ignore_ascii_case(&pin.path) || (!pin.aumid.is_empty() && p.aumid == pin.aumid)) {
            out.push(pin);
        }
    }
    out
}

/// Aplikasi yang di-pin di taskbar Windows: (path untuk dijalankan/dicocokkan, nama tampilan).
/// Yang punya target .exe memakai exe itu (agar status "berjalan" cocok); aplikasi Store memakai .lnk-nya.
fn taskbar_pins() -> Vec<Pin> {
    if let Some(c) = TB_PINS.with(|p| p.borrow().clone()) {
        return c;
    }
    // Sumber utama: registry Taskband (urutan asli + aplikasi Store). Folder shortcut hanya cadangan.
    let mut out = favorites_blob().map(|b| pins_from_favorites(&b)).unwrap_or_default();
    PINS_SIG.with(|s| s.set(favorites_sig()));
    if !out.is_empty() {
        TB_PINS.with(|p| *p.borrow_mut() = Some(out.clone()));
        return out;
    }
    if let Ok(appdata) = std::env::var("APPDATA") {
        let dir = std::path::Path::new(&appdata).join(r"Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar");
        let mut files: Vec<_> = std::fs::read_dir(dir)
            .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("lnk"))).collect())
            .unwrap_or_default();
        files.sort();
        for f in files {
            let label = f.file_stem().map(|s| s.to_string_lossy().to_string());
            let (target, aumid) = resolve_lnk(&f);
            let mut target = target.filter(|t| std::path::Path::new(t).exists());
            if target.is_none() && label.as_deref() == Some("File Explorer") {
                // Pin bawaan Windows 11 ini tak punya path file (berbasis AppUserModelID).
                target = std::env::var("SystemRoot").ok().map(|r| format!(r"{r}\explorer.exe"));
            }
            out.push(Pin { path: target.unwrap_or_else(|| f.to_string_lossy().to_string()), label, aumid });
        }
    }
    TB_PINS.with(|p| *p.borrow_mut() = Some(out.clone()));
    out
}

fn collect_items() -> Vec<Item> {
    let cfg = config::current();
    let mut hwnds: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(enum_cb), LPARAM(&mut hwnds as *mut _ as isize));
    }

    // Kelompokkan jendela per aplikasi (exe); aplikasi UWP per jendela.
    let mut groups: Vec<Item> = Vec::new();
    for h in hwnds {
        let Some(path) = exe_path(h) else { continue };
        let lower = path.to_lowercase();
        let uwp = lower.ends_with("applicationframehost.exe");
        let aumid = window_aumid(h);
        // Aplikasi UWP: satu kelompok per aplikasi (AUMID), bukan per jendela.
        let key = if uwp && !aumid.is_empty() { format!("uwp:{aumid}") } else if uwp { format!("uwp:{}", h.0 as isize) } else { lower.clone() };
        match groups.iter_mut().find(|g| g.key == key) {
            Some(g) => g.windows.push(h.0 as isize),
            None => groups.push(Item {
                key,
                name: if uwp { title(h) } else { friendly_name(&path) },
                path,
                windows: vec![h.0 as isize],
                pinned: false,
                icon_hwnd: uwp.then_some(h.0 as isize),
                aumid,
            }),
        }
    }

    let mut items: Vec<Item> = Vec::new();
    if cfg.dock.start_button {
        items.push(Item { key: START_KEY.into(), path: String::new(), name: "Windows".into(), windows: vec![], pinned: true, icon_hwnd: None, aumid: String::new() });
    }
    // Pin dari config dulu, lalu pin taskbar Windows (tanpa duplikat).
    let mut pins: Vec<Pin> = cfg
        .dock
        .pinned
        .iter()
        .map(|p| match p.strip_prefix("aumid:") {
            // Aplikasi Store: dijalankan lewat shell:AppsFolder, dicocokkan dengan jendela lewat AUMID.
            Some(id) => Pin { path: format!("shell:AppsFolder\\{id}"), label: appsfolder_name(id), aumid: id.to_lowercase() },
            None => Pin { path: expand_env(p), label: None, aumid: String::new() },
        })
        .collect();
    if cfg.dock.import_taskbar_pins {
        pins.extend(taskbar_pins());
    }
    for Pin { path, label, aumid: pin_aumid } in pins {
        let key = path.to_lowercase();
        if items.iter().any(|i| i.key == key || (!pin_aumid.is_empty() && i.aumid == pin_aumid)) {
            continue;
        }
        let running = groups
            .iter()
            .position(|g| g.key == key || (!pin_aumid.is_empty() && g.aumid == pin_aumid))
            .map(|i| groups.remove(i));
        let mut it = match running {
            Some(mut g) => {
                g.pinned = true;
                g
            }
            None => Item { key, name: if path.to_lowercase().starts_with("shell:appsfolder") { label.clone().unwrap_or_else(|| pretty_name(&path)) } else { friendly_name(&path) }, path, windows: vec![], pinned: true, icon_hwnd: None, aumid: pin_aumid },
        };
        if let Some(l) = label {
            it.name = l;
        }
        items.push(it);
    }

    // Aplikasi berjalan yang tidak disematkan: urutan stabil (urutan kemunculan pertama).
    ORDER.with(|o| {
        let mut o = o.borrow_mut();
        o.retain(|k| groups.iter().any(|g| &g.key == k));
        for g in &groups {
            if !o.contains(&g.key) {
                o.push(g.key.clone());
            }
        }
        for k in o.iter() {
            if let Some(g) = groups.iter().find(|g| &g.key == k) {
                items.push(g.clone());
            }
        }
    });
    items
}

/// Ikon ukuran besar (jumbo, 256 px) dari shell untuk file apa pun, termasuk .lnk.
fn shell_icon(path: &str) -> Option<Image> {
    use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;
    use windows::Win32::UI::Controls::{IImageList, ILD_TRANSPARENT};
    const SHIL_JUMBO: i32 = 4;
    unsafe {
        let mut sfi: SHFILEINFOW = zeroed();
        let r = SHGetFileInfoW(PCWSTR(wide260(path).as_ptr()), FILE_ATTRIBUTE_NORMAL, Some(&mut sfi), std::mem::size_of::<SHFILEINFOW>() as u32, SHGFI_SYSICONINDEX);
        if r == 0 {
            return None;
        }
        let list: IImageList = SHGetImageList(SHIL_JUMBO).ok()?;
        let icon = list.GetIcon(sfi.iIcon, ILD_TRANSPARENT.0 as u32).ok()?;
        let img = Image::from_hicon(icon);
        let _ = DestroyIcon(icon);
        img
    }
}

/// Skor sebuah varian file logo paket (lebih besar = lebih baik); None = jangan dipakai.
/// Meniru taskbar Windows 11: utamakan `altform-unplated` (tanpa plat latar) berukuran `targetsize` terdekat
/// ke 96 px; tema kontras tinggi dan varian untuk latar terang dilewati.
fn logo_variant_score(file_lower: &str) -> Option<i32> {
    if file_lower.contains("contrast-") || file_lower.contains("lightunplated") || file_lower.contains("theme-light") {
        return None;
    }
    let num_after = |key: &str| -> Option<i32> {
        let i = file_lower.find(key)? + key.len();
        let digits: String = file_lower[i..].chars().take_while(|c| c.is_ascii_digit()).collect();
        digits.parse().ok()
    };
    let mut score = 0;
    if file_lower.contains("altform-unplated") {
        score += 10_000;
    }
    if let Some(px) = num_after("targetsize-") {
        // Cukup besar untuk Dock (>= 64) diutamakan; di atas itu, yang paling dekat ke 96.
        score += if px >= 64 { 5_000 - (px - 96).abs().min(400) } else { 1_000 + px };
    } else if let Some(sc) = num_after("scale-") {
        score += 500 + sc.min(400);
    }
    Some(score)
}

/// Logo asli aplikasi paket (Store/MSIX) langsung dari folder instalasinya, tanpa plat latar.
/// None untuk aplikasi desktop biasa atau paket yang tidak menyediakan varian tanpa plat.
fn package_logo(aumid: &str) -> Option<Image> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{IShellItem2, SHCreateItemFromParsingName};
    const PKEY_PACKAGE_INSTALL_PATH: PROPERTYKEY = PROPERTYKEY { fmtid: windows::core::GUID::from_u128(0x9F4C2855_9F79_4B39_A8D0_E1D42DE1D5F3), pid: 15 };
    const PKEY_TILES_SMALL_LOGO_PATH: PROPERTYKEY = PROPERTYKEY { fmtid: windows::core::GUID::from_u128(0x86D40B4D_9069_443C_819A_2A54090DCCEC), pid: 2 };
    let (install, logo) = unsafe {
        let name: Vec<u16> = format!("shell:AppsFolder\\{aumid}").encode_utf16().chain(std::iter::once(0)).collect();
        let item: IShellItem2 = SHCreateItemFromParsingName(PCWSTR(name.as_ptr()), None).ok()?;
        let get = |key: &PROPERTYKEY| -> Option<String> {
            let p = item.GetString(key).ok()?;
            let s = p.to_string().ok();
            CoTaskMemFree(Some(p.0 as *const _));
            s.filter(|s| !s.is_empty())
        };
        (get(&PKEY_PACKAGE_INSTALL_PATH)?, get(&PKEY_TILES_SMALL_LOGO_PATH)?)
    };
    let base = std::path::Path::new(&install).join(&logo);
    let dir = base.parent()?;
    let stem = base.file_stem()?.to_string_lossy().to_lowercase();
    let ext = base.extension()?.to_string_lossy().to_lowercase();
    // Varian bernama "<stem>.<kualifikasi>.<ext>" di folder yang sama (atau "<stem>.<ext>" polos).
    let best = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().to_lowercase();
            let rest = n.strip_prefix(&stem)?.strip_suffix(&ext)?;
            (rest.starts_with('.') && rest.ends_with('.')).then(|| logo_variant_score(&n).map(|s| (s, e.path()))).flatten()
        })
        .max_by_key(|(s, _)| *s)?;
    // Tanpa varian "unplated", logo dirancang untuk plat berwarna: biarkan shell yang menggambarnya.
    if best.0 < 10_000 {
        return None;
    }
    Image::from_bytes(&std::fs::read(best.1).ok()?)
}

/// Ikon aplikasi dari shell:AppsFolder berdasarkan AppUserModelID: logo paket tanpa plat bila ada,
/// selain itu ikon shell ukuran jumbo.
fn appsfolder_icon(aumid: &str) -> Option<Image> {
    use windows::Win32::UI::Controls::{IImageList, ILD_TRANSPARENT};
    const SHIL_JUMBO: i32 = 4;
    if let Some(img) = package_logo(aumid) {
        return Some(img);
    }
    unsafe {
        let name = wide260(&format!("shell:AppsFolder\\{aumid}"));
        let mut pidl: *mut windows::Win32::UI::Shell::Common::ITEMIDLIST = std::ptr::null_mut();
        SHParseDisplayName(PCWSTR(name.as_ptr()), None, &mut pidl, 0, None).ok()?;
        let mut sfi: SHFILEINFOW = zeroed();
        let r = SHGetFileInfoW(PCWSTR(pidl as *const u16), Default::default(), Some(&mut sfi), std::mem::size_of::<SHFILEINFOW>() as u32, SHGFI_PIDL | SHGFI_SYSICONINDEX);
        ILFree(Some(pidl));
        if r == 0 {
            return None;
        }
        let list: IImageList = SHGetImageList(SHIL_JUMBO).ok()?;
        let icon = list.GetIcon(sfi.iIcon, ILD_TRANSPARENT.0 as u32).ok()?;
        let img = Image::from_hicon(icon);
        let _ = DestroyIcon(icon);
        img
    }
}

thread_local! {
    static AUMID_ICONS: RefCell<HashMap<String, Option<Image>>> = RefCell::new(HashMap::new());
}

/// Ikon asli aplikasi (96 px) dari AppUserModelID-nya, di-cache. Dipakai kartu notifikasi di notch.
pub fn aumid_icon(aumid: &str) -> Option<Image> {
    let key = aumid.to_lowercase();
    AUMID_ICONS.with(|c| {
        let mut c = c.borrow_mut();
        if !c.contains_key(&key) {
            let img = appsfolder_icon(aumid).map(|i| i.resized(96).unwrap_or(i));
            c.insert(key.clone(), img);
        }
        c.get(&key).and_then(|i| i.as_ref()).and_then(|i| i.resized(96))
    })
}

/// Nama tampilan aplikasi paket dari shell:AppsFolder.
fn appsfolder_name(aumid: &str) -> Option<String> {
    unsafe {
        let name = wide260(&format!("shell:AppsFolder\\{aumid}"));
        let mut pidl: *mut windows::Win32::UI::Shell::Common::ITEMIDLIST = std::ptr::null_mut();
        SHParseDisplayName(PCWSTR(name.as_ptr()), None, &mut pidl, 0, None).ok()?;
        let mut sfi: SHFILEINFOW = zeroed();
        let r = SHGetFileInfoW(PCWSTR(pidl as *const u16), Default::default(), Some(&mut sfi), std::mem::size_of::<SHFILEINFOW>() as u32, SHGFI_PIDL | SHGFI_DISPLAYNAME);
        ILFree(Some(pidl));
        if r == 0 {
            return None;
        }
        let n = sfi.szDisplayName.iter().position(|&c| c == 0).unwrap_or(sfi.szDisplayName.len());
        let s = String::from_utf16_lossy(&sfi.szDisplayName[..n]);
        (!s.is_empty()).then_some(s)
    }
}

thread_local! {
    static NAMES: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

/// Deskripsi file exe (mis. "Microsoft Edge" untuk msedge.exe), jatuh ke nama file bila tak ada.
pub(crate) fn friendly_name(path: &str) -> String {
    use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
    if let Some(n) = NAMES.with(|c| c.borrow().get(path).cloned()) {
        return n;
    }
    let name = (|| unsafe {
        let wp = wide260(path);
        let size = GetFileVersionInfoSizeW(PCWSTR(wp.as_ptr()), None);
        if size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        GetFileVersionInfoW(PCWSTR(wp.as_ptr()), None, size, buf.as_mut_ptr() as *mut _).ok()?;
        let (mut p, mut len) = (std::ptr::null_mut(), 0u32);
        let tr: Vec<u16> = "\\VarFileInfo\\Translation".encode_utf16().chain(std::iter::once(0)).collect();
        let (lang, cp) = if VerQueryValueW(buf.as_ptr() as *const _, PCWSTR(tr.as_ptr()), &mut p, &mut len).as_bool() && len >= 4 {
            let v = std::slice::from_raw_parts(p as *const u16, 2);
            (v[0], v[1])
        } else {
            (0x0409, 0x04B0)
        };
        let q: Vec<u16> = format!("\\StringFileInfo\\{lang:04x}{cp:04x}\\FileDescription").encode_utf16().chain(std::iter::once(0)).collect();
        if !VerQueryValueW(buf.as_ptr() as *const _, PCWSTR(q.as_ptr()), &mut p, &mut len).as_bool() || len == 0 {
            return None;
        }
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(p as *const u16, len as usize));
        let s = s.trim_matches('\0').trim().to_string();
        (!s.is_empty()).then_some(s)
    })()
    .unwrap_or_else(|| pretty_name(path));
    NAMES.with(|c| c.borrow_mut().insert(path.to_string(), name.clone()));
    name
}

fn load_icon(item: &Item) -> Option<Image> {
    // Aplikasi UWP: ikon sebenarnya dari AppsFolder (ikon jendela ApplicationFrameHost sering generik).
    // Pin Store (.lnk tanpa target exe) memakai AUMID dari shortcut taskbar; yang berjalan memakai AUMID jendela.
    let packaged = item.icon_hwnd.is_some() || item.aumid.contains('!') || item.path.to_lowercase().ends_with(".lnk") || item.path.to_lowercase().starts_with("shell:appsfolder");
    if packaged && !item.aumid.is_empty() {
        if let Some(img) = appsfolder_icon(&item.aumid) {
            return Some(img);
        }
    }
    unsafe {
        if let Some(h) = item.icon_hwnd {
            let hwnd = HWND(h as *mut _);
            let mut res = 0usize;
            SendMessageTimeoutW(hwnd, WM_GETICON, WPARAM(ICON_BIG as usize), LPARAM(0), SMTO_ABORTIFHUNG, 150, Some(&mut res));
            let mut icon = HICON(res as *mut _);
            if icon.0.is_null() {
                icon = HICON(GetClassLongPtrW(hwnd, GCL_HICON) as *mut _);
            }
            if !icon.0.is_null() {
                return Image::from_hicon(icon); // ikon milik jendela: jangan di-destroy
            }
        }
        let mut icons = [HICON::default(); 1];
        let n = PrivateExtractIconsW(&wide260(&item.path), 0, 128, 128, Some(&mut icons), None, 0);
        if n == 0 || icons[0].0.is_null() || n == u32::MAX {
            return shell_icon(&item.path); // mis. shortcut .lnk aplikasi Store
        }
        let img = Image::from_hicon(icons[0]);
        let _ = DestroyIcon(icons[0]);
        img
    }
}

fn ensure_icon(item: &Item) {
    ICONS.with(|c| {
        let mut c = c.borrow_mut();
        if !matches!(c.get(&item.key), Some(Some(_))) {
            // Disimpan sebagai bitmap 96 px: cukup tajam untuk magnifikasi maksimum dan murah digambar.
            let img = load_icon(item).map(|i| i.resized(96).unwrap_or(i));
            c.insert(item.key.clone(), img);
        }
    });
}

// ---------- Geometri ----------

struct Geo {
    icon: f32,
    gap: f32,
    pad: f32,
    mag: f32,
    pill_h: f32,
    win_w: f32,
    win_h: f32,
    s: f32,
}

fn geo(n: usize, mon: &Mon) -> Geo {
    let d = &config::current().dock;
    let s = mon.scale;
    let icon = d.icon_size as f32 * s;
    let (gap, pad, mag) = (8.0 * s, 12.0 * s, d.magnification.max(1.0));
    let pill_h = icon + 2.0 * pad;
    let n = n.max(1) as f32;
    let base = n * icon + (n - 1.0) * gap + 2.0 * pad;
    let extra = 4.2 * (mag - 1.0) * icon;
    Geo {
        icon,
        gap,
        pad,
        mag,
        pill_h,
        win_w: (base + extra + 40.0 * s).min(mon.w as f32),
        win_h: pill_h + icon * (mag - 1.0) + 52.0 * s,
        s,
    }
}

/// Skala target tiap ikon dari posisi kursor (kurva Gauss pada jarak antar-ikon).
fn targets(n: usize, g: &Geo, mouse_x: Option<f32>) -> Vec<f32> {
    let pitch = g.icon + g.gap;
    let base = n as f32 * g.icon + (n as f32 - 1.0).max(0.0) * g.gap + 2.0 * g.pad;
    let left0 = (g.win_w - base) / 2.0 + g.pad;
    (0..n)
        .map(|i| match mouse_x {
            Some(mx) => {
                let d = (mx - (left0 + i as f32 * pitch + g.icon / 2.0)) / pitch;
                1.0 + (g.mag - 1.0) * (-(d * d) / (2.0 * 1.15 * 1.15)).exp()
            }
            None => 1.0,
        })
        .collect()
}

fn bounce_offset(key: &str, s: f32) -> f32 {
    BOUNCE.with(|b| {
        b.borrow().get(key).map(|t| {
            let ms = t.elapsed().as_millis();
            if ms > BOUNCE_MS {
                // setelah satu putaran, diam sampai aplikasi muncul (atau menyerah)
                0.0
            } else {
                let p = ms as f32 / BOUNCE_MS as f32;
                (p * std::f32::consts::PI * 3.0).sin().abs() * (1.0 - p) * 22.0 * s
            }
        })
    })
    .unwrap_or(0.0)
}

fn bouncing() -> bool {
    BOUNCE.with(|b| b.borrow().values().any(|t| t.elapsed().as_millis() < BOUNCE_MS))
}

// ---------- Gambar ----------

thread_local! {
    /// (jumlah frame, total ms, maksimum ms) untuk pengukuran HOLM_PROFILE.
    static PROF: Cell<(u32, f64, f64)> = const { Cell::new((0, 0.0, 0.0)) };
}

/// Catat waktu gambar per frame ke dock-profile.txt bila HOLM_PROFILE diset (alat uji kehalusan).
fn profile(ms: f64) {
    if std::env::var_os("HOLM_PROFILE").is_none() {
        return;
    }
    let (n, total, max) = PROF.with(|p| p.get());
    let (n, total, max) = (n + 1, total + ms, max.max(ms));
    if n >= 60 {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(config::dir().join("dock-profile.txt")) {
            let _ = writeln!(f, "frames=60 avg_ms={:.2} max_ms={:.2}", total / n as f64, max);
        }
        PROF.with(|p| p.set((0, 0.0, 0.0)));
    } else {
        PROF.with(|p| p.set((n, total, max)));
    }
}

pub fn redraw(hwnd: HWND) {
    let t0 = Instant::now();
    redraw_inner(hwnd);
    profile(t0.elapsed().as_secs_f64() * 1000.0);
}

fn redraw_inner(hwnd: HWND) {
    let items = dock_items(hwnd);
    let Some(mon) = with(hwnd, |w| w.mon) else { return };
    let n = items.len();
    let g = geo(n, &mon);
    let cfg = config::current();
    let active = ACTIVE.with(|a| a.get());

    let (cur, mouse_x) = with(hwnd, |w| {
        if w.cur.len() != n {
            w.cur = vec![1.0; n];
        }
        (w.cur.clone(), w.mouse_x)
    })
    .unwrap_or_default();

    let (ww, wh) = (g.win_w.round(), g.win_h.round());
    let mut cached = with(hwnd, |w| w.canvas.take()).flatten();
    if cached.as_ref().map(|c| (c.w, c.h)) != Some((ww as i32, wh as i32)) {
        cached = Canvas::new(ww as i32, wh as i32);
    }
    let Some(c) = cached else { return };
    c.clear();
    if n == 0 {
        c.present(hwnd, Some((mon.x, mon.y)));
        with(hwnd, |w| w.canvas = Some(c));
        return;
    }
    let shown = with(hwnd, |w| w.shown).unwrap_or(1.0);

    // Lebar total mengikuti ukuran ikon yang sedang membesar.
    let sizes: Vec<f32> = cur.iter().map(|k| g.icon * k).collect();
    let total = sizes.iter().sum::<f32>() + (n as f32 - 1.0) * g.gap + 2.0 * g.pad;
    let left = (ww - total) / 2.0;
    let pill_bottom = wh - 4.0 * g.s;
    let pill_top = pill_bottom - g.pill_h;
    let r = 20.0 * g.s;
    let bg = config::color(&cfg.dock.background);

    // Kaca: tepi terang tipis di luar, isi gelap tembus pandang.
    c.fill_round_rect(left, pill_top, total, g.pill_h, (r, r, r, r), 0x44FF_FFFF);
    c.fill_round_rect(left + 1.0, pill_top + 1.0, total - 2.0, g.pill_h - 2.0, (r - 1.0, r - 1.0, r - 1.0, r - 1.0), config::with_alpha(bg, cfg.dock.opacity));

    let mut rects = Vec::with_capacity(n);
    let mut x = left + g.pad;
    let mut hover: Option<usize> = None;
    for (i, it) in items.iter().enumerate() {
        let sz = sizes[i];
        let bounce = bounce_offset(&it.key, g.s);
        let y = pill_bottom - g.pad - sz - bounce;
        if let Some(mx) = mouse_x {
            if mx >= x - g.gap / 2.0 && mx < x + sz + g.gap / 2.0 {
                hover = Some(i);
            }
        }
        if it.key == START_KEY {
            // Tombol Windows: logo Windows 11 asli (4 kotak biru bergradasi, tanpa ubin latar).
            let (side, gap) = (sz * 0.455, sz * 0.07);
            let (ox, oy) = (x + (sz - 2.0 * side - gap) / 2.0, y + (sz - 2.0 * side - gap) / 2.0);
            let cols = [0xFF4CC2FFu32, 0xFF2FA6F7, 0xFF1C8DEF, 0xFF0A6FD1];
            for (k, (dx, dy)) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)].into_iter().enumerate() {
                c.fill_rect(ox + dx * (side + gap), oy + dy * (side + gap), side, side, cols[k]);
            }
        } else {
            ensure_icon(it);
            ICONS.with(|ic| match ic.borrow().get(&it.key) {
                Some(Some(img)) => c.draw_image_fast(img, x, y, sz, sz),
                _ => {
                    c.fill_round_rect(x, y, sz, sz, (sz * 0.22, sz * 0.22, sz * 0.22, sz * 0.22), 0xFF3A3A42);
                    let f = Font::new(&cfg.bar.font, sz * 0.5, true);
                    c.text(&it.name.chars().next().unwrap_or('?').to_string(), &f, (x, y, sz, sz), 0xFFFFFFFF, Align::Center);
                }
            });
        }

        // Titik "berjalan"; lebih terang bila aplikasinya sedang aktif.
        if !it.windows.is_empty() {
            let is_active = it.windows.contains(&active);
            let col = if is_active { 0xFFFFFFFF } else { 0x99FFFFFF };
            let d = 4.0 * g.s;
            c.fill_ellipse(x + sz / 2.0 - d / 2.0, pill_bottom - g.pad / 2.0 - d / 2.0 + 1.0 * g.s, d, d, col);
        }
        rects.push((x, y, sz, sz));
        x += sz + g.gap;
    }

    // Label nama di atas ikon yang disorot.
    if let Some(i) = hover.filter(|_| shown > 0.98) {
        let font = Font::new(&cfg.bar.font, 12.5 * g.s, false);
        let (ix, iy, isz, _) = rects[i];
        let name = &items[i].name;
        let tw = c.measure(name, &font) + 6.0 * g.s;
        let (lw, lh) = (tw + 18.0 * g.s, 26.0 * g.s);
        let lx = (ix + isz / 2.0 - lw / 2.0).clamp(2.0, ww - lw - 2.0);
        let ly = (iy - lh - 8.0 * g.s).max(2.0);
        let lr = 8.0 * g.s;
        c.fill_round_rect(lx, ly, lw, lh, (lr, lr, lr, lr), 0xE6202024);
        c.text(name, &font, (lx, ly, lw, lh), 0xFFFFFFFF, Align::Center);
    }

    // Auto-hide: geser turun melewati tepi bawah layar dengan easing.
    let e = ease(shown);
    let slide = ((1.0 - e) * (wh + 8.0 * g.s)).round() as i32;
    let (px, py) = (mon.x + ((mon.w as f32 - ww) / 2.0).round() as i32, dock_y(hwnd, wh) + slide);
    clip_to_monitor(hwnd, ww as i32, wh as i32, py, mon.y + mon.h);
    c.present(hwnd, Some((px, py)));
    with(hwnd, |w| {
        w.rects = rects;
        w.canvas = Some(c);
    });
}

fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

thread_local! {
    static CLIP: RefCell<HashMap<isize, i32>> = RefCell::new(HashMap::new());
}

/// Saat Dock meluncur turun untuk bersembunyi, bagian yang melewati tepi bawah monitor dipotong agar tidak
/// tampil di monitor tetangga (mis. monitor lain berada di bawahnya).
fn clip_to_monitor(hwnd: HWND, w: i32, h: i32, y: i32, mon_bottom: i32) {
    let visible = (mon_bottom - y).clamp(1, h);
    let key = hwnd.0 as isize;
    if CLIP.with(|c| c.borrow().get(&key).copied()) == Some(visible) {
        return;
    }
    CLIP.with(|c| c.borrow_mut().insert(key, visible));
    unsafe {
        use windows::Win32::Graphics::Gdi::{CreateRectRgn, SetWindowRgn};
        if visible >= h {
            SetWindowRgn(hwnd, None, false);
        } else {
            let rgn = CreateRectRgn(0, 0, w, visible);
            SetWindowRgn(hwnd, Some(rgn), false);
        }
    }
}

/// Posisi y jendela dock: menempel di bawah area yang disisakan AppBar (di atas taskbar bila ada).
fn dock_y(hwnd: HWND, win_h: f32) -> i32 {
    let mon_bottom = with(hwnd, |w| w.mon.y + w.mon.h).unwrap_or(0);
    let bottom = DOCK_BOTTOM.with(|b| b.borrow().get(&(hwnd.0 as isize)).copied()).unwrap_or(mon_bottom);
    bottom - 6 - win_h as i32
}

thread_local! {
    /// Batas bawah (koordinat layar) area yang disisakan AppBar untuk tiap dock.
    static DOCK_BOTTOM: RefCell<HashMap<isize, i32>> = RefCell::new(HashMap::new());
}

// ---------- Layout / AppBar ----------

pub fn layout(hwnd: HWND) {
    let cfg = config::current();
    let Some(mon) = with(hwnd, |w| w.mon) else { return };
    let n = dock_len(hwnd);
    let g = geo(n, &mon);
    // Taskbar asli yang disembunyikan masih menyisakan ruangnya; Dock menempel di tepi bawah layar
    // dan appbar-nya hanya menambah selisihnya, agar tidak ada celah kosong di bawah Dock.
    let replacing = cfg.dock.replace_taskbar && mon.primary;
    // Bila auto-hide taskbar menyala (ruangnya sudah dilepas), Dock menyisakan seluruh bandnya sendiri.
    let tray = if replacing && !crate::taskbar::autohid() { crate::taskbar::thickness() } else { 0 };
    let band = ((g.pill_h + 14.0 * g.s).round() as i32 - tray).max(2);

    let mut bottom = mon.y + mon.h;
    unsafe {
        let mut abd: APPBARDATA = zeroed();
        abd.cbSize = std::mem::size_of::<APPBARDATA>() as u32;
        abd.hWnd = hwnd;
        if cfg.dock.reserve_space && cfg.dock.autohide == "off" {
            abd.uCallbackMessage = WM_APPBAR_DOCK;
            SHAppBarMessage(ABM_NEW, &mut abd);
            abd.uEdge = ABE_BOTTOM;
            abd.rc = RECT { left: mon.x, top: mon.y + mon.h - band, right: mon.x + mon.w, bottom: mon.y + mon.h };
            SHAppBarMessage(ABM_QUERYPOS, &mut abd);
            abd.rc.top = abd.rc.bottom - band;
            SHAppBarMessage(ABM_SETPOS, &mut abd);
            bottom = if replacing { mon.y + mon.h } else { abd.rc.bottom };
        } else {
            SHAppBarMessage(ABM_REMOVE, &mut abd);
        }
    }
    DOCK_BOTTOM.with(|b| b.borrow_mut().insert(hwnd.0 as isize, bottom));
    with(hwnd, |w| {
        w.win_w = g.win_w as i32;
        w.win_h = g.win_h as i32;
    });
    redraw(hwnd);
}

pub fn remove(hwnd: HWND) {
    unsafe {
        let mut abd: APPBARDATA = zeroed();
        abd.cbSize = std::mem::size_of::<APPBARDATA>() as u32;
        abd.hWnd = hwnd;
        SHAppBarMessage(ABM_REMOVE, &mut abd);
    }
    DOCK_BOTTOM.with(|b| b.borrow_mut().remove(&(hwnd.0 as isize)));
}

// ---------- Pembaruan daftar ----------

/// Muat ulang daftar aplikasi. Mengembalikan true bila daftar/aktif berubah (semua dock digambar ulang).
pub fn refresh(docks: &[HWND], fg: Option<HWND>) {
    let active_now = fg.map(|f| f.0 as isize).unwrap_or_else(|| unsafe { GetForegroundWindow() }.0 as isize);
    let active_changed = ACTIVE.with(|a| a.replace(active_now)) != active_now;
    pins_changed(); // pin/unpin di taskbar Windows langsung tercermin di Dock
    let new_items = collect_items();
    // Bounce selesai saat aplikasinya sudah punya jendela (atau menyerah).
    BOUNCE.with(|b| {
        b.borrow_mut().retain(|k, t| {
            t.elapsed().as_millis() < BOUNCE_GIVEUP_MS && !new_items.iter().any(|i| &i.key == k && !i.windows.is_empty())
        })
    });
    if std::env::var_os("HOLM_DEBUG").is_some() {
        let lines: Vec<String> = new_items.iter().map(|i| format!("{} | name={} | aumid={} | path={} | windows={} | pinned={} | uwp_hwnd={}", i.key, i.name, i.aumid, i.path, i.windows.len(), i.pinned, i.icon_hwnd.is_some())).collect();
        let _ = std::fs::write(config::dir().join("dock-items.txt"), lines.join("
"));
    }
    ITEMS.with(|i| *i.borrow_mut() = new_items.clone());
    // Tiap Dock hanya menampilkan jendela yang berada di monitornya (seperti taskbar Windows:
    // "tampilkan aplikasi di taskbar tempat jendelanya terbuka"). Pin hanya di Dock monitor utama.
    let split = docks.len() > 1;
    let mut origin: HashMap<isize, (i32, i32)> = HashMap::new();
    for &d in docks {
        let Some(mon) = with(d, |w| w.mon) else { continue };
        let mine: Vec<Item> = if !split {
            new_items.clone()
        } else {
            new_items
                .iter()
                .filter_map(|it| {
                    let mut it = it.clone();
                    it.windows.retain(|&h| *origin.entry(h).or_insert_with(|| window_monitor(HWND(h as *mut _))) == (mon.x, mon.y));
                    // Pin hanya di Dock monitor utama; monitor lain = tombol Windows + jendela di monitor itu.
                    let keep = !it.windows.is_empty() || it.key == START_KEY || (mon.primary && it.pinned);
                    keep.then_some(it)
                })
                .collect()
        };
        let changed = with(d, |w| {
            let c = w.items != mine;
            w.items = mine;
            c
        })
        .unwrap_or(false);
        if changed {
            layout(d);
        } else if active_changed {
            redraw(d);
        }
        ensure_timer(d);
        ensure_poll(d);
    }
}

/// Sudut kiri-atas monitor tempat jendela berada (jendela yang diminimize: posisi sebelum diminimize).
fn window_monitor(h: HWND) -> (i32, i32) {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
    unsafe {
        let m = MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(m, &mut info).as_bool() {
            (info.rcMonitor.left, info.rcMonitor.top)
        } else {
            (0, 0)
        }
    }
}

/// Daftar item Dock ini (sudah dipisah per monitor).
fn dock_items(hwnd: HWND) -> Vec<Item> {
    with(hwnd, |w| w.items.clone()).unwrap_or_default()
}

fn dock_len(hwnd: HWND) -> usize {
    with(hwnd, |w| w.items.len()).unwrap_or(0)
}

fn animating(hwnd: HWND) -> bool {
    with(hwnd, |w| {
        let n = w.cur.len();
        let g = geo(n, &w.mon);
        let t = targets(n, &g, w.mouse_x);
        let mag = w.cur.iter().zip(t).any(|(c, t)| (c - t).abs() > 0.004);
        let slide = (w.shown - if w.want { 1.0 } else { 0.0 }).abs() > 0.001;
        mag || slide || w.dirty
    })
    .unwrap_or(false)
        || bouncing()
}

/// Nyalakan timer animasi (dan resolusi timer 1 ms agar langkah tidak tersendat 15,6 ms).
fn ensure_timer(hwnd: HWND) {
    if !animating(hwnd) {
        return;
    }
    let start_hires = with(hwnd, |w| {
        w.last = if w.hires { w.last } else { Instant::now() };
        !std::mem::replace(&mut w.hires, true)
    })
    .unwrap_or(false);
    unsafe {
        if start_hires {
            timeBeginPeriod(1);
        }
        SetTimer(Some(hwnd), TIMER_ANIM, FRAME_MS, None);
    }
}

fn stop_timer(hwnd: HWND) {
    let was = with(hwnd, |w| std::mem::replace(&mut w.hires, false)).unwrap_or(false);
    unsafe {
        let _ = KillTimer(Some(hwnd), TIMER_ANIM);
        if was {
            timeEndPeriod(1);
        }
    }
}

fn step(hwnd: HWND) {
    let Some(()) = with(hwnd, |w| {
        let now = Instant::now();
        let dt = now.duration_since(w.last).as_secs_f32().clamp(0.001, 0.08);
        w.last = now;
        let n = w.cur.len();
        let g = geo(n, &w.mon);
        let t = targets(n, &g, w.mouse_x);
        // Di dalam Dock skala ikon mengikuti kursor hampir langsung (gelombang realtime ala macOS);
        // pelembutan hanya saat kursor masuk dan keluar.
        let tau = match (w.mouse_x, w.enter_at) {
            (Some(_), Some(t)) if t.elapsed().as_millis() < 160 => 0.05,
            (Some(_), _) => 0.010,
            _ => MAG_TAU,
        };
        let k = 1.0 - (-dt / tau).exp();
        w.drawn = now;
        for (c, t) in w.cur.iter_mut().zip(t) {
            *c += (t - *c) * k;
            if (*c - t).abs() < 0.002 {
                *c = t;
            }
        }
        let goal = if w.want { 1.0 } else { 0.0 };
        let d = dt / SLIDE_SECS;
        w.shown = if w.shown < goal { (w.shown + d).min(goal) } else { (w.shown - d).max(goal) };
        w.dirty = false;
    }) else {
        return;
    };
    redraw(hwnd);
    if !animating(hwnd) {
        stop_timer(hwnd);
    }
}

// ---------- Auto-hide ----------

fn ensure_poll(hwnd: HWND) {
    let on = config::current().dock.autohide != "off";
    unsafe {
        if on {
            SetTimer(Some(hwnd), TIMER_POLL, POLL_MS, None);
        } else {
            let _ = KillTimer(Some(hwnd), TIMER_POLL);
        }
    }
    if !on {
        let changed = with(hwnd, |w| std::mem::replace(&mut w.want, true) != true).unwrap_or(false);
        if changed {
            ensure_timer(hwnd);
        }
    }
}

/// Kotak pill Dock dalam koordinat layar (tidak bergantung pada posisi geser saat ini).
fn pill_rect(hwnd: HWND) -> Option<(f32, f32, f32, f32)> {
    let (mon, n) = (with(hwnd, |w| w.mon)?, dock_len(hwnd));
    let g = geo(n, &mon);
    let base = n.max(1) as f32 * g.icon + (n.max(1) as f32 - 1.0) * g.gap + 2.0 * g.pad;
    let bottom = DOCK_BOTTOM.with(|b| b.borrow().get(&(hwnd.0 as isize)).copied()).unwrap_or(mon.y + mon.h) as f32 - 6.0 - 4.0 * g.s;
    Some((mon.x as f32 + (mon.w as f32 - base) / 2.0, bottom - g.pill_h, base, g.pill_h))
}

/// Ada jendela aplikasi (tidak diminimize) yang menutupi pill Dock?
fn overlapped(hwnd: HWND) -> bool {
    let Some((px, py, pw, ph)) = pill_rect(hwnd) else { return false };
    let s = with(hwnd, |w| w.mon.scale).unwrap_or(1.0);
    let inset = 3.0 * s;
    let (l, t, r, b) = (px, py + inset, px + pw, py + ph);
    let items = dock_items(hwnd);
    for it in items.iter().filter(|i| i.key != START_KEY) {
        for &h in &it.windows {
            let hw = HWND(h as *mut _);
            unsafe {
                if IsIconic(hw).as_bool() {
                    continue;
                }
                let mut rc = RECT::default();
                if DwmGetWindowAttribute(hw, DWMWA_EXTENDED_FRAME_BOUNDS, &mut rc as *mut RECT as *mut _, std::mem::size_of::<RECT>() as u32).is_err() && GetWindowRect(hw, &mut rc).is_err() {
                    continue;
                }
                if (rc.left as f32) < r && (rc.right as f32) > l && (rc.top as f32) < b && (rc.bottom as f32) > t {
                    return true;
                }
            }
        }
    }
    false
}

thread_local! {
    /// Hitungan tick polling: pekerjaan mahal (penegakan taskbar, cek jendela menutupi) tidak tiap tick.
    static POLL_N: Cell<u32> = const { Cell::new(0) };
    static OVERLAP: RefCell<HashMap<isize, bool>> = RefCell::new(HashMap::new());
}

fn poll(hwnd: HWND) {
    let n = POLL_N.with(|c| {
        let v = c.get().wrapping_add(1);
        c.set(v);
        v
    });
    if n % 25 == 0 {
        // Jendela bisa dipindah antar monitor tanpa event yang kita pantau (mis. Win+Shift+Panah).
        unsafe {
            let _ = PostThreadMessageW(GetCurrentThreadId(), WM_DOCK_REFRESH, WPARAM(0), LPARAM(0));
        }
    }
    let cfg = config::current();
    let mode = cfg.dock.autohide.as_str();
    if mode == "off" {
        ensure_poll(hwnd);
        return;
    }
    let mut pt = POINT::default();
    if unsafe { GetCursorPos(&mut pt) }.is_err() {
        return;
    }
    let Some(mon) = with(hwnd, |w| w.mon) else { return };
    let s = mon.scale;
    let on_mon = pt.x >= mon.x && pt.x < mon.x + mon.w && pt.y >= mon.y && pt.y < mon.y + mon.h;
    // Tepi bawah: beberapa piksel terakhir layar (kursor berhenti di baris terakhir).
    let at_edge = on_mon && pt.y >= mon.y + mon.h - (3.0 * s).ceil().max(2.0) as i32;
    // Kursor di tepi bawah memancing taskbar Windows (auto-hide) muncul: sembunyikan seketika (daftar taskbar di-cache, murah).
    {
        crate::taskbar::enforce();
    }
    let near_dock = on_mon
        && pill_rect(hwnd)
            .map(|(px, py, pw, ph)| {
                let m = 18.0 * s;
                let (x, y) = (pt.x as f32, pt.y as f32);
                x >= px - m && x <= px + pw + m && y >= py - 48.0 * s && y <= py + ph + 40.0 * s
            })
            .unwrap_or(false);
    let busy = crate::menu::is_open() || with(hwnd, |w| w.tracking && w.mouse_x.is_some()).unwrap_or(false);
    // Cek jendela yang menutupi Dock cukup ~5x per detik.
    let key = hwnd.0 as isize;
    let overlap = if mode == "always" {
        true
    } else if n % 4 == 0 || OVERLAP.with(|o| !o.borrow().contains_key(&key)) {
        let v = overlapped(hwnd);
        OVERLAP.with(|o| o.borrow_mut().insert(key, v));
        v
    } else {
        OVERLAP.with(|o| o.borrow().get(&key).copied().unwrap_or(false))
    };
    let delay = std::time::Duration::from_millis(cfg.dock.autohide_delay_ms as u64);

    let changed = with(hwnd, |w| {
        let before = w.want;
        if w.want {
            // Tetap tampil selama kursor di Dock atau masih menempel di tepi bawah.
            if near_dock || at_edge || busy || !overlap {
                w.away_since = None;
            } else {
                let since = *w.away_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= delay {
                    w.want = false;
                    w.away_since = None;
                }
            }
        } else {
            w.away_since = None;
            if at_edge || !overlap {
                w.want = true; // langsung muncul begitu kursor menyentuh tepi bawah
            }
        }
        w.want != before
    })
    .unwrap_or(false);
    if changed {
        with(hwnd, |w| w.last = Instant::now());
        ensure_timer(hwnd);
    }
}

// ---------- Aksi ----------

fn item_at(hwnd: HWND, x: f32, y: f32) -> Option<usize> {
    with(hwnd, |w| {
        let pad = 4.0 * w.mon.scale;
        w.rects.iter().position(|&(rx, ry, rw, rh)| x >= rx - pad && x <= rx + rw + pad && y >= ry - pad && y <= ry + rh + pad + 12.0 * w.mon.scale)
    })
    .flatten()
}

fn activate(item: &Item) {
    unsafe {
        let fg = GetForegroundWindow().0 as isize;
        let ws: Vec<HWND> = item.windows.iter().map(|&h| HWND(h as *mut _)).collect();
        let pos = item.windows.iter().position(|&h| h == fg);
        let target = match pos {
            // Aplikasi sudah aktif: satu jendela -> minimize; banyak -> ganti ke jendela berikutnya.
            Some(_) if ws.len() == 1 => {
                let _ = ShowWindow(ws[0], SW_MINIMIZE);
                return;
            }
            Some(p) => ws[(p + 1) % ws.len()],
            None => ws[0],
        };
        if IsIconic(target).as_bool() {
            let _ = ShowWindow(target, SW_RESTORE);
        }
        let _ = SetForegroundWindow(target);
    }
}

/// Tombol Windows di Dock. Monitor utama: Start Menu Windows asli (tombol Win). Windows 11 hanya mampu membuka
/// Start Menu di monitor utama (ia tidak punya jendela yang bisa dipindah), jadi di monitor tambahan tombol ini
/// membuka pencarian aplikasi HolmBar di monitor itu.
fn open_start_menu(dock: HWND) {
    let primary = with(dock, |w| w.mon.primary).unwrap_or(true);
    if !primary {
        crate::popup::open_launcher();
        return;
    }
    let mk = |up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VK_LWIN, wScan: 0, dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) }, time: 0, dwExtraInfo: 0 } },
    };
    unsafe {
        SendInput(&[mk(false), mk(true)], std::mem::size_of::<INPUT>() as i32);
    }
}

fn launch(item: &Item) {
    let p: Vec<u16> = item.path.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        ShellExecuteW(None, windows::core::w!("open"), PCWSTR(p.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
    BOUNCE.with(|b| b.borrow_mut().insert(item.key.clone(), Instant::now()));
}

/// Entri `dock.pinned` di config untuk item ini (aplikasi Store -> aumid:ID, lainnya -> path exe).
fn pin_entry(item: &Item) -> Option<String> {
    if !item.aumid.is_empty() && (item.icon_hwnd.is_some() || item.aumid.contains('!')) {
        return Some(format!("aumid:{}", item.aumid));
    }
    (!item.path.is_empty() && !item.key.starts_with("uwp:")).then(|| item.path.clone())
}

/// Apakah item ini berasal dari `dock.pinned` di config (bukan dari pin taskbar Windows)?
fn config_pinned(item: &Item) -> bool {
    config::current().dock.pinned.iter().any(|e| entry_matches(e, item))
}

fn entry_matches(entry: &str, item: &Item) -> bool {
    match entry.strip_prefix("aumid:") {
        Some(id) => !item.aumid.is_empty() && item.aumid == id.to_lowercase(),
        None => expand_env(entry).to_lowercase() == item.key,
    }
}

fn item_menu(_owner: HWND, item: &Item) {
    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    let running = !item.windows.is_empty();
    let mut items: Vec<crate::menu::MenuItem> = Vec::new();
    // Seperti macOS: aplikasi dengan beberapa jendela mencantumkannya di atas menu.
    if item.windows.len() > 1 {
        for (i, &h) in item.windows.iter().enumerate().take(12) {
            items.push(crate::menu::MenuItem::new(&window_label(HWND(h as *mut _)), "", 100 + i));
        }
        items.push(crate::menu::MenuItem::sep());
    }
    items.push(crate::menu::MenuItem::new(if running { "Show" } else { "Open" }, "", 1));
    if running {
        items.push(crate::menu::MenuItem::new("Close", "", 2));
    }
    items.push(crate::menu::MenuItem::sep());
    if config_pinned(item) {
        items.push(crate::menu::MenuItem::new("Remove from Dock", "", 3));
    } else if !item.pinned && pin_entry(item).is_some() {
        items.push(crate::menu::MenuItem::new("Keep in Dock", "", 4));
    } else {
        let mut m = crate::menu::MenuItem::new("Pinned in Windows taskbar", "", 0);
        m.disabled = true;
        items.push(m);
    }
    let item = item.clone();
    crate::menu::show(items, pt.x, pt.y, move |cmd| match cmd {
        1 if item.windows.is_empty() => launch(&item),
        1 => activate_first(&item),
        2 => {
            for &h in &item.windows {
                unsafe {
                    let _ = PostMessageW(Some(HWND(h as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            }
        }
        3 => {
            if config::update_pinned(None, |e| entry_matches(e, &item)).is_ok() {
                crate::reload_config();
            }
        }
        4 => {
            if let Some(e) = pin_entry(&item) {
                if config::update_pinned(Some(&e), |_| false).is_ok() {
                    crate::reload_config();
                }
            }
        }
        n if n >= 100 => {
            if let Some(&h) = item.windows.get(n - 100) {
                focus_window(HWND(h as *mut _));
            }
        }
        _ => {}
    });
}

thread_local! {
    /// Ikon yang sedang di-hover (dock dan kunci item), untuk daftar jendela saat hover.
    static HOVER: RefCell<Option<(isize, String)>> = const { RefCell::new(None) };
}

/// Judul jendela untuk daftar (dipotong); awalan "-" untuk yang sedang diminimize.
fn window_label(h: HWND) -> String {
    let t = title(h);
    let t = if t.trim().is_empty() { "(tanpa judul)".to_string() } else { t };
    let t: String = if t.chars().count() > 52 { t.chars().take(51).chain(std::iter::once('\u{2026}')).collect() } else { t };
    if unsafe { IsIconic(h) }.as_bool() { format!("\u{2014} {t}") } else { t }
}

fn focus_window(h: HWND) {
    unsafe {
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        let _ = SetForegroundWindow(h);
    }
}

fn hover_stop(hwnd: HWND) {
    HOVER.with(|h| *h.borrow_mut() = None);
    unsafe {
        let _ = KillTimer(Some(hwnd), TIMER_HOVER);
    }
}

/// Mouse bergerak: bila berpindah ke ikon aplikasi yang punya >= 2 jendela, mulai hitung jeda tampil daftar.
fn hover_track(hwnd: HWND, lparam: LPARAM) {
    let (x, y) = (((lparam.0 & 0xFFFF) as i16) as f32, (((lparam.0 >> 16) & 0xFFFF) as i16) as f32);
    let item = item_at(hwnd, x, y).and_then(|i| dock_items(hwnd).get(i).cloned());
    let key = item.as_ref().filter(|it| !it.windows.is_empty() && it.key != START_KEY).map(|it| it.key.clone());
    let same = HOVER.with(|h| h.borrow().as_ref().map(|(w, k)| *w == hwnd.0 as isize && Some(k) == key.as_ref()).unwrap_or(false));
    if same {
        return;
    }
    hover_stop(hwnd);
    if let Some(k) = key {
        HOVER.with(|h| *h.borrow_mut() = Some((hwnd.0 as isize, k)));
        unsafe {
            SetTimer(Some(hwnd), TIMER_HOVER, HOVER_MS, None);
        }
    }
}

fn hover_fire(hwnd: HWND) {
    unsafe {
        let _ = KillTimer(Some(hwnd), TIMER_HOVER);
    }
    let Some((_, key)) = HOVER.with(|h| h.borrow().clone()) else { return };
    if crate::menu::is_open() {
        return;
    }
    // Masih di atas ikon yang sama?
    let mut pt = POINT::default();
    let mut wr = RECT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
        let _ = GetWindowRect(hwnd, &mut wr);
    }
    let Some(i) = item_at(hwnd, (pt.x - wr.left) as f32, (pt.y - wr.top) as f32) else { return };
    let Some(item) = dock_items(hwnd).get(i).cloned().filter(|it| it.key == key && !it.windows.is_empty()) else { return };
    let Some((rx, ry, rw, _)) = with(hwnd, |w| w.rects.get(i).copied()).flatten() else { return };
    let scale = with(hwnd, |w| w.mon.scale).unwrap_or(1.0);
    let Some(mon) = with(hwnd, |w| w.mon) else { return };
    let icon_top = wr.top + ry as i32;
    let pill_top = pill_rect(hwnd).map(|p| p.1 as i32).unwrap_or(icon_top);
    // Di atas ikon yang dibesarkan dan label namanya, supaya tidak menimpa keduanya.
    let bottom = icon_top.min(pill_top) - (36.0 * scale) as i32;
    crate::preview::show(&item.windows, wr.left + (rx + rw / 2.0) as i32, bottom, mon);
}

fn activate_first(item: &Item) {
    let h = HWND(item.windows[0] as *mut _);
    unsafe {
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        let _ = SetForegroundWindow(h);
    }
}

fn track_leave(hwnd: HWND) {
    if with(hwnd, |w| !std::mem::replace(&mut w.tracking, true)).unwrap_or(false) {
        let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
        unsafe {
            let _ = TrackMouseEvent(&mut tme);
        }
    }
}

pub unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let xy = |l: LPARAM| ((l.0 & 0xFFFF) as i16 as f32, ((l.0 >> 16) & 0xFFFF) as i16 as f32);
    match msg {
        WM_MOUSEMOVE => {
            let (x, _) = xy(lparam);
            with(hwnd, |w| {
                if w.mouse_x.is_none() {
                    w.enter_at = Some(Instant::now());
                }
                w.mouse_x = Some(x);
                w.dirty = true;
            });
            track_leave(hwnd);
            hover_track(hwnd, lparam);
            ensure_timer(hwnd);
            // Gambar langsung dari pesan mouse (dibatasi ~100 fps) agar gelombang terasa realtime;
            // sisanya dilanjutkan timer.
            if with(hwnd, |w| w.drawn.elapsed().as_millis() >= 10).unwrap_or(false) {
                step(hwnd);
            }
            LRESULT(0)
        }
        0x02A3 => {
            hover_stop(hwnd);
            with(hwnd, |w| {
                w.mouse_x = None;
                w.tracking = false;
                w.dirty = true;
            });
            ensure_timer(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = xy(lparam);
            if let Some(i) = item_at(hwnd, x, y) {
                if let Some(it) = dock_items(hwnd).get(i).cloned() {
                    crate::preview::hide();
                    if it.key == START_KEY {
                        open_start_menu(hwnd);
                    } else if it.windows.is_empty() {
                        launch(&it);
                        ensure_timer(hwnd);
                    } else {
                        activate(&it);
                    }
                }
            }
            LRESULT(0)
        }
        WM_MBUTTONUP => {
            // Seperti taskbar: klik tengah membuka jendela/instance baru aplikasi itu.
            let (x, y) = xy(lparam);
            if let Some(it) = item_at(hwnd, x, y).and_then(|i| dock_items(hwnd).get(i).cloned()) {
                if it.key != START_KEY {
                    crate::preview::hide();
                    launch(&it);
                    ensure_timer(hwnd);
                }
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            crate::preview::hide();
            let (x, y) = xy(lparam);
            match item_at(hwnd, x, y).and_then(|i| dock_items(hwnd).get(i).cloned()) {
                Some(it) if it.key == START_KEY => {
                    let mut pt = POINT::default();
                    unsafe {
                        let _ = GetCursorPos(&mut pt);
                    }
                    crate::menus::show_start_menu_up(hwnd, pt.x, pt.y);
                }
                Some(it) => item_menu(hwnd, &it),
                None => crate::show_menu(hwnd),
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_ANIM => {
            step(hwnd);
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_HOVER => {
            hover_fire(hwnd);
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_POLL => {
            poll(hwnd);
            LRESULT(0)
        }
        WM_APPBAR_DOCK => {
            match wparam.0 as u32 {
                ABN_FULLSCREENAPP => crate::set_fullscreen(lparam.0 != 0),
                ABN_POSCHANGED => layout(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_POLL);
            }
            stop_timer(hwnd);
            remove(hwnd);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Nama + ikon aplikasi pemutar media dari AppUserModelID SMTC
/// (mis. "Spotify.exe", "MSEdge", "Chrome", "Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic").
pub fn source_app(src: &str) -> Option<(String, Image)> {
    let l = src.trim().to_lowercase();
    if l.is_empty() {
        return None;
    }
    let stem = l.rsplit(['\\', '/']).next().unwrap_or("").trim_end_matches(".exe").to_string();
    let items = ITEMS.with(|i| i.borrow().clone());
    let stem_of = |p: &str| std::path::Path::new(p).file_stem().map(|s| s.to_string_lossy().to_lowercase());
    if let Some(it) = items.iter().find(|it| it.key != START_KEY && (it.aumid == l || (!stem.is_empty() && stem_of(&it.path).as_deref() == Some(stem.as_str())))) {
        if let Some(img) = load_icon(it) {
            return Some((it.name.clone(), img.resized(64).unwrap_or(img)));
        }
    }
    if l.contains('!') {
        let img = appsfolder_icon(src.trim())?;
        let name = appsfolder_name(src.trim()).unwrap_or_else(|| pretty_name(&stem));
        return Some((name, img.resized(64).unwrap_or(img)));
    }
    let img = crate::popup::app_icon(&stem)?;
    Some((pretty_name(&stem), img))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn favorites_blob_is_split_into_pidls() {
        // Dua entri (panjang 4 dan 2) lalu penanda akhir 0xFF; sisa sesudahnya diabaikan.
        let blob = [0u8, 4, 0, 0, 0, 1, 2, 3, 4, 0, 2, 0, 0, 0, 9, 9, 0xFF, 7, 7];
        let e = favorites_entries(&blob);
        assert_eq!(e, vec![&[1u8, 2, 3, 4][..], &[9u8, 9][..]]);
        // Panjang melebihi blob: berhenti tanpa panik.
        assert!(favorites_entries(&[0, 200, 0, 0, 0, 1, 2]).is_empty());
        assert!(favorites_entries(&[]).is_empty());
    }

    #[test]
    fn apps_folder_pidl_is_detected_by_guid() {
        let mut pidl = vec![0x14, 0x00, 0x1f, 0x80, 0x9b, 0xd4, 0x34, 0x42, 0x45, 0x02, 0xf3, 0x4d, 0xb7, 0x80, 0x38, 0x93, 0x94, 0x34, 0x56, 0xe1];
        pidl.extend_from_slice(&[8, 0, 1, 2, 3, 4, 5, 6, 0, 0]);
        assert!(is_apps_folder_pidl(&pidl));
        pidl[5] ^= 0xFF;
        assert!(!is_apps_folder_pidl(&pidl));
        assert!(!is_apps_folder_pidl(&[0x14, 0x00]));
    }

    #[test]
    fn unplated_logo_near_96px_wins() {
        let s = |n: &str| logo_variant_score(n);
        assert!(s("logo.targetsize-96_altform-unplated.png") > s("logo.targetsize-256_altform-unplated.png"));
        assert!(s("logo.targetsize-256_altform-unplated.png") > s("logo.targetsize-32_altform-unplated.png"));
        assert!(s("logo.targetsize-32_altform-unplated.png") > s("logo.targetsize-96.png"));
        assert!(s("logo.targetsize-96.png") > s("logo.scale-200.png"));
        assert_eq!(s("logo.targetsize-96_altform-lightunplated.png"), None);
        assert_eq!(s("logo.contrast-black_targetsize-96.png"), None);
        assert!(s("logo.scale-200.png").unwrap() < 10_000, "varian berplat tidak dipakai sebagai logo tanpa plat");
    }
}
