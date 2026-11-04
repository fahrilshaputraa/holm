//! Host system tray: ikon notifikasi aplikasi (yang biasanya di pojok kanan taskbar) dipindah ke
//! menu bar HolmBar.
//!
//! Cara kerja (tanpa menyuntik explorer):
//! - `Shell_NotifyIcon` di setiap aplikasi mencari jendela `Shell_TrayWnd` pertama (urutan-z) lalu
//!   mengirim `WM_COPYDATA`. HolmBar membuat jendela tersembunyi ber-class `Shell_TrayWnd` yang
//!   dijaga tetap paling atas, sehingga pesan itu sampai ke sini.
//! - SEMUA `WM_COPYDATA` tetap diteruskan ke tray Explorer (AppBar, ikon, Shell_NotifyIconGetRect),
//!   jadi taskbar/AppBar asli tetap benar dan bila HolmBar mati semuanya kembali seperti biasa.
//! - Saat mulai, pesan `TaskbarCreated` disiarkan agar aplikasi mendaftarkan ulang ikonnya.
//! - Klik di bar diteruskan ke jendela pemilik ikon sesuai versi protokol (NOTIFYICON_VERSION_4).
//!
//! Batasan: aplikasi yang berjalan sebagai Administrator tidak menerima klik dari proses biasa (UIPI).

use crate::canvas::Image;
use crate::config;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Pesan thread ke loop utama: daftar ikon berubah.
pub const WM_TRAY_CHANGED: u32 = WM_APP + 12;

const TIMER_GUARD: usize = 1;
const NIM_ADD: u32 = 0;
const NIM_MODIFY: u32 = 1;
const NIM_DELETE: u32 = 2;
const NIM_SETVERSION: u32 = 4;
const NIF_MESSAGE: u32 = 0x1;
const NIF_ICON: u32 = 0x2;
const NIF_TIP: u32 = 0x4;
const NIF_STATE: u32 = 0x8;
const NIF_GUID: u32 = 0x20;
const NIS_HIDDEN: u32 = 0x1;
const NIN_SELECT: u32 = WM_USER;

/// NOTIFYICONDATAW versi shell: handle dikirim sebagai 32-bit agar sama untuk proses 32/64-bit.
#[repr(C)]
#[derive(Clone, Copy)]
struct Nid32 {
    cb_size: u32,
    hwnd: u32,
    uid: u32,
    flags: u32,
    callback: u32,
    hicon: u32,
    tip: [u16; 128],
    state: u32,
    state_mask: u32,
    info: [u16; 256],
    version: u32,
    info_title: [u16; 64],
    info_flags: u32,
    guid: [u8; 16],
    balloon_icon: u32,
}

pub struct TrayIcon {
    pub id: u64,
    owner: HWND,
    uid: u32,
    guid: Option<[u8; 16]>,
    callback: u32,
    hicon: Option<HICON>,
    pub image: Option<Rc<Image>>,
    pub tip: String,
    version: u32,
    hidden: bool,
    pub exe: String,
}

struct Host {
    hwnd: HWND,
    icons: Vec<TrayIcon>,
    next_id: u64,
}

thread_local! {
    static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
    static REV: Cell<u64> = const { Cell::new(0) };
    static PROMOTED: RefCell<(HashMap<String, bool>, Option<Instant>)> = RefCell::new((HashMap::new(), None));
}

fn with<R>(f: impl FnOnce(&mut Host) -> R) -> Option<R> {
    HOST.with(|h| h.try_borrow_mut().ok().and_then(|mut b| b.as_mut().map(f)))
}

/// Penanda perubahan untuk tanda tangan gambar bar.
pub fn revision() -> u64 {
    REV.with(|r| r.get())
}

fn changed() {
    REV.with(|r| r.set(r.get() + 1));
    unsafe {
        let _ = PostThreadMessageW(GetCurrentThreadId(), WM_TRAY_CHANGED, WPARAM(0), LPARAM(0));
    }
}

fn wanted() -> bool {
    let cfg = config::current();
    cfg.tray.enabled && cfg.bar.enabled && cfg.bar.right.iter().any(|i| i == "tray")
}

fn taskbar_created() -> u32 {
    unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) }
}

/// Nyalakan/matikan host sesuai config (dipanggil saat mulai dan setelah config dimuat ulang).
pub fn sync() {
    let running = with(|h| h.hwnd).is_some();
    match (wanted(), running) {
        (true, false) => start(),
        (false, true) => stop(),
        _ => {}
    }
}

fn start() {
    unsafe {
        let Ok(hmod) = GetModuleHandleW(None) else { return };
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hmod.into(),
            lpszClassName: w!("Shell_TrayWnd"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let Ok(hwnd) = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST, w!("Shell_TrayWnd"), PCWSTR::null(), WS_POPUP, 0, 0, 0, 0, None, None, Some(hmod.into()), None) else {
            return;
        };
        // Aplikasi dengan integritas lebih rendah (sandbox) tetap boleh mengirim ikon.
        let _ = ChangeWindowMessageFilterEx(hwnd, WM_COPYDATA, MSGFLT_ALLOW, None);
        HOST.with(|h| *h.borrow_mut() = Some(Host { hwnd, icons: Vec::new(), next_id: 1 }));
        raise(hwnd);
        SetTimer(Some(hwnd), TIMER_GUARD, 2000, None);
        // Minta semua aplikasi mendaftarkan ulang ikonnya (kini sampai ke host ini dulu).
        let _ = PostMessageW(Some(HWND_BROADCAST), taskbar_created(), WPARAM(0), LPARAM(0));
    }
    changed();
}

pub fn stop() {
    if let Some(host) = HOST.with(|h| h.borrow_mut().take()) {
        for ic in host.icons {
            if let Some(i) = ic.hicon {
                unsafe {
                    let _ = DestroyIcon(i);
                }
            }
        }
        unsafe {
            let _ = DestroyWindow(host.hwnd);
        }
        changed();
    }
}

fn raise(hwnd: HWND) {
    unsafe {
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    }
}

fn exe_of(owner: HWND) -> String {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(owner, Some(&mut pid));
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return String::new() };
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(h);
        if !ok {
            return String::new();
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit(['\\', '/']).next().unwrap_or("").to_lowercase()
    }
}

fn wstr(buf: &[u16]) -> String {
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n])
}

/// Proses satu pesan Shell_NotifyIcon. true = berhasil.
fn handle_notify(msg: u32, nid: &Nid32) -> bool {
    let owner = HWND(nid.hwnd as i32 as isize as *mut _);
    let guid = (nid.flags & NIF_GUID != 0 && nid.guid != [0u8; 16]).then_some(nid.guid);
    let ok = with(|host| {
        let pos = host.icons.iter().position(|i| match (guid, i.guid) {
            (Some(g), Some(h)) => g == h,
            (Some(_), None) | (None, Some(_)) => false,
            (None, None) => i.owner == owner && i.uid == nid.uid,
        });
        match msg {
            NIM_ADD | NIM_MODIFY => {
                let idx = match pos {
                    Some(p) => p,
                    None if msg == NIM_ADD => {
                        let id = host.next_id;
                        host.next_id += 1;
                        host.icons.push(TrayIcon {
                            id,
                            owner,
                            uid: nid.uid,
                            guid,
                            callback: 0,
                            hicon: None,
                            image: None,
                            tip: String::new(),
                            version: 0,
                            hidden: false,
                            exe: exe_of(owner),
                        });
                        host.icons.len() - 1
                    }
                    None => return false,
                };
                let ic = &mut host.icons[idx];
                ic.owner = owner;
                if nid.flags & NIF_MESSAGE != 0 {
                    ic.callback = nid.callback;
                }
                if nid.flags & NIF_ICON != 0 {
                    if let Some(old) = ic.hicon.take() {
                        unsafe {
                            let _ = DestroyIcon(old);
                        }
                    }
                    let src = HICON(nid.hicon as i32 as isize as *mut _);
                    ic.hicon = if src.0.is_null() { None } else { unsafe { CopyIcon(src) }.ok() };
                    ic.image = ic.hicon.and_then(Image::from_hicon).map(|img| {
                        img.lighten_if_dark();
                        Rc::new(img)
                    });
                }
                if nid.flags & NIF_TIP != 0 {
                    ic.tip = wstr(&nid.tip);
                }
                if nid.flags & NIF_STATE != 0 && nid.state_mask & NIS_HIDDEN != 0 {
                    ic.hidden = nid.state & NIS_HIDDEN != 0;
                }
                true
            }
            NIM_DELETE => match pos {
                Some(p) => {
                    let ic = host.icons.remove(p);
                    if let Some(i) = ic.hicon {
                        unsafe {
                            let _ = DestroyIcon(i);
                        }
                    }
                    true
                }
                None => false,
            },
            NIM_SETVERSION => match pos {
                Some(p) => {
                    host.icons[p].version = nid.version;
                    true
                }
                None => false,
            },
            _ => true,
        }
    })
    .unwrap_or(false);
    if ok && msg != NIM_SETVERSION {
        changed();
    }
    ok
}

/// Hapus ikon yang pemiliknya sudah tidak ada (aplikasi crash tanpa NIM_DELETE).
fn prune() {
    let removed = with(|host| {
        let before = host.icons.len();
        host.icons.retain(|i| {
            let alive = unsafe { IsWindow(Some(i.owner)) }.as_bool();
            if !alive {
                if let Some(h) = i.hicon {
                    unsafe {
                        let _ = DestroyIcon(h);
                    }
                }
            }
            alive
        });
        before != host.icons.len()
    })
    .unwrap_or(false);
    if removed {
        changed();
    }
}

// ---------- Pengaturan "tampil di taskbar" milik Windows ----------

/// exe (huruf kecil) -> IsPromoted dari HKCU\Control Panel\NotifyIconSettings (Windows 11).
fn promoted_map() -> HashMap<String, bool> {
    let mut out = HashMap::new();
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, w!("Control Panel\\NotifyIconSettings"), Some(0), KEY_READ, &mut key).is_err() {
            return out;
        }
        let mut i = 0u32;
        loop {
            let mut name = [0u16; 128];
            let mut len = name.len() as u32;
            if RegEnumKeyExW(key, i, Some(PWSTR(name.as_mut_ptr())), &mut len, None, None, None, None).is_err() {
                break;
            }
            i += 1;
            let sub = PCWSTR(name.as_ptr());
            let mut path = [0u16; 520];
            let mut size = (path.len() * 2) as u32;
            if RegGetValueW(key, sub, w!("ExecutablePath"), RRF_RT_REG_SZ, None, Some(path.as_mut_ptr() as *mut _), Some(&mut size)).is_err() {
                continue;
            }
            let mut promoted = 0u32;
            let mut dsize = 4u32;
            let _ = RegGetValueW(key, sub, w!("IsPromoted"), RRF_RT_REG_DWORD, None, Some(&mut promoted as *mut u32 as *mut _), Some(&mut dsize));
            let exe = wstr(&path).rsplit(['\\', '/']).next().unwrap_or("").to_lowercase();
            if !exe.is_empty() {
                let e = out.entry(exe).or_insert(false);
                *e |= promoted != 0;
            }
        }
        let _ = RegCloseKey(key);
    }
    out
}

fn is_promoted(exe: &str) -> bool {
    PROMOTED.with(|p| {
        let mut p = p.borrow_mut();
        if p.1.map(|t| t.elapsed() > Duration::from_secs(10)).unwrap_or(true) {
            p.0 = promoted_map();
            p.1 = Some(Instant::now());
        }
        p.0.get(exe).copied().unwrap_or(false)
    })
}

fn matches(list: &[String], ic: &TrayIcon) -> bool {
    list.iter().any(|p| {
        let p = p.trim().to_lowercase();
        !p.is_empty() && (ic.exe == p || ic.exe.trim_end_matches(".exe") == p.trim_end_matches(".exe") || ic.tip.to_lowercase().contains(&p))
    })
}

/// Tampilan satu ikon untuk bar/popup.
#[derive(Clone)]
pub struct View {
    pub id: u64,
    pub image: Option<Rc<Image>>,
    pub tip: String,
}

/// (ikon langsung di bar, ikon di popup overflow) sesuai config `[tray]`.
pub fn split() -> (Vec<View>, Vec<View>) {
    let cfg = config::current();
    let t = &cfg.tray;
    let Some(all) = with(|host| {
        host.icons
            .iter()
            .filter(|i| !i.hidden && i.image.is_some() && !matches(&t.hidden, i))
            .map(|i| (View { id: i.id, image: i.image.clone(), tip: i.tip.clone() }, matches(&t.pinned, i), i.exe.clone()))
            .collect::<Vec<_>>()
    }) else {
        return (Vec::new(), Vec::new());
    };
    let max = t.max_inline.max(0) as usize;
    let mode = t.inline.to_lowercase();
    let (mut inline, mut overflow) = (Vec::new(), Vec::new());
    // Yang disematkan di config selalu duluan, sesuai urutan daftar pinned.
    let mut ordered: Vec<_> = all.into_iter().collect();
    ordered.sort_by_key(|(_, pinned, _)| !*pinned);
    for (v, pinned, exe) in ordered {
        let want = pinned
            || match mode.as_str() {
                "all" => true,
                "none" => false,
                _ => is_promoted(&exe),
            };
        if want && inline.len() < max {
            inline.push(v);
        } else {
            overflow.push(v);
        }
    }
    (inline, overflow)
}

/// Teruskan klik ke aplikasi pemilik ikon. `anchor` = titik layar untuk menu konteksnya.
pub fn click(id: u64, right: bool, double: bool, anchor: (i32, i32)) {
    let Some((owner, uid, callback, version)) = with(|h| h.icons.iter().find(|i| i.id == id).map(|i| (i.owner, i.uid, i.callback, i.version))).flatten() else { return };
    if callback == 0 {
        return;
    }
    unsafe {
        if !IsWindow(Some(owner)).as_bool() {
            prune();
            return;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(owner, Some(&mut pid));
        let _ = AllowSetForegroundWindow(pid);
        let post = |wp: WPARAM, lp: LPARAM| {
            let _ = PostMessageW(Some(owner), callback, wp, lp);
        };
        let seq: &[u32] = match (right, double) {
            (true, _) => &[WM_RBUTTONDOWN, WM_RBUTTONUP, WM_CONTEXTMENU],
            (false, true) => &[WM_LBUTTONDBLCLK, WM_LBUTTONUP],
            (false, false) => &[WM_LBUTTONDOWN, WM_LBUTTONUP, NIN_SELECT],
        };
        if version >= 4 {
            let wp = WPARAM(((anchor.1 as u16 as usize) << 16) | anchor.0 as u16 as usize);
            for &m in seq {
                post(wp, LPARAM((((uid & 0xFFFF) as isize) << 16) | m as isize));
            }
        } else {
            for &m in seq {
                // Versi lama tidak mengenal NIN_SELECT/WM_CONTEXTMENU kecuali versi 3.
                if version < 3 && (m == NIN_SELECT || m == WM_CONTEXTMENU) {
                    continue;
                }
                post(WPARAM(uid as usize), LPARAM(m as isize));
            }
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COPYDATA => {
            let cds = unsafe { &*(lparam.0 as *const COPYDATASTRUCT) };
            let mut handled = None;
            if cds.dwData == 1 && cds.cbData >= 8 && !cds.lpData.is_null() {
                let bytes = unsafe { std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize) };
                let message = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
                let mut nid: Nid32 = unsafe { std::mem::zeroed() };
                let n = (bytes.len() - 8).min(std::mem::size_of::<Nid32>());
                unsafe { std::ptr::copy_nonoverlapping(bytes[8..].as_ptr(), &mut nid as *mut Nid32 as *mut u8, n) };
                handled = Some(handle_notify(message, &nid));
            }
            // Selalu teruskan ke Explorer (AppBar, ikon, GetRect) agar perilaku asli tetap jalan.
            let mut result = 0usize;
            if let Some(real) = crate::taskbar::shell_tray().filter(|r| *r != hwnd) {
                unsafe {
                    SendMessageTimeoutW(real, WM_COPYDATA, wparam, lparam, SMTO_ABORTIFHUNG, 3000, Some(&mut result));
                }
            }
            match handled {
                Some(ok) => LRESULT(ok as isize),
                None => LRESULT(result as isize),
            }
        }
        WM_TIMER if wparam.0 == TIMER_GUARD => {
            // Explorer bisa naik lagi ke atas urutan-z: rebut kembali posisi pertama lalu sinkron ulang.
            let first = unsafe { FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) }.ok();
            if first != Some(hwnd) {
                raise(hwnd);
                unsafe {
                    let _ = PostMessageW(Some(HWND_BROADCAST), taskbar_created(), WPARAM(0), LPARAM(0));
                }
            }
            prune();
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn nid32_matches_shell_layout() {
        // Ukuran NOTIFYICONDATAW (v6) dengan handle 32-bit seperti yang dikirim shell.
        assert_eq!(std::mem::size_of::<super::Nid32>(), 956);
    }
}
