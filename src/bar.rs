//! Top menu bar: AppBar yang mereservasi ruang di tepi atas layar.

use crate::canvas::{Align, Canvas, Font};
use crate::config::{self, Config};
use crate::monitors::Mon;
use std::cell::RefCell;
use std::collections::HashMap;
use std::mem::zeroed;
use windows::Win32::Foundation::*;
use windows::Win32::System::Power::*;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const WM_APPBAR: u32 = WM_USER + 101;

#[derive(Clone, Copy)]
pub enum Action {
    /// Logo: menu Windows (pencarian, pengaturan, daya).
    StartMenu,
    /// Judul menu aplikasi (File, Edit, ...) menurut indeks di config.
    AppMenu(usize),
    Popup(crate::popup::Kind),
    Uri(&'static str),
    /// Ikon tray aplikasi (id dari tray.rs).
    Tray(u64),
}

struct Hit {
    x0: f32,
    x1: f32,
    action: Action,
}

/// State satu jendela bar (satu per monitor).
struct Win {
    mon: Mon,
    hits: Vec<Hit>,
    sig: String,
    width: i32,
    height: i32,
}

thread_local! {
    static WINS: RefCell<HashMap<isize, Win>> = RefCell::new(HashMap::new());
    /// Nama aplikasi aktif, dibagi semua bar.
    static APP: RefCell<String> = const { RefCell::new(String::new()) };
}

fn with_win<R>(hwnd: HWND, f: impl FnOnce(&mut Win) -> R) -> Option<R> {
    WINS.with(|w| w.borrow_mut().get_mut(&(hwnd.0 as isize)).map(f))
}

pub fn attach(hwnd: HWND, mon: Mon) {
    WINS.with(|w| {
        w.borrow_mut().insert(hwnd.0 as isize, Win { mon, hits: Vec::new(), sig: String::new(), width: mon.w, height: 0 });
    });
}

pub fn detach(hwnd: HWND) {
    WINS.with(|w| w.borrow_mut().remove(&(hwnd.0 as isize)));
}

pub(crate) fn battery() -> Option<(u8, bool)> {
    unsafe {
        let mut s: SYSTEM_POWER_STATUS = zeroed();
        if GetSystemPowerStatus(&mut s).is_err() || s.BatteryFlag & 128 != 0 || s.BatteryLifePercent > 100 {
            return None;
        }
        Some((s.BatteryLifePercent, s.ACLineStatus == 1))
    }
}

fn clock_string(cfg: &Config) -> String {
    let st = unsafe { GetLocalTime() };
    let id = cfg.general.language != "en";
    let (days, months): (&[&str; 7], &[&str; 12]) = if id {
        (
            &["Minggu", "Senin", "Selasa", "Rabu", "Kamis", "Jumat", "Sabtu"],
            &["Januari", "Februari", "Maret", "April", "Mei", "Juni", "Juli", "Agustus", "September", "Oktober", "November", "Desember"],
        )
    } else {
        (
            &["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"],
            &["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"],
        )
    };
    let day = days[st.wDayOfWeek as usize % 7];
    let mon = months[(st.wMonth.max(1) as usize - 1) % 12];
    let short = |s: &str| s.chars().take(3).collect::<String>();

    let mut out = String::new();
    let mut it = cfg.bar.clock_format.chars();
    while let Some(c) = it.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('a') => out += &short(day),
            Some('A') => out += day,
            Some('b') => out += &short(mon),
            Some('B') => out += mon,
            Some('d') => out += &format!("{:02}", st.wDay),
            Some('m') => out += &format!("{:02}", st.wMonth),
            Some('Y') => out += &st.wYear.to_string(),
            Some('y') => out += &format!("{:02}", st.wYear % 100),
            Some('H') => out += &format!("{:02}", st.wHour),
            Some('M') => out += &format!("{:02}", st.wMinute),
            Some('%') => out.push('%'),
            Some(o) => {
                out.push('%');
                out.push(o);
            }
            None => out.push('%'),
        }
    }
    out
}

fn app_name_for(cfg: &Config, hwnd: HWND) -> Option<String> {
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
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        let stem = std::path::Path::new(&path).file_stem()?.to_string_lossy().to_string();
        if stem.eq_ignore_ascii_case("explorer") {
            return Some(cfg.bar.desktop_label.clone());
        }
        // Aplikasi UWP berjalan di dalam ApplicationFrameHost: nama aplikasinya = judul jendelanya.
        if stem.eq_ignore_ascii_case("ApplicationFrameHost") {
            let mut t = [0u16; 128];
            let n = GetWindowTextW(hwnd, &mut t).max(0) as usize;
            let title = String::from_utf16_lossy(&t[..n]);
            if !title.trim().is_empty() {
                return Some(title.trim().to_string());
            }
        }
        // Nama produk dari exe ("Microsoft Edge", bukan "Msedge"); deskripsi yang terlalu panjang diabaikan.
        let nice = crate::dock::friendly_name(&path);
        if !nice.is_empty() && nice.chars().count() <= 32 {
            return Some(nice);
        }
        let mut c = stem.chars();
        Some(c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or(stem))
    }
}

/// Dipanggil saat jendela foreground berubah (event, bukan polling).
/// Mengembalikan true bila nama aplikasi aktif berubah (pemanggil menggambar ulang semua bar).
pub fn on_foreground(fg: HWND) -> bool {
    let cfg = config::current();
    match app_name_for(&cfg, fg) {
        Some(name) => APP.with(|a| {
            LAST_APP.store(fg.0 as isize, std::sync::atomic::Ordering::Relaxed);
            let mut a = a.borrow_mut();
            let changed = *a != name;
            *a = name;
            changed
        }),
        None => false,
    }
}

/// Pasang/perbarui reservasi ruang AppBar di tepi atas monitor milik jendela ini.
pub fn layout(hwnd: HWND) {
    let cfg = config::current();
    let Some(mon) = with_win(hwnd, |w| w.mon) else { return };
    let (w, h) = (mon.w, (cfg.bar.height as f32 * mon.scale).round() as i32);
    with_win(hwnd, |win| {
        win.width = w;
        win.height = h;
    });
    unsafe {
        let mut abd: APPBARDATA = zeroed();
        abd.cbSize = std::mem::size_of::<APPBARDATA>() as u32;
        abd.hWnd = hwnd;
        abd.uCallbackMessage = WM_APPBAR;
        SHAppBarMessage(ABM_NEW, &mut abd);
        abd.uEdge = ABE_TOP;
        abd.rc = RECT { left: mon.x, top: mon.y, right: mon.x + w, bottom: mon.y + h };
        SHAppBarMessage(ABM_QUERYPOS, &mut abd);
        abd.rc.bottom = abd.rc.top + h;
        SHAppBarMessage(ABM_SETPOS, &mut abd);
        let _ = MoveWindow(hwnd, abd.rc.left, abd.rc.top, abd.rc.right - abd.rc.left, h, true);
    }
    with_win(hwnd, |win| win.sig.clear());
    redraw(hwnd);
}

pub fn remove(hwnd: HWND) {
    unsafe {
        let mut abd: APPBARDATA = zeroed();
        abd.cbSize = std::mem::size_of::<APPBARDATA>() as u32;
        abd.hWnd = hwnd;
        SHAppBarMessage(ABM_REMOVE, &mut abd);
    }
}

/// Paksa nama aplikasi aktif yang tampil di bar (untuk tangkapan dokumentasi).
pub fn set_app_name(name: &str) {
    APP.with(|a| *a.borrow_mut() = name.to_string());
}

/// Gambar ulang walau tidak ada yang berubah (mis. untuk tangkapan layar).
pub fn force_redraw(hwnd: HWND) {
    with_win(hwnd, |win| win.sig.clear());
    redraw(hwnd);
}

pub fn redraw(hwnd: HWND) {
    let cfg = config::current();
    let bat = battery();
    let clock = clock_string(&cfg);
    let app = APP.with(|a| a.borrow().clone());
    let Some((w, h, s)) = with_win(hwnd, |win| (win.width, win.height, win.mon.scale)) else { return };
    let vol = crate::status::volume();
    let wifi = crate::status::wifi();
    let sig = format!("{app}|{clock}|{bat:?}|{vol:?}|{wifi:?}|{w}x{h}x{s}|t{}|{}", crate::tray::revision(), cfg.bar.transparent);
    if with_win(hwnd, |win| win.sig == sig).unwrap_or(true) {
        return;
    }
    let Some(c) = Canvas::new(w, h) else { return };

    let transparent = cfg.bar.transparent;
    let bg = if transparent { 0 } else { config::with_alpha(config::color(&cfg.bar.background), cfg.bar.opacity) };
    let text = config::color(&cfg.bar.text);
    let dim = config::color(&cfg.bar.dim_text);
    let fs = cfg.bar.font_size * s;
    let font = Font::new(&cfg.bar.font, fs, false);
    let bold = Font::new(&cfg.bar.font, fs, true);
    let icon = Font::new(&cfg.bar.icon_font, 14.0 * s, false);
    let fh = h as f32;

    if transparent {
        // Tembus pandang: hanya bayangan tipis dari atas agar teks terbaca di wallpaper terang.
        // Alpha minimal 1/255 di seluruh bar: piksel yang sepenuhnya transparan tidak menerima klik.
        c.fill_rect(0.0, 0.0, w as f32, fh, 0x0100_0000);
        c.fill_gradient(0.0, 0.0, w as f32, fh, (0.0, 0.0, 0.0, 0.0), 0x5200_0000, 0x0A00_0000, false);
    } else {
        c.fill_rect(0.0, 0.0, w as f32, fh, bg);
        c.fill_rect(0.0, fh - 1.0, w as f32, 1.0, 0x33FF_FFFF);
    }
    // Teks bar; pada mode transparan diberi bayangan 1 px.
    let label = |t: &str, f: &Font, r: (f32, f32, f32, f32), col: u32| {
        if transparent {
            c.text(t, f, (r.0, r.1 + 1.0 * s, r.2, r.3), 0x9900_0000, Align::Near);
        }
        c.text(t, f, r, col, Align::Near);
    };

    let mut hits: Vec<Hit> = Vec::new();

    // Kiri: logo, nama aplikasi aktif, menu.
    let mut x = 14.0 * s;
    let lw = if cfg.bar.logo == "windows" {
        // Logo Windows vektor: 4 kotak, tajam di semua DPI dan tanpa bergantung pada font.
        let size = 13.0 * s;
        let gap = 1.5 * s;
        let q = (size - gap) / 2.0;
        let y0 = (fh - size) / 2.0;
        for (dx, dy) in [(0.0, 0.0), (q + gap, 0.0), (0.0, q + gap), (q + gap, q + gap)] {
            c.fill_rect(x + dx, y0 + dy, q, q, text);
        }
        size
    } else {
        let lw = c.measure(&cfg.bar.logo, &icon);
        label(&cfg.bar.logo, &icon, (x, 0.0, lw + 4.0, fh), text);
        lw
    };
    hits.push(Hit { x0: x - 8.0 * s, x1: x + lw + 8.0 * s, action: Action::StartMenu });
    x += lw + 14.0 * s;

    let name = if app.is_empty() { cfg.bar.desktop_label.clone() } else { app };
    let nw = c.measure(&name, &bold).min(220.0 * s);
    label(&name, &bold, (x, 0.0, nw + 4.0, fh), text);
    x += nw + 16.0 * s;

    for (i, m) in cfg.bar.menus.iter().enumerate() {
        let mw = c.measure(m, &font);
        label(m, &font, (x, 0.0, mw + 4.0, fh), dim);
        hits.push(Hit { x0: x - 8.0 * s, x1: x + mw + 8.0 * s, action: Action::AppMenu(i) });
        x += mw + 16.0 * s;
    }

    // Kanan: dari ujung kanan ke kiri.
    let mut rx = w as f32 - 14.0 * s;
    let gap = 14.0 * s;
    for item in cfg.bar.right.iter().rev() {
        let (width, action): (f32, Option<Action>) = match item.as_str() {
            "tray" => {
                // Ikon tray: panah overflow paling kanan, ikon langsung di kirinya.
                let (inline, overflow) = crate::tray::split();
                let isz = cfg.tray.icon_size.clamp(10, 20) as f32 * s;
                let igap = 8.0 * s;
                let mut used = 0.0f32;
                if !overflow.is_empty() {
                    let cw = 12.0 * s;
                    crate::icons::draw(&c, crate::icons::Icon::ChevronDown, (rx - cw, (fh - cw) / 2.0, cw, cw), dim, bg);
                    hits.push(Hit { x0: rx - cw - 6.0 * s, x1: rx + gap / 2.0, action: Action::Popup(crate::popup::Kind::Tray) });
                    used = cw + igap;
                }
                for v in inline.iter().rev() {
                    let x = rx - used - isz;
                    if let Some(img) = &v.image {
                        c.draw_image(img, x, (fh - isz) / 2.0, isz, isz, 0.0, 1.0);
                    }
                    hits.push(Hit { x0: x - igap / 2.0, x1: x + isz + igap / 2.0, action: Action::Tray(v.id) });
                    used += isz + igap;
                }
                ((used - igap).max(0.0), None)
            }
            "clock" => {
                let cw = c.measure(&clock, &bold);
                label(&clock, &bold, (rx - cw, 0.0, cw + 4.0, fh), text);
                (cw, Some(Action::Popup(crate::popup::Kind::Calendar)))
            }
            "battery" => match bat {
                Some((pct, charging)) => {
                    let pct_text = format!("{pct}%");
                    let lw = c.measure(&pct_text, &font);
                    label(&pct_text, &font, (rx - lw, 0.0, lw + 4.0, fh), text);
                    let (bw, bh) = (22.0 * s, 11.0 * s);
                    let bx = rx - lw - 6.0 * s - bw - 2.0 * s;
                    let by = (fh - bh) / 2.0;
                    c.fill_round_rect(bx, by, bw, bh, (3.0 * s, 3.0 * s, 3.0 * s, 3.0 * s), 0x55FF_FFFF);
                    let fill = if charging {
                        config::color(&cfg.notch.accent)
                    } else if pct <= 20 {
                        0xFFFF_453A
                    } else {
                        text
                    };
                    let inner = (bw - 3.0 * s) * pct as f32 / 100.0;
                    c.fill_round_rect(bx + 1.5 * s, by + 1.5 * s, inner.max(1.0), bh - 3.0 * s, (2.0 * s, 2.0 * s, 2.0 * s, 2.0 * s), fill);
                    c.fill_round_rect(bx + bw + 0.5 * s, by + 3.5 * s, 2.0 * s, bh - 7.0 * s, (0.0, 1.0 * s, 1.0 * s, 0.0), 0x88FF_FFFF);
                    (lw + 6.0 * s + bw + 2.0 * s, Some(Action::Uri("ms-settings:powersleep")))
                }
                None => (0.0, None), // PC desktop: tidak ada baterai, jangan dipalsukan
            },
            "control_center" => {
                let isz = 16.0 * s;
                let opaque = config::color(&cfg.bar.background) | 0xFF00_0000;
                crate::icons::draw(&c, crate::icons::Icon::Toggles, (rx - isz, (fh - isz) / 2.0, isz, isz), dim, opaque);
                (isz, Some(Action::Popup(crate::popup::Kind::Control)))
            }
            other => {
                use crate::status::Wifi;
                let mut color = dim;
                let (glyph, act) = match other {
                    "wifi" => match wifi {
                        Wifi::Absent => ("", Action::Uri("")),
                        Wifi::Disconnected => {
                            color = config::with_alpha(dim, 0.35);
                            ("\u{f1eb}", Action::Popup(crate::popup::Kind::Wifi))
                        }
                        Wifi::Connected { quality } => {
                            let a = if quality >= 66 { 1.0 } else if quality >= 33 { 0.7 } else { 0.5 };
                            color = config::with_alpha(dim, a);
                            ("\u{f1eb}", Action::Popup(crate::popup::Kind::Wifi))
                        }
                    },
                    "volume" => match vol {
                        None => ("", Action::Uri("")),
                        Some(v) if v.muted || v.level <= 0.001 => {
                            color = config::with_alpha(dim, 0.45);
                            ("\u{f026}", Action::Popup(crate::popup::Kind::Volume))
                        }
                        Some(v) if v.level < 0.5 => ("\u{f027}", Action::Popup(crate::popup::Kind::Volume)),
                        Some(_) => ("\u{f028}", Action::Popup(crate::popup::Kind::Volume)),
                    },
                    "bluetooth" => ("\u{f293}", Action::Popup(crate::popup::Kind::Bluetooth)),
                    "control_center" => ("\u{f013}", Action::Uri("ms-settings:")),
                    _ => ("", Action::Uri("")),
                };
                if glyph.is_empty() {
                    (0.0, None)
                } else {
                    let gw = c.measure(glyph, &icon);
                    label(glyph, &icon, (rx - gw, 0.0, gw + 4.0, fh), color);
                    (gw, Some(act))
                }
            }
        };
        if width > 0.0 {
            if let Some(a) = action {
                hits.push(Hit { x0: rx - width - gap / 2.0, x1: rx + gap / 2.0, action: a });
            }
            rx -= width + gap;
        }
    }

    c.present(hwnd, None);
    with_win(hwnd, |win| {
        win.hits = hits;
        win.sig = sig;
    });
}

/// Aksi di posisi x beserta tepi kiri/kanan item (koordinat layar), monitor, dan tinggi bar (px).
pub fn hit_test(hwnd: HWND, x: f32) -> Option<(Action, f32, f32, Mon, i32)> {
    with_win(hwnd, |win| win.hits.iter().find(|h| x >= h.x0 && x <= h.x1).map(|h| (h.action, win.mon.x as f32 + h.x0, win.mon.x as f32 + h.x1, win.mon, win.height))).flatten()
}

static LAST_APP: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

/// Jendela aplikasi (bukan milik kita) yang terakhir aktif; target aksi menu aplikasi.
pub fn last_app_window() -> Option<HWND> {
    match LAST_APP.load(std::sync::atomic::Ordering::Relaxed) {
        0 => None,
        h => Some(HWND(h as *mut _)),
    }
}
