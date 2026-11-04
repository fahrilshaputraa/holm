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
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const WM_DOCK_REFRESH: u32 = WM_APP + 20;
pub const WM_APPBAR_DOCK: u32 = WM_USER + 102;

const TIMER_ANIM: usize = 1;
const FRAME_MS: u32 = 16;
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
}

struct Win {
    mon: Mon,
    mouse_x: Option<f32>,
    cur: Vec<f32>,
    tracking: bool,
    /// Kotak ikon (x, y, w, h) hasil gambar terakhir, untuk hit-test.
    rects: Vec<(f32, f32, f32, f32)>,
    win_w: i32,
    win_h: i32,
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
        w.borrow_mut().insert(hwnd.0 as isize, Win { mon, mouse_x: None, cur: Vec::new(), tracking: false, rects: Vec::new(), win_w: 0, win_h: 0 });
    });
}

pub fn detach(hwnd: HWND) {
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

fn exe_path(hwnd: HWND) -> Option<String> {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 || pid == GetCurrentProcessId() {
            return None;
        }
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
        if cloaked != 0 {
            return false;
        }
        !matches!(class_name(hwnd).as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "HormBarClass" | "HormNotchClass" | "HormDockClass")
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
    static TB_PINS: RefCell<Option<Vec<(String, Option<String>)>>> = const { RefCell::new(None) };
}

/// Buang cache pin taskbar (dibaca ulang saat Dock dibangun ulang / config dimuat ulang).
pub fn reset_pins() {
    TB_PINS.with(|p| *p.borrow_mut() = None);
}

/// Target sebuah shortcut .lnk (kosong untuk aplikasi Store yang tidak punya path file).
fn resolve_lnk(lnk: &std::path::Path) -> Option<String> {
    use windows::core::Interface;
    use windows::Win32::System::Com::*;
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let pf: IPersistFile = link.cast().ok()?;
        let w: Vec<u16> = lnk.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
        pf.Load(PCWSTR(w.as_ptr()), STGM_READ).ok()?;
        let mut buf = [0u16; 520];
        link.GetPath(&mut buf, std::ptr::null_mut(), 0).ok()?;
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let s = String::from_utf16_lossy(&buf[..n]);
        (!s.is_empty()).then_some(s)
    }
}

/// Aplikasi yang di-pin di taskbar Windows: (path untuk dijalankan/dicocokkan, nama tampilan).
/// Yang punya target .exe memakai exe itu (agar status "berjalan" cocok); aplikasi Store memakai .lnk-nya.
fn taskbar_pins() -> Vec<(String, Option<String>)> {
    if let Some(c) = TB_PINS.with(|p| p.borrow().clone()) {
        return c;
    }
    let mut out = Vec::new();
    if let Ok(appdata) = std::env::var("APPDATA") {
        let dir = std::path::Path::new(&appdata).join(r"Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar");
        let mut files: Vec<_> = std::fs::read_dir(dir)
            .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("lnk"))).collect())
            .unwrap_or_default();
        files.sort();
        for f in files {
            let label = f.file_stem().map(|s| s.to_string_lossy().to_string());
            let mut target = resolve_lnk(&f).filter(|t| std::path::Path::new(t).exists());
            if target.is_none() && label.as_deref() == Some("File Explorer") {
                // Pin bawaan Windows 11 ini tak punya path file (berbasis AppUserModelID).
                target = std::env::var("SystemRoot").ok().map(|r| format!(r"{r}\explorer.exe"));
            }
            out.push((target.unwrap_or_else(|| f.to_string_lossy().to_string()), label));
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
        let key = if uwp { format!("uwp:{}", h.0 as isize) } else { lower.clone() };
        match groups.iter_mut().find(|g| g.key == key) {
            Some(g) => g.windows.push(h.0 as isize),
            None => groups.push(Item {
                key,
                name: if uwp { title(h) } else { pretty_name(&path) },
                path,
                windows: vec![h.0 as isize],
                pinned: false,
                icon_hwnd: uwp.then_some(h.0 as isize),
            }),
        }
    }

    let mut items: Vec<Item> = Vec::new();
    if cfg.dock.start_button {
        items.push(Item { key: START_KEY.into(), path: String::new(), name: "Windows".into(), windows: vec![], pinned: true, icon_hwnd: None });
    }
    // Pin dari config dulu, lalu pin taskbar Windows (tanpa duplikat).
    let mut pins: Vec<(String, Option<String>)> = cfg.dock.pinned.iter().map(|p| (expand_env(p), None)).collect();
    if cfg.dock.import_taskbar_pins {
        pins.extend(taskbar_pins());
    }
    for (path, label) in pins {
        let key = path.to_lowercase();
        if items.iter().any(|i| i.key == key) {
            continue;
        }
        let running = groups.iter().position(|g| g.key == key).map(|i| groups.remove(i));
        let mut it = match running {
            Some(mut g) => {
                g.pinned = true;
                g
            }
            None => Item { key, name: pretty_name(&path), path, windows: vec![], pinned: true, icon_hwnd: None },
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

fn load_icon(item: &Item) -> Option<Image> {
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
            let img = load_icon(item);
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

pub fn redraw(hwnd: HWND) {
    let items = ITEMS.with(|i| i.borrow().clone());
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
    let Some(c) = Canvas::new(ww as i32, wh as i32) else { return };
    if n == 0 {
        c.present(hwnd, Some((mon.x, mon.y)));
        return;
    }

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
    let font = Font::new(&cfg.bar.font, 12.5 * g.s, false);
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
            // Tombol Windows: ubin aksen dengan logo 4 kotak (vektor).
            let cr = sz * 0.22;
            c.fill_round_rect(x, y, sz, sz, (cr, cr, cr, cr), config::color(&cfg.general.accent) | 0xFF00_0000);
            let (side, gap) = (sz * 0.2, sz * 0.04);
            let (ox, oy) = (x + (sz - 2.0 * side - gap) / 2.0, y + (sz - 2.0 * side - gap) / 2.0);
            for (dx, dy) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
                c.fill_rect(ox + dx * (side + gap), oy + dy * (side + gap), side, side, 0xFFFF_FFFF);
            }
        } else {
            ensure_icon(it);
            ICONS.with(|ic| match ic.borrow().get(&it.key) {
                Some(Some(img)) => c.draw_image(img, x, y, sz, sz, 0.0, 1.0),
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
    if let Some(i) = hover {
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

    c.present(hwnd, Some((mon.x + ((mon.w as f32 - ww) / 2.0).round() as i32, dock_y(hwnd, wh))));
    with(hwnd, |w| w.rects = rects);
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
    let n = ITEMS.with(|i| i.borrow().len());
    let g = geo(n, &mon);
    // Taskbar asli yang disembunyikan masih menyisakan ruangnya; Dock menempel di tepi bawah layar
    // dan appbar-nya hanya menambah selisihnya, agar tidak ada celah kosong di bawah Dock.
    let replacing = cfg.dock.replace_taskbar && mon.primary;
    let tray = if replacing { crate::taskbar::thickness() } else { 0 };
    let band = ((g.pill_h + 14.0 * g.s).round() as i32 - tray).max(2);

    let mut bottom = mon.y + mon.h;
    unsafe {
        let mut abd: APPBARDATA = zeroed();
        abd.cbSize = std::mem::size_of::<APPBARDATA>() as u32;
        abd.hWnd = hwnd;
        if cfg.dock.reserve_space {
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
    if let Some(f) = fg {
        ACTIVE.with(|a| a.set(f.0 as isize));
    } else {
        ACTIVE.with(|a| a.set(unsafe { GetForegroundWindow() }.0 as isize));
    }
    let new_items = collect_items();
    // Bounce selesai saat aplikasinya sudah punya jendela (atau menyerah).
    BOUNCE.with(|b| {
        b.borrow_mut().retain(|k, t| {
            t.elapsed().as_millis() < BOUNCE_GIVEUP_MS && !new_items.iter().any(|i| &i.key == k && !i.windows.is_empty())
        })
    });
    let changed = ITEMS.with(|i| {
        let mut i = i.borrow_mut();
        let c = *i != new_items;
        *i = new_items;
        c
    });
    for &d in docks {
        if changed {
            layout(d);
        } else {
            redraw(d);
        }
        ensure_timer(d);
    }
}

fn ensure_timer(hwnd: HWND) {
    let moving = with(hwnd, |w| {
        let n = w.cur.len();
        let g = geo(n, &w.mon);
        let t = targets(n, &g, w.mouse_x);
        w.cur.iter().zip(t).any(|(c, t)| (c - t).abs() > 0.004)
    })
    .unwrap_or(false);
    if moving || bouncing() {
        unsafe {
            SetTimer(Some(hwnd), TIMER_ANIM, FRAME_MS, None);
        }
    }
}

fn step(hwnd: HWND) {
    let Some(()) = with(hwnd, |w| {
        let n = w.cur.len();
        let g = geo(n, &w.mon);
        let t = targets(n, &g, w.mouse_x);
        for (c, t) in w.cur.iter_mut().zip(t) {
            *c += (t - *c) * 0.30; // peluruhan eksponensial: halus seperti Dock macOS
            if (*c - t).abs() < 0.002 {
                *c = t;
            }
        }
    }) else {
        return;
    };
    redraw(hwnd);
    let moving = with(hwnd, |w| {
        let n = w.cur.len();
        let g = geo(n, &w.mon);
        let t = targets(n, &g, w.mouse_x);
        w.cur.iter().zip(t).any(|(c, t)| (c - t).abs() > 0.004)
    })
    .unwrap_or(false);
    if !moving && !bouncing() {
        unsafe {
            let _ = KillTimer(Some(hwnd), TIMER_ANIM);
        }
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

fn launch(item: &Item) {
    let p: Vec<u16> = item.path.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        ShellExecuteW(None, windows::core::w!("open"), PCWSTR(p.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
    BOUNCE.with(|b| b.borrow_mut().insert(item.key.clone(), Instant::now()));
}

fn item_menu(owner: HWND, item: &Item) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let _ = AppendMenuW(menu, MF_STRING, 1, if item.windows.is_empty() { windows::core::w!("Open") } else { windows::core::w!("Show") });
        if !item.windows.is_empty() {
            let _ = AppendMenuW(menu, MF_STRING, 2, windows::core::w!("Close"));
        }
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(owner);
        let cmd = TrackPopupMenuEx(menu, (TPM_RIGHTBUTTON | TPM_RETURNCMD).0, pt.x, pt.y, owner, None).0;
        let _ = DestroyMenu(menu);
        match cmd {
            1 if item.windows.is_empty() => launch(item),
            1 => activate_first(item),
            2 => {
                for &h in &item.windows {
                    let _ = PostMessageW(Some(HWND(h as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            }
            _ => {}
        }
    }
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
            with(hwnd, |w| w.mouse_x = Some(x));
            track_leave(hwnd);
            ensure_timer(hwnd);
            redraw(hwnd);
            LRESULT(0)
        }
        0x02A3 => {
            with(hwnd, |w| {
                w.mouse_x = None;
                w.tracking = false;
            });
            ensure_timer(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = xy(lparam);
            if let Some(i) = item_at(hwnd, x, y) {
                if let Some(it) = ITEMS.with(|v| v.borrow().get(i).cloned()) {
                    if it.key == START_KEY {
                        crate::popup::open_launcher();
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
        WM_RBUTTONUP => {
            let (x, y) = xy(lparam);
            match item_at(hwnd, x, y).and_then(|i| ITEMS.with(|v| v.borrow().get(i).cloned())) {
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
        WM_APPBAR_DOCK => {
            match wparam.0 as u32 {
                ABN_FULLSCREENAPP => crate::set_z(if lparam.0 != 0 { HWND_BOTTOM } else { HWND_TOPMOST }),
                ABN_POSCHANGED => layout(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_ANIM);
            }
            remove(hwnd);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
