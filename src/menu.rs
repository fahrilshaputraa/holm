//! Menu konteks dan dialog konfirmasi bergaya macOS (gelap, sudut membulat, sorotan aksen),
//! digambar sendiri dengan `Canvas` menggantikan menu Win32 bawaan yang tampil klasik.
//!
//! Tidak memblokir: `show` membuat jendela lalu kembali; pilihan dikirim lewat callback.
//! Menutup saat kehilangan fokus, Esc, atau setelah memilih. Navigasi: panah atas/bawah, Enter.

use crate::canvas::{Align, Canvas, Font};
use crate::config;
use std::cell::RefCell;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::*;

const CLASS: PCWSTR = w!("HolmMenuClass");
const DIM: u32 = 0xFF9E9EA8;

#[derive(Clone)]
pub struct MenuItem {
    pub label: String,
    pub hint: String,
    pub id: usize,
    pub sep: bool,
    pub disabled: bool,
}

impl MenuItem {
    pub fn new(label: &str, hint: &str, id: usize) -> MenuItem {
        MenuItem { label: label.into(), hint: hint.into(), id, sep: false, disabled: false }
    }
    pub fn sep() -> MenuItem {
        MenuItem { label: String::new(), hint: String::new(), id: 0, sep: true, disabled: false }
    }
}

enum Mode {
    Menu,
    Confirm { lines: Vec<String>, yes: String },
}

struct St {
    hwnd: HWND,
    mode: Mode,
    items: Vec<MenuItem>,
    /// (y, tinggi) tiap baris dalam piksel.
    rows: Vec<(f32, f32)>,
    hover: Option<usize>,
    s: f32,
    w: f32,
    h: f32,
    on_pick: Option<Box<dyn FnOnce(usize)>>,
}

thread_local! {
    static STATE: RefCell<Option<St>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut St) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut b| b.as_mut().map(f)))
}

pub fn is_open() -> bool {
    STATE.with(|s| s.try_borrow().map(|b| b.is_some()).unwrap_or(true))
}

pub fn register() {
    unsafe {
        let Ok(hmod) = GetModuleHandleW(None) else { return };
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hmod.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&wc);
    }
}

pub fn close() {
    if let Some(h) = with(|st| st.hwnd) {
        unsafe {
            let _ = DestroyWindow(h);
        }
    }
}

struct Place {
    scale: f32,
    work: RECT,
}

fn place_at(x: i32, y: i32) -> Place {
    unsafe {
        let pt = POINT { x, y };
        let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        Place { scale: dx.max(96) as f32 / 96.0, work: mi.rcWork }
    }
}

fn font_name() -> String {
    config::current().bar.font.clone()
}

fn create(w: f32, h: f32, x: i32, y: i32) -> Option<HWND> {
    unsafe {
        let hmod = GetModuleHandleW(None).ok()?;
        let hwnd = CreateWindowExW(WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST, CLASS, w!("HolmBar Menu"), WS_POPUP, x, y, w as i32, h as i32, None, None, Some(hmod.into()), None).ok()?;
        Some(hwnd)
    }
}

/// Tampilkan menu di (x, y) layar. Membuka ke bawah; bila tidak muat, otomatis ke atas.
/// `on_pick` dipanggil dengan `id` item yang dipilih (tidak dipanggil bila dibatalkan).
pub fn show(items: Vec<MenuItem>, x: i32, y: i32, on_pick: impl FnOnce(usize) + 'static) {
    close();
    let pl = place_at(x, y);
    let s = pl.scale;
    let font = Font::new(&font_name(), 13.0 * s, false);
    let Some(m) = Canvas::new(1, 1) else { return };
    let (pad, row_h, sep_h, side) = (6.0 * s, 28.0 * s, 11.0 * s, 14.0 * s);
    let lab_w = items.iter().map(|i| m.measure(&i.label, &font)).fold(0.0f32, f32::max);
    let hint_w = items.iter().map(|i| m.measure(&i.hint, &font)).fold(0.0f32, f32::max);
    let gap = if hint_w > 0.0 { 28.0 * s } else { 0.0 };
    let w = (lab_w + gap + hint_w + 2.0 * (pad + side) + 6.0 * s).max(190.0 * s).round();
    let mut rows = Vec::new();
    let mut cy = pad;
    for it in &items {
        let hh = if it.sep { sep_h } else { row_h };
        rows.push((cy, hh));
        cy += hh;
    }
    let h = (cy + pad).round();
    let (wr, hr) = (pl.work.right - pl.work.left, pl.work.bottom - pl.work.top);
    let _ = (wr, hr);
    let mx = x.clamp(pl.work.left + 4, (pl.work.right - w as i32 - 4).max(pl.work.left + 4));
    let my = if y + h as i32 <= pl.work.bottom - 4 { y } else { (y - h as i32).max(pl.work.top + 4) };
    let Some(hwnd) = create(w, h, mx, my) else { return };
    STATE.with(|st| {
        *st.borrow_mut() = Some(St { hwnd, mode: Mode::Menu, items, rows, hover: None, s, w, h, on_pick: Some(Box::new(on_pick)) });
    });
    render((mx, my));
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
}

fn wrap(c: &Canvas, f: &Font, text: &str, max_w: f32) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let cand = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
        if c.measure(&cand, f) > max_w && !cur.is_empty() {
            lines.push(std::mem::take(&mut cur));
            cur = word.to_string();
        } else {
            cur = cand;
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

/// Dialog konfirmasi di tengah layar (pengganti MessageBox). `on_yes` dipanggil hanya bila dikonfirmasi.
pub fn confirm(text: &str, yes_label: &str, on_yes: impl FnOnce() + 'static) {
    close();
    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    let pl = place_at(pt.x, pt.y);
    let s = pl.scale;
    let Some(m) = Canvas::new(1, 1) else { return };
    let f = Font::new(&font_name(), 13.5 * s, false);
    let w = (360.0 * s).round();
    let lines = wrap(&m, &f, text, w - 48.0 * s);
    let h = (24.0 * s + lines.len() as f32 * 22.0 * s + 22.0 * s + 34.0 * s + 22.0 * s).round();
    let x = pl.work.left + ((pl.work.right - pl.work.left) as f32 - w) as i32 / 2;
    let y = pl.work.top + ((pl.work.bottom - pl.work.top) as f32 * 0.32 - h / 2.0) as i32;
    let Some(hwnd) = create(w, h, x, y) else { return };
    let items = vec![MenuItem::new("Batal", "", 0), MenuItem::new(yes_label, "", 1)];
    STATE.with(|st| {
        *st.borrow_mut() = Some(St {
            hwnd,
            mode: Mode::Confirm { lines, yes: yes_label.to_string() },
            items,
            rows: Vec::new(),
            hover: Some(0),
            s,
            w,
            h,
            on_pick: Some(Box::new(move |id| {
                if id == 1 {
                    on_yes();
                }
            })),
        });
    });
    render((x, y));
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
}

fn window_pos(hwnd: HWND) -> (i32, i32) {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut r);
    }
    (r.left, r.top)
}

/// Tombol dialog konfirmasi: (batal, ya) dalam piksel.
fn confirm_buttons(st: &St) -> [(f32, f32, f32, f32); 2] {
    let s = st.s;
    let (bw, bh) = (92.0 * s, 32.0 * s);
    let y = st.h - 22.0 * s - bh;
    [(st.w - 24.0 * s - 2.0 * bw - 10.0 * s, y, bw, bh), (st.w - 24.0 * s - bw, y, bw, bh)]
}

fn render(pos: (i32, i32)) {
    let Some(()) = with(|st| {
        let cfg = config::current();
        let s = st.s;
        let Some(c) = Canvas::new(st.w as i32, st.h as i32) else { return };
        let accent = config::color(&cfg.general.accent) | 0xFF00_0000;
        let font = Font::new(&cfg.bar.font, 13.0 * s, false);
        let r = 12.0 * s;
        // Bayangan lembut, tepi terang tipis, isi gelap.
        c.fill_round_rect(0.0, 0.0, st.w, st.h, (r, r, r, r), 0x38FFFFFF);
        c.fill_round_rect(1.0, 1.0, st.w - 2.0, st.h - 2.0, (r - 1.0, r - 1.0, r - 1.0, r - 1.0), 0xF4202024);
        match &st.mode {
            Mode::Menu => {
                let (pad, side) = (6.0 * s, 14.0 * s);
                for (i, it) in st.items.iter().enumerate() {
                    let (y, hh) = st.rows[i];
                    if it.sep {
                        c.fill_rect(pad + side, y + hh / 2.0, st.w - 2.0 * (pad + side), 1.0_f32.max(s * 0.8), 0x30FFFFFF);
                        continue;
                    }
                    let hot = st.hover == Some(i) && !it.disabled;
                    if hot {
                        let k = 7.0 * s;
                        c.fill_round_rect(pad, y, st.w - 2.0 * pad, hh, (k, k, k, k), accent);
                    }
                    let col = if it.disabled { 0xFF6E6E78 } else { 0xFFFFFFFF };
                    c.text(&it.label, &font, (pad + side, y, st.w, hh), col, Align::Near);
                    if !it.hint.is_empty() {
                        c.text(&it.hint, &font, (pad, y, st.w - 2.0 * pad - side, hh), if hot { 0xCCFFFFFF } else { DIM }, Align::Far);
                    }
                }
            }
            Mode::Confirm { lines, yes } => {
                let big = Font::new(&cfg.bar.font, 13.5 * s, false);
                for (i, l) in lines.iter().enumerate() {
                    c.text(l, &big, (24.0 * s, 24.0 * s + i as f32 * 22.0 * s, st.w - 48.0 * s, 22.0 * s), 0xFFFFFFFF, Align::Near);
                }
                let btns = confirm_buttons(st);
                let bold = Font::new(&cfg.bar.font, 13.0 * s, true);
                for (i, b) in btns.iter().enumerate() {
                    let k = 8.0 * s;
                    let hot = st.hover == Some(i);
                    let fill = if i == 1 { if hot { accent } else { config::with_alpha(accent, 0.85) } } else if hot { 0xFF4A4A52 } else { 0xFF38383E };
                    c.fill_round_rect(b.0, b.1, b.2, b.3, (k, k, k, k), fill);
                    c.text(if i == 0 { "Batal" } else { yes }, &bold, *b, 0xFFFFFFFF, Align::Center);
                }
            }
        }
        c.present(st.hwnd, Some(pos));
    }) else {
        return;
    };
}

fn hit(st: &St, x: f32, y: f32) -> Option<usize> {
    match st.mode {
        Mode::Menu => {
            let pad = 6.0 * st.s;
            if x < pad || x > st.w - pad {
                return None;
            }
            st.items.iter().enumerate().find(|(i, it)| !it.sep && !it.disabled && y >= st.rows[*i].0 && y < st.rows[*i].0 + st.rows[*i].1).map(|(i, _)| i)
        }
        Mode::Confirm { .. } => confirm_buttons(st).iter().position(|b| x >= b.0 && x <= b.0 + b.2 && y >= b.1 && y <= b.1 + b.3),
    }
}

/// Pilih item `i`: tutup jendela lalu panggil callback (di luar peminjaman state).
fn pick(i: usize) {
    let taken = STATE.with(|s| s.borrow_mut().take());
    let Some(mut st) = taken else { return };
    let id = match st.mode {
        Mode::Menu => st.items[i].id,
        Mode::Confirm { .. } => i,
    };
    unsafe {
        let _ = DestroyWindow(st.hwnd);
    }
    if let Some(cb) = st.on_pick.take() {
        cb(id);
    }
}

fn step(dir: i32) {
    with(|st| {
        let n = st.items.len() as i32;
        if n == 0 {
            return;
        }
        let mut i = st.hover.map(|h| h as i32).unwrap_or(if dir > 0 { -1 } else { n });
        for _ in 0..n {
            i = (i + dir).rem_euclid(n);
            let it = &st.items[i as usize];
            if !it.sep && !it.disabled {
                st.hover = Some(i as usize);
                break;
            }
        }
    });
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let xy = |l: LPARAM| ((l.0 & 0xFFFF) as i16 as f32, ((l.0 >> 16) & 0xFFFF) as i16 as f32);
    match msg {
        WM_MOUSEMOVE => {
            let (x, y) = xy(lparam);
            let changed = with(|st| {
                let h = hit(st, x, y);
                if matches!(st.mode, Mode::Confirm { .. }) && h.is_none() {
                    return false; // tombol tetap tersorot terakhir di dialog
                }
                std::mem::replace(&mut st.hover, h) != h
            })
            .unwrap_or(false);
            if changed {
                render(window_pos(hwnd));
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = xy(lparam);
            if let Some(Some(i)) = with(|st| hit(st, x, y)) {
                pick(i);
            }
            LRESULT(0)
        }
        WM_ACTIVATE => {
            if wparam.0 & 0xFFFF == 0 {
                close();
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            match wparam.0 as u16 {
                0x1B => close(),
                0x26 => {
                    step(-1);
                    render(window_pos(hwnd));
                }
                0x28 => {
                    step(1);
                    render(window_pos(hwnd));
                }
                0x25 | 0x27 if with(|st| matches!(st.mode, Mode::Confirm { .. })).unwrap_or(false) => {
                    with(|st| st.hover = Some(if wparam.0 == 0x25 { 0 } else { 1 }));
                    render(window_pos(hwnd));
                }
                0x0D => {
                    if let Some(Some(i)) = with(|st| st.hover) {
                        pick(i);
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // Dihancurkan dari luar `pick` (fokus hilang / Esc): buang state tanpa memanggil callback.
            STATE.with(|s| {
                if let Ok(mut b) = s.try_borrow_mut() {
                    if b.as_ref().map(|st| st.hwnd == hwnd).unwrap_or(false) {
                        *b = None;
                    }
                }
            });
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
