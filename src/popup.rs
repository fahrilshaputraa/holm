//! Popup bergaya macOS (kaca gelap, sudut membulat) yang muncul di bawah item bar:
//! kalender, volume, Wi-Fi, Bluetooth, dan peluncur pencarian aplikasi (Spotlight).
//! Satu jendela dipakai ulang. Menutup saat kehilangan fokus atau Esc.

use crate::canvas::{Align, Canvas, Font, Image};
use crate::config;
use crate::monitors::Mon;
use crate::radios::{self, Which};
use crate::status;
use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

type Rect = (f32, f32, f32, f32);

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Calendar,
    Volume,
    Wifi,
    Bluetooth,
    Launcher,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    CalPrev,
    CalNext,
    CalToday,
    VolSlider,
    VolMute,
    Uri(&'static str),
    AppRow(usize),
    WifiToggle,
    WifiNet(usize),
    PwSubmit,
    PwCancel,
    BtToggle,
    BtDev,
}

struct App {
    name: String,
    lower: String,
    path: String,
}

struct P {
    hwnd: HWND,
    kind: Kind,
    mon: Mon,
    anchor_x: f32,
    rects: Vec<(Rect, Hit)>,
    hover: Option<Hit>,
    dragging: bool,
    cal_off: i32,
    query: String,
    sel: usize,
    last_closed: Option<(Kind, Instant)>,
}

/// Data tampilan Wi-Fi (di thread UI).
#[derive(Default)]
struct WifiView {
    nets: Vec<status::Net>,
    /// Indeks jaringan yang sedang diminta kata sandinya.
    pw_for: Option<usize>,
    pw: String,
    msg: String,
    /// Profil buatan kita yang menunggu hasil sambungan (dihapus bila gagal).
    pending: Option<(String, Instant)>,
}

thread_local! {
    static WIFI: RefCell<WifiView> = RefCell::new(WifiView::default());
    static POP: RefCell<Option<P>> = const { RefCell::new(None) };
    static APPS: RefCell<(Vec<App>, Option<Instant>)> = const { RefCell::new((Vec::new(), None)) };
    static ICONS: RefCell<HashMap<String, Option<Image>>> = RefCell::new(HashMap::new());
}

fn with<R>(f: impl FnOnce(&mut P) -> R) -> Option<R> {
    POP.with(|p| p.borrow_mut().as_mut().map(f))
}

// ---------- Tanggal ----------

fn dow(y: i32, m: u32, d: u32) -> u32 {
    let t = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if m < 3 { y - 1 } else { y };
    ((y + y / 4 - y / 100 + y / 400 + t[(m - 1) as usize] + d as i32).rem_euclid(7)) as u32 // 0 = Minggu
}

fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
                29
            } else {
                28
            }
        }
    }
}

// ---------- UI immediate-mode ----------

struct Ui<'a> {
    c: Option<&'a Canvas>,
    s: f32,
    font: String,
    accent: u32,
    hover: Option<Hit>,
    rects: Vec<(Rect, Hit)>,
}

impl Ui<'_> {
    fn text(&self, t: &str, r: Rect, size: f32, bold: bool, color: u32, al: Align) {
        if let Some(c) = self.c {
            let f = Font::new(&self.font, size * self.s, bold);
            c.text(t, &f, r, color, al);
        }
    }
    fn round(&self, r: Rect, rad: f32, color: u32) {
        if let Some(c) = self.c {
            let k = rad * self.s;
            c.fill_round_rect(r.0, r.1, r.2, r.3, (k, k, k, k), color);
        }
    }
    fn poly(&self, pts: &[(f32, f32)], color: u32) {
        if let Some(c) = self.c {
            c.fill_polygon(pts, color);
        }
    }
    fn circle(&self, cx: f32, cy: f32, rad: f32, color: u32) {
        if let Some(c) = self.c {
            c.fill_ellipse(cx - rad, cy - rad, 2.0 * rad, 2.0 * rad, color);
        }
    }
    fn hit(&mut self, r: Rect, h: Hit) {
        self.rects.push((r, h));
    }
    fn hovered(&self, h: Hit) -> bool {
        self.hover == Some(h)
    }
    /// Tombol teks dengan latar saat disorot.
    fn button(&mut self, r: Rect, label: &str, h: Hit) {
        if self.hovered(h) {
            self.round(r, 8.0, 0x26FFFFFF);
        }
        self.text(label, r, 13.0, false, 0xFFFFFFFF, Align::Center);
        self.hit(r, h);
    }
}

const TIMER_RESCAN: usize = 1;
const TIMER_PENDING: usize = 2;
const WHITE: u32 = 0xFFFFFFFF;
const DIM: u32 = 0xFF9E9EA8;

fn build(ui: &mut Ui, kind: Kind, w: f32, cal_off: i32, query: &str, sel: usize) -> f32 {
    match kind {
        Kind::Calendar => build_calendar(ui, w, cal_off),
        Kind::Volume => build_volume(ui, w),
        Kind::Launcher => build_launcher(ui, w, query, sel),
        Kind::Wifi => build_wifi(ui, w),
        Kind::Bluetooth => build_bluetooth(ui, w),
    }
}

/// Saklar ala macOS pada kotak `r`.
fn switch(ui: &mut Ui, r: Rect, on: bool, h: Hit) {
    let col = if on { ui.accent } else { 0x59FFFFFF };
    ui.round(r, r.3 / (2.0 * ui.s), col);
    let k = r.3 - 4.0 * ui.s;
    let kx = if on { r.0 + r.2 - k - 2.0 * ui.s } else { r.0 + 2.0 * ui.s };
    ui.circle(kx + k / 2.0, r.1 + r.3 / 2.0, k / 2.0, WHITE);
    ui.hit(r, h);
}

fn signal_bars(ui: &Ui, x: f32, cy: f32, quality: u32, color: u32) {
    let s = ui.s;
    let lit = ((quality as f32 / 25.0).ceil() as usize).clamp(1, 4);
    for i in 0..4 {
        let h = (5.0 + i as f32 * 3.5) * s;
        let c = if i < lit { color } else { 0x40FFFFFF };
        ui.round((x + i as f32 * 5.0 * s, cy + 8.0 * s - h, 3.0 * s, h), 1.0, c);
    }
}

fn lock_icon(ui: &Ui, x: f32, cy: f32, color: u32) {
    let s = ui.s;
    ui.round((x, cy - 1.0 * s, 10.0 * s, 8.0 * s), 2.0, color);
    ui.round((x + 2.0 * s, cy - 5.0 * s, 1.6 * s, 5.0 * s), 0.8, color);
    ui.round((x + 6.4 * s, cy - 5.0 * s, 1.6 * s, 5.0 * s), 0.8, color);
    ui.round((x + 2.0 * s, cy - 5.5 * s, 6.0 * s, 1.8 * s), 0.9, color);
}

fn build_wifi(ui: &mut Ui, w: f32) -> f32 {
    let (s, pad) = (ui.s, 18.0 * ui.s);
    let on = radios::is_on(Which::Wifi);
    ui.text("Wi-Fi", (pad, pad, w - 2.0 * pad - 60.0 * s, 26.0 * s), 15.0, true, WHITE, Align::Near);
    if let Some(v) = on {
        switch(ui, (w - pad - 44.0 * s, pad + 1.0 * s, 44.0 * s, 24.0 * s), v, Hit::WifiToggle);
    }
    let mut y = pad + 38.0 * s;

    let (nets, pw_for, pw, msg) = WIFI.with(|v| {
        let v = v.borrow();
        (v.nets.clone(), v.pw_for, v.pw.clone(), v.msg.clone())
    });

    if let Some(i) = pw_for.filter(|&i| i < nets.len()) {
        // Kolom kata sandi untuk jaringan terpilih.
        ui.text(&format!("Kata sandi untuk \"{}\"", nets[i].ssid), (pad, y, w - 2.0 * pad, 22.0 * s), 13.0, true, WHITE, Align::Near);
        y += 28.0 * s;
        ui.round((pad, y, w - 2.0 * pad, 38.0 * s), 9.0, 0x1AFFFFFF);
        let dots = "\u{2022}".repeat(pw.chars().count());
        let shown = if pw.is_empty() { "Ketik kata sandi...".to_string() } else { format!("{dots}|") };
        ui.text(&shown, (pad + 12.0 * s, y, w - 2.0 * pad - 24.0 * s, 38.0 * s), 14.0, false, if pw.is_empty() { 0xFF7A7A85 } else { WHITE }, Align::Near);
        y += 48.0 * s;
        let bw = (w - 2.0 * pad - 8.0 * s) / 2.0;
        ui.button((pad, y, bw, 34.0 * s), "Batal", Hit::PwCancel);
        let ok = (pad + bw + 8.0 * s, y, bw, 34.0 * s);
        ui.round(ok, 8.0, ui.accent);
        ui.text("Hubungkan", ok, 13.0, true, WHITE, Align::Center);
        ui.hit(ok, Hit::PwSubmit);
        y += 34.0 * s + 6.0 * s;
    } else {
        match on {
            Some(false) => {
                ui.text("Wi-Fi nonaktif", (pad, y, w - 2.0 * pad, 40.0 * s), 13.0, false, DIM, Align::Center);
                y += 44.0 * s;
            }
            None if nets.is_empty() => {
                ui.text("Tidak ada adapter Wi-Fi", (pad, y, w - 2.0 * pad, 40.0 * s), 13.0, false, DIM, Align::Center);
                y += 44.0 * s;
            }
            _ if nets.is_empty() => {
                ui.text("Mencari jaringan...", (pad, y, w - 2.0 * pad, 40.0 * s), 13.0, false, DIM, Align::Center);
                y += 44.0 * s;
            }
            _ => {
                let row = 40.0 * s;
                for (i, n) in nets.iter().take(7).enumerate() {
                    let r = (pad - 6.0 * s, y, w - 2.0 * pad + 12.0 * s, row);
                    let h = Hit::WifiNet(i);
                    if ui.hovered(h) {
                        ui.round(r, 9.0, 0x1FFFFFFF);
                    }
                    let cy = y + row / 2.0;
                    signal_bars(ui, pad + 2.0 * s, cy, n.quality, WHITE);
                    let name_w = r.2 - 40.0 * s - 100.0 * s;
                    ui.text(&n.ssid, (pad + 34.0 * s, y, name_w, row), 13.5, n.connected, WHITE, Align::Near);
                    if n.connected {
                        let t = if ui.hovered(h) { "Putuskan" } else { "Terhubung" };
                        ui.text(t, (r.0 + r.2 - 100.0 * s, y, 90.0 * s, row), 12.0, false, if ui.hovered(h) { DIM } else { ui.accent }, Align::Far);
                    } else if n.secured {
                        lock_icon(ui, r.0 + r.2 - 22.0 * s, cy, DIM);
                    }
                    ui.hit(r, h);
                    y += row;
                }
            }
        }
    }
    if !msg.is_empty() {
        ui.text(&msg, (pad, y, w - 2.0 * pad, 24.0 * s), 12.0, false, DIM, Align::Near);
        y += 26.0 * s;
    }
    ui.button((pad, y + 4.0 * s, w - 2.0 * pad, 32.0 * s), "Pengaturan jaringan...", Hit::Uri("ms-settings:network-wifi"));
    y + 4.0 * s + 32.0 * s + pad
}

fn build_bluetooth(ui: &mut Ui, w: f32) -> f32 {
    let (s, pad) = (ui.s, 18.0 * ui.s);
    let on = radios::is_on(Which::Bluetooth);
    ui.text("Bluetooth", (pad, pad, w - 2.0 * pad - 60.0 * s, 26.0 * s), 15.0, true, WHITE, Align::Near);
    if let Some(v) = on {
        switch(ui, (w - pad - 44.0 * s, pad + 1.0 * s, 44.0 * s, 24.0 * s), v, Hit::BtToggle);
    }
    let mut y = pad + 38.0 * s;
    let devs = radios::bt_devices();
    match on {
        Some(false) => {
            ui.text("Bluetooth nonaktif", (pad, y, w - 2.0 * pad, 40.0 * s), 13.0, false, DIM, Align::Center);
            y += 44.0 * s;
        }
        None => {
            ui.text("Tidak ada adapter Bluetooth", (pad, y, w - 2.0 * pad, 40.0 * s), 13.0, false, DIM, Align::Center);
            y += 44.0 * s;
        }
        _ if devs.is_empty() => {
            ui.text("Belum ada perangkat terpasang", (pad, y, w - 2.0 * pad, 40.0 * s), 13.0, false, DIM, Align::Center);
            y += 44.0 * s;
        }
        _ => {
            let row = 40.0 * s;
            for d in devs.iter().take(8) {
                let r = (pad - 6.0 * s, y, w - 2.0 * pad + 12.0 * s, row);
                if ui.hovered(Hit::BtDev) && false {
                    ui.round(r, 9.0, 0x1FFFFFFF);
                }
                let cy = y + row / 2.0;
                ui.circle(pad + 6.0 * s, cy, 4.0 * s, if d.connected { ui.accent } else { 0x55FFFFFF });
                ui.text(&d.name, (pad + 24.0 * s, y, r.2 - 24.0 * s - 100.0 * s, row), 13.5, d.connected, WHITE, Align::Near);
                ui.text(if d.connected { "Terhubung" } else { "Terpasang" }, (r.0 + r.2 - 100.0 * s, y, 90.0 * s, row), 12.0, false, if d.connected { ui.accent } else { DIM }, Align::Far);
                y += row;
            }
            ui.text("Pasang/sambungkan perangkat lewat pengaturan Windows.", (pad, y + 2.0 * s, w - 2.0 * pad, 20.0 * s), 11.0, false, 0xFF7A7A85, Align::Near);
            y += 24.0 * s;
        }
    }
    ui.button((pad, y + 4.0 * s, w - 2.0 * pad, 32.0 * s), "Pengaturan Bluetooth...", Hit::Uri("ms-settings:bluetooth"));
    y + 4.0 * s + 32.0 * s + pad
}
fn build_calendar(ui: &mut Ui, w: f32, off: i32) -> f32 {
    let s = ui.s;
    let pad = 18.0 * s;
    let lang_en = config::current().general.language == "en";
    let now = unsafe { GetLocalTime() };
    let total = now.wYear as i32 * 12 + (now.wMonth as i32 - 1) + off;
    let (y, m) = (total.div_euclid(12), total.rem_euclid(12) as u32 + 1);

    let months_id = ["Januari", "Februari", "Maret", "April", "Mei", "Juni", "Juli", "Agustus", "September", "Oktober", "November", "Desember"];
    let months_en = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
    let name = if lang_en { months_en[(m - 1) as usize] } else { months_id[(m - 1) as usize] };

    // Judul + navigasi.
    ui.text(&format!("{name} {y}"), (pad, pad, w - 2.0 * pad - 90.0 * s, 26.0 * s), 15.0, true, WHITE, Align::Near);
    let (bx, by, bs) = (w - pad - 84.0 * s, pad, 26.0 * s);
    for (i, (h, dir)) in [(Hit::CalPrev, -1.0f32), (Hit::CalToday, 0.0), (Hit::CalNext, 1.0)].into_iter().enumerate() {
        let r = (bx + i as f32 * 29.0 * s, by, bs, bs);
        if ui.hovered(h) {
            ui.round(r, 7.0, 0x26FFFFFF);
        }
        let (cx, cy) = (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0);
        if dir == 0.0 {
            ui.circle(cx, cy, 3.5 * s, WHITE);
        } else {
            let k = 4.5 * s;
            ui.poly(&[(cx - dir * k * 0.6, cy - k), (cx - dir * k * 0.6, cy + k), (cx + dir * k * 0.9, cy)], WHITE);
        }
        ui.hit(r, h);
    }

    // Hari dalam seminggu (Senin pertama untuk id, Minggu pertama untuk en).
    let first_dow = if lang_en { 0 } else { 1 };
    let heads_id = ["S", "S", "R", "K", "J", "S", "M"]; // Sen..Min
    let heads_en = ["S", "M", "T", "W", "T", "F", "S"];
    let cell = (w - 2.0 * pad) / 7.0;
    let row_h = 34.0 * s;
    let top = pad + 40.0 * s;
    for i in 0..7 {
        let t = if lang_en { heads_en[i] } else { heads_id[i] };
        ui.text(t, (pad + i as f32 * cell, top, cell, 20.0 * s), 11.0, true, DIM, Align::Center);
    }

    let start = (dow(y, m, 1) + 7 - first_dow) % 7;
    let n = days_in_month(y, m);
    let is_current = off == 0;
    for d in 1..=n {
        let idx = start + d - 1;
        let (col, row) = (idx % 7, idx / 7);
        let r = (pad + col as f32 * cell, top + 24.0 * s + row as f32 * row_h, cell, row_h);
        let today = is_current && d == now.wDay as u32;
        let (cx, cy) = (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0);
        if today {
            ui.circle(cx, cy, 14.0 * s, ui.accent);
        }
        let weekend = ((col as u32 + first_dow) % 7) == 0 || ((col as u32 + first_dow) % 7) == 6;
        let color = if today { WHITE } else if weekend { 0xFFB0B0BA } else { WHITE };
        ui.text(&d.to_string(), (r.0, r.1, r.2, r.3), 13.0, today, color, Align::Center);
    }
    let rows = (start + n + 6) / 7;
    top + 24.0 * s + rows as f32 * row_h + pad - 6.0 * s
}

fn build_volume(ui: &mut Ui, w: f32) -> f32 {
    let (s, pad) = (ui.s, 18.0 * ui.s);
    let v = status::volume().unwrap_or(status::Volume { level: 0.0, muted: false });
    ui.text("Suara", (pad, pad, w - 2.0 * pad, 24.0 * s), 15.0, true, WHITE, Align::Near);

    let y = pad + 44.0 * s;
    // Ikon speaker (klik = mute).
    let ir = (pad - 6.0 * s, y - 14.0 * s, 36.0 * s, 28.0 * s);
    if ui.hovered(Hit::VolMute) {
        ui.round(ir, 8.0, 0x26FFFFFF);
    }
    let (cx, cy) = (pad + 12.0 * s, y);
    let col = if v.muted { 0xFF8E8E98 } else { WHITE };
    ui.poly(&[(cx - 8.0 * s, cy - 4.0 * s), (cx - 3.0 * s, cy - 4.0 * s), (cx + 3.0 * s, cy - 9.0 * s), (cx + 3.0 * s, cy + 9.0 * s), (cx - 3.0 * s, cy + 4.0 * s), (cx - 8.0 * s, cy + 4.0 * s)], col);
    if v.muted {
        let k = 4.0 * s;
        ui.poly(&[(cx + 7.0 * s, cy - k), (cx + 9.0 * s, cy - k), (cx + 15.0 * s, cy + k), (cx + 13.0 * s, cy + k)], col);
        ui.poly(&[(cx + 13.0 * s, cy - k), (cx + 15.0 * s, cy - k), (cx + 9.0 * s, cy + k), (cx + 7.0 * s, cy + k)], col);
    }
    ui.hit(ir, Hit::VolMute);

    // Slider.
    let (tx, tw, th) = (pad + 40.0 * s, w - 2.0 * pad - 40.0 * s - 44.0 * s, 6.0 * s);
    let ty = y - th / 2.0;
    ui.round((tx, ty, tw, th), 3.0, 0x40FFFFFF);
    let lvl = if v.muted { 0.0 } else { v.level };
    ui.round((tx, ty, (tw * lvl).max(th), th), 3.0, ui.accent);
    ui.circle(tx + tw * lvl, y, 9.0 * s, 0x55000000);
    ui.circle(tx + tw * lvl, y, 8.0 * s, WHITE);
    ui.hit((tx - 8.0 * s, y - 16.0 * s, tw + 16.0 * s, 32.0 * s), Hit::VolSlider);
    ui.text(&format!("{}%", (lvl * 100.0).round() as i32), (tx + tw + 8.0 * s, y - 12.0 * s, 40.0 * s, 24.0 * s), 12.0, false, DIM, Align::Far);

    let b = (pad, y + 28.0 * s, w - 2.0 * pad, 32.0 * s);
    ui.button(b, "Pengaturan suara...", Hit::Uri("ms-settings:sound"));
    y + 28.0 * s + 32.0 * s + pad
}

// ---------- Peluncur aplikasi ----------

fn start_menu_dirs() -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    if let Some(p) = std::env::var_os("ProgramData") {
        v.push(std::path::PathBuf::from(p).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    if let Some(p) = std::env::var_os("APPDATA") {
        v.push(std::path::PathBuf::from(p).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    v
}

fn scan(dir: &std::path::Path, depth: u32, out: &mut Vec<App>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if depth < 4 {
                scan(&p, depth + 1, out);
            }
        } else if p.extension().map(|x| x.eq_ignore_ascii_case("lnk")).unwrap_or(false) {
            let name = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let l = name.to_lowercase();
            if !l.contains("uninstall") && !l.contains("hapus") && !name.is_empty() && !out.iter().any(|a| a.lower == l) {
                out.push(App { lower: l, name, path: p.to_string_lossy().to_string() });
            }
        }
    }
}

fn ensure_apps() {
    APPS.with(|a| {
        let mut a = a.borrow_mut();
        if a.1.map(|t| t.elapsed() < Duration::from_secs(120)).unwrap_or(false) {
            return;
        }
        let mut v = Vec::new();
        for d in start_menu_dirs() {
            scan(&d, 0, &mut v);
        }
        v.sort_by(|x, y| x.lower.cmp(&y.lower));
        *a = (v, Some(Instant::now()));
    });
}

fn score(name: &str, q: &str) -> Option<i32> {
    if name == q {
        return Some(1000);
    }
    if name.starts_with(q) {
        return Some(800 - name.len() as i32);
    }
    if name.split([' ', '-', '_', '.']).any(|w| w.starts_with(q)) {
        return Some(600 - name.len() as i32);
    }
    if let Some(p) = name.find(q) {
        return Some(400 - p as i32);
    }
    // subsekuens (mis. "vsc" -> "visual studio code")
    let mut it = name.chars();
    if q.chars().all(|c| it.any(|n| n == c)) {
        return Some(100 - name.len() as i32);
    }
    None
}

fn results(query: &str) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return vec![];
    }
    APPS.with(|a| {
        let a = a.borrow();
        let mut r: Vec<(i32, usize)> = a.0.iter().enumerate().filter_map(|(i, x)| score(&x.lower, &q).map(|s| (s, i))).collect();
        r.sort_by(|a, b| b.0.cmp(&a.0));
        r.into_iter().take(8).map(|(_, i)| i).collect()
    })
}

fn icon_for(path: &str) {
    ICONS.with(|c| {
        let mut c = c.borrow_mut();
        if c.contains_key(path) {
            return;
        }
        let w: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        let mut info = SHFILEINFOW::default();
        let img = unsafe {
            let r = SHGetFileInfoW(PCWSTR(w.as_ptr()), Default::default(), Some(&mut info), std::mem::size_of::<SHFILEINFOW>() as u32, SHGFI_ICON | SHGFI_LARGEICON);
            if r != 0 && !info.hIcon.0.is_null() {
                let i = Image::from_hicon(info.hIcon);
                let _ = DestroyIcon(info.hIcon);
                i
            } else {
                None
            }
        };
        c.insert(path.to_string(), img);
    });
}

fn build_launcher(ui: &mut Ui, w: f32, query: &str, sel: usize) -> f32 {
    let (s, pad) = (ui.s, 16.0 * ui.s);
    // Kotak pencarian.
    let fh = 40.0 * s;
    ui.round((pad, pad, w - 2.0 * pad, fh), 10.0, 0x1AFFFFFF);
    // Ikon kaca pembesar: lingkaran + gagang.
    let (mx, my) = (pad + 22.0 * s, pad + fh / 2.0 - 1.0 * s);
    ui.circle(mx, my, 7.0 * s, DIM);
    ui.circle(mx, my, 5.2 * s, 0xFF2A2A30);
    ui.poly(&[(mx + 4.0 * s, my + 6.0 * s), (mx + 6.0 * s, my + 4.0 * s), (mx + 11.0 * s, my + 9.0 * s), (mx + 9.0 * s, my + 11.0 * s)], DIM);
    let tr = (pad + 42.0 * s, pad, w - 2.0 * pad - 54.0 * s, fh);
    if query.is_empty() {
        ui.text("Cari aplikasi...", tr, 15.0, false, 0xFF7A7A85, Align::Near);
    } else {
        ui.text(&format!("{query}|"), tr, 15.0, false, WHITE, Align::Near);
    }

    let res = results(query);
    let mut y = pad + fh + 8.0 * s;
    let row = 46.0 * s;
    if res.is_empty() && !query.trim().is_empty() {
        ui.text("Tidak ada hasil", (pad, y, w - 2.0 * pad, 40.0 * s), 13.0, false, DIM, Align::Center);
        y += 40.0 * s;
    }
    for (i, &ai) in res.iter().enumerate() {
        let r = (pad, y, w - 2.0 * pad, row);
        let selected = i == sel.min(res.len().saturating_sub(1));
        if selected || ui.hovered(Hit::AppRow(i)) {
            ui.round(r, 10.0, if selected { ui.accent } else { 0x1FFFFFFF });
        }
        let (name, path) = APPS.with(|a| {
            let a = a.borrow();
            (a.0[ai].name.clone(), a.0[ai].path.clone())
        });
        if ui.c.is_some() {
            icon_for(&path);
            ICONS.with(|c| {
                if let (Some(c0), Some(Some(img))) = (ui.c, c.borrow().get(&path)) {
                    c0.draw_image(img, r.0 + 10.0 * s, r.1 + (row - 30.0 * s) / 2.0, 30.0 * s, 30.0 * s, 0.0, 1.0);
                }
            });
        }
        ui.text(&name, (r.0 + 52.0 * s, r.1, r.2 - 64.0 * s, row), 14.0, false, WHITE, Align::Near);
        ui.hit(r, Hit::AppRow(i));
        y += row;
    }
    y + pad - 8.0 * s
}

// ---------- Jendela ----------

fn render() {
    let Some((hwnd, kind, mon, anchor_x, hover, cal_off, query, sel)) =
        with(|p| (p.hwnd, p.kind, p.mon, p.anchor_x, p.hover, p.cal_off, p.query.clone(), p.sel))
    else {
        return;
    };
    let cfg = config::current();
    let s = mon.scale;
    let w = match kind {
        Kind::Calendar => 300.0,
        Kind::Volume => 300.0,
        Kind::Wifi | Kind::Bluetooth => 340.0,
        Kind::Launcher => 600.0,
    } * s;
    let accent = config::color(&cfg.general.accent);
    let mut ui = Ui { c: None, s, font: cfg.bar.font.clone(), accent, hover, rects: Vec::new() };
    let h = build(&mut ui, kind, w, cal_off, &query, sel).ceil();

    let Some(c) = Canvas::new(w as i32, h as i32) else { return };
    let r = 16.0 * s;
    c.fill_round_rect(0.0, 0.0, w, h, (r, r, r, r), 0x30FFFFFF);
    c.fill_round_rect(1.0, 1.0, w - 2.0, h - 2.0, (r - 1.0, r - 1.0, r - 1.0, r - 1.0), 0xF21E1E22);
    let mut ui = Ui { c: Some(&c), s, font: cfg.bar.font.clone(), accent, hover, rects: Vec::new() };
    build(&mut ui, kind, w, cal_off, &query, sel);
    let rects = ui.rects;

    // Posisi: di bawah bar, dipusatkan pada item (peluncur: pusat layar, sepertiga atas).
    let bar_h = (cfg.bar.height as f32 * s).round();
    let (x, y) = if kind == Kind::Launcher {
        (mon.x as f32 + (mon.w as f32 - w) / 2.0, mon.y as f32 + mon.h as f32 * 0.18)
    } else {
        ((anchor_x - w / 2.0).clamp(mon.x as f32 + 8.0 * s, (mon.x + mon.w) as f32 - w - 8.0 * s), mon.y as f32 + bar_h + 6.0 * s)
    };
    c.present(hwnd, Some((x.round() as i32, y.round() as i32)));
    with(|p| p.rects = rects);
}

fn ensure_window() -> Option<HWND> {
    if let Some(h) = with(|p| p.hwnd) {
        return Some(h);
    }
    unsafe {
        let hinst = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).ok()?;
        let class: Vec<u16> = "HormPopupClass".encode_utf16().chain(std::iter::once(0)).collect();
        let h = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            PCWSTR(class.as_ptr()),
            windows::core::w!("HormBar Popup"),
            WS_POPUP,
            0,
            0,
            10,
            10,
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .ok()?;
        POP.with(|p| {
            *p.borrow_mut() = Some(P {
                hwnd: h,
                kind: Kind::Calendar,
                mon: Mon { x: 0, y: 0, w: 1920, h: 1080, scale: 1.0, primary: true },
                anchor_x: 0.0,
                rects: Vec::new(),
                hover: None,
                dragging: false,
                cal_off: 0,
                query: String::new(),
                sel: 0,
                last_closed: None,
            })
        });
        Some(h)
    }
}

pub fn is_open(kind: Kind) -> bool {
    with(|p| p.kind == kind && unsafe { IsWindowVisible(p.hwnd) }.as_bool()).unwrap_or(false)
}

fn hide() {
    if let Some((h, k)) = with(|p| (p.hwnd, p.kind)) {
        unsafe {
            let _ = ShowWindow(h, SW_HIDE);
        }
        with(|p| {
            p.last_closed = Some((k, Instant::now()));
            p.dragging = false;
        });
    }
}

/// Buka popup `kind` (anchor_x = koordinat layar pusat item bar); klik kedua menutup.
pub fn toggle(kind: Kind, anchor_x: f32, mon: Mon) {
    let Some(h) = ensure_window() else { return };
    if is_open(kind) {
        hide();
        return;
    }
    // Klik pada item bar menonaktifkan popup (WM_ACTIVATE) lalu mengirim klik: jangan buka ulang.
    if with(|p| matches!(p.last_closed, Some((k, t)) if k == kind && t.elapsed() < Duration::from_millis(300))).unwrap_or(false) {
        return;
    }
    with(|p| {
        p.kind = kind;
        p.mon = mon;
        p.anchor_x = anchor_x;
        p.cal_off = 0;
        p.query.clear();
        p.sel = 0;
        p.hover = None;
    });
    match kind {
        Kind::Launcher => ensure_apps(),
        Kind::Wifi => {
            WIFI.with(|v| {
                let mut v = v.borrow_mut();
                v.pw_for = None;
                v.pw.clear();
                v.msg.clear();
                v.nets = status::networks();
            });
            status::wifi_scan();
            radios::refresh();
            unsafe {
                SetTimer(Some(h), TIMER_RESCAN, 2500, None);
            }
        }
        Kind::Bluetooth => {
            radios::refresh();
            radios::refresh_bt_devices();
        }
        _ => {}
    }
    render();
    unsafe {
        let _ = ShowWindow(h, SW_SHOW);
        let _ = SetForegroundWindow(h);
    }
}

/// Buka peluncur di monitor tempat kursor berada (hotkey / menu).
pub fn open_launcher() {
    let mons = crate::monitors::all();
    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    let mon = mons.iter().copied().find(|m| pt.x >= m.x && pt.x < m.x + m.w && pt.y >= m.y && pt.y < m.y + m.h).or(mons.first().copied());
    if let Some(m) = mon {
        toggle(Kind::Launcher, 0.0, m);
    }
}

/// Status sistem berubah (mis. volume dari tombol keyboard): gambar ulang popup volume bila terbuka.
pub fn on_status_changed() {
    if is_open(Kind::Volume) && !with(|p| p.dragging).unwrap_or(false) {
        render();
    } else if is_open(Kind::Wifi) {
        // Kata sandi yang sedang diketik jangan terganggu; hanya segarkan daftar.
        WIFI.with(|v| v.borrow_mut().nets = status::networks());
        render();
    } else if is_open(Kind::Bluetooth) {
        render();
    }
}

fn hit_at(x: f32, y: f32) -> Option<Hit> {
    with(|p| p.rects.iter().rev().find(|(r, _)| x >= r.0 && x <= r.0 + r.2 && y >= r.1 && y <= r.1 + r.3).map(|(_, h)| *h)).flatten()
}

fn slider_value(x: f32) -> Option<f32> {
    with(|p| {
        p.rects.iter().find(|(_, h)| *h == Hit::VolSlider).map(|(r, _)| {
            let s = p.mon.scale;
            let (tx, tw) = (r.0 + 8.0 * s, r.2 - 16.0 * s);
            ((x - tx) / tw).clamp(0.0, 1.0)
        })
    })
    .flatten()
}

fn launch(path: &str) {
    hide();
    let w: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        ShellExecuteW(None, windows::core::w!("open"), PCWSTR(w.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

fn activate(h: Hit) {
    match h {
        Hit::CalPrev => {
            with(|p| p.cal_off -= 1);
            render();
        }
        Hit::CalNext => {
            with(|p| p.cal_off += 1);
            render();
        }
        Hit::CalToday => {
            with(|p| p.cal_off = 0);
            render();
        }
        Hit::VolMute => {
            status::toggle_mute();
        }
        Hit::VolSlider => {}
        Hit::Uri(u) => {
            hide();
            let w: Vec<u16> = u.encode_utf16().chain(std::iter::once(0)).collect();
            unsafe {
                ShellExecuteW(None, windows::core::w!("open"), PCWSTR(w.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
            }
        }
        Hit::WifiToggle => {
            let on = radios::is_on(Which::Wifi).unwrap_or(true);
            radios::set(Which::Wifi, !on);
        }
        Hit::BtToggle => {
            let on = radios::is_on(Which::Bluetooth).unwrap_or(true);
            radios::set(Which::Bluetooth, !on);
        }
        Hit::BtDev => {}
        Hit::PwCancel => {
            WIFI.with(|v| {
                let mut v = v.borrow_mut();
                v.pw_for = None;
                v.pw.clear();
            });
            render();
        }
        Hit::PwSubmit => submit_password(),
        Hit::WifiNet(i) => click_network(i),
        Hit::AppRow(i) => {
            let q = with(|p| p.query.clone()).unwrap_or_default();
            if let Some(&ai) = results(&q).get(i) {
                let path = APPS.with(|a| a.borrow().0[ai].path.clone());
                launch(&path);
            }
        }
    }
}

fn set_msg(m: &str) {
    WIFI.with(|v| v.borrow_mut().msg = m.to_string());
}

fn click_network(i: usize) {
    let net = WIFI.with(|v| v.borrow().nets.get(i).cloned());
    let Some(net) = net else { return };
    if net.connected {
        status::wifi_disconnect();
        set_msg("");
        return;
    }
    let supported = net.has_profile || (!net.secured && net.auth == 1) || net.auth == 7 || net.auth == 9;
    if !supported {
        set_msg("Jenis keamanan ini perlu diatur lewat Windows.");
    } else if net.has_profile || !net.secured {
        set_msg(&format!("Menghubungkan ke {}...", net.ssid));
        if !status::connect(&net, None) {
            set_msg("Gagal memulai sambungan.");
        }
    } else {
        WIFI.with(|v| {
            let mut v = v.borrow_mut();
            v.pw_for = Some(i);
            v.pw.clear();
            v.msg.clear();
        });
    }
    render();
}

fn submit_password() {
    let (net, pw) = WIFI.with(|v| {
        let v = v.borrow();
        (v.pw_for.and_then(|i| v.nets.get(i).cloned()), v.pw.clone())
    });
    let Some(net) = net else { return };
    if pw.len() < 8 {
        set_msg("Kata sandi minimal 8 karakter.");
        render();
        return;
    }
    if status::connect(&net, Some(&pw)) {
        set_msg(&format!("Menghubungkan ke {}...", net.ssid));
        WIFI.with(|v| {
            let mut v = v.borrow_mut();
            v.pending = Some((net.ssid.clone(), Instant::now()));
            v.pw_for = None;
            v.pw.clear();
        });
        if let Some(h) = with(|p| p.hwnd) {
            unsafe {
                SetTimer(Some(h), TIMER_PENDING, 8000, None);
            }
        }
    } else {
        set_msg("Gagal membuat profil jaringan.");
    }
    render();
}

pub unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let xy = |l: LPARAM| ((l.0 & 0xFFFF) as i16 as f32, ((l.0 >> 16) & 0xFFFF) as i16 as f32);
    match msg {
        WM_ACTIVATE => {
            if (wparam.0 & 0xFFFF) == 0 {
                hide();
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = xy(lparam);
            if with(|p| p.dragging).unwrap_or(false) {
                if let Some(v) = slider_value(x) {
                    status::change_volume(v);
                }
                return LRESULT(0);
            }
            let h = hit_at(x, y);
            if with(|p| std::mem::replace(&mut p.hover, h) != h).unwrap_or(false) {
                render();
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = xy(lparam);
            if hit_at(x, y) == Some(Hit::VolSlider) {
                with(|p| p.dragging = true);
                unsafe {
                    SetCapture(hwnd);
                }
                if let Some(v) = slider_value(x) {
                    status::change_volume(v);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = xy(lparam);
            if with(|p| std::mem::replace(&mut p.dragging, false)).unwrap_or(false) {
                unsafe {
                    let _ = ReleaseCapture();
                }
                render();
            } else if let Some(h) = hit_at(x, y) {
                activate(h);
            }
            LRESULT(0)
        }
        WM_TIMER => {
            match wparam.0 {
                TIMER_RESCAN => {
                    unsafe {
                        let _ = KillTimer(Some(hwnd), TIMER_RESCAN);
                    }
                    if is_open(Kind::Wifi) {
                        WIFI.with(|v| v.borrow_mut().nets = status::networks());
                        render();
                    }
                }
                TIMER_PENDING => {
                    unsafe {
                        let _ = KillTimer(Some(hwnd), TIMER_PENDING);
                    }
                    // Sandi salah / gagal tersambung: hapus profil buatan kita agar tidak tersimpan.
                    let pending = WIFI.with(|v| v.borrow_mut().pending.take());
                    if let Some((ssid, _)) = pending {
                        let nets = status::networks();
                        let ok = nets.iter().any(|n| n.ssid == ssid && n.connected);
                        if !ok {
                            status::delete_profile(&ssid);
                            set_msg("Gagal tersambung. Periksa kata sandi lalu coba lagi.");
                        } else {
                            set_msg("");
                        }
                        WIFI.with(|v| v.borrow_mut().nets = nets);
                        render();
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) & 0xFFFF) as i16 as i32;
            match with(|p| p.kind) {
                Some(Kind::Calendar) => {
                    with(|p| p.cal_off += if delta > 0 { -1 } else { 1 });
                    render();
                }
                Some(Kind::Volume) => {
                    let cur = status::volume().map(|v| v.level).unwrap_or(0.0);
                    status::change_volume(cur + if delta > 0 { 0.04 } else { -0.04 });
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_CHAR => {
            if with(|p| p.kind) == Some(Kind::Wifi) && WIFI.with(|v| v.borrow().pw_for.is_some()) {
                let c = wparam.0 as u32;
                if c == 0x08 {
                    WIFI.with(|v| {
                        v.borrow_mut().pw.pop();
                    });
                } else if c >= 0x20 && c != 0x7F {
                    if let Some(ch) = char::from_u32(c) {
                        WIFI.with(|v| v.borrow_mut().pw.push(ch));
                    }
                } else {
                    return LRESULT(0);
                }
                render();
                return LRESULT(0);
            }
            if with(|p| p.kind) == Some(Kind::Launcher) {
                let c = wparam.0 as u32;
                if c == 0x08 {
                    with(|p| {
                        p.query.pop();
                        p.sel = 0;
                    });
                } else if c >= 0x20 && c != 0x7F {
                    if let Some(ch) = char::from_u32(c) {
                        with(|p| {
                            p.query.push(ch);
                            p.sel = 0;
                        });
                    }
                } else {
                    return LRESULT(0);
                }
                render();
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let vk = wparam.0 as u16;
            let kind = with(|p| p.kind);
            let pw_open = kind == Some(Kind::Wifi) && WIFI.with(|v| v.borrow().pw_for.is_some());
            if vk == VK_ESCAPE.0 && pw_open {
                activate(Hit::PwCancel);
            } else if vk == VK_RETURN.0 && pw_open {
                submit_password();
            } else if vk == VK_ESCAPE.0 {
                hide();
            } else if kind == Some(Kind::Launcher) {
                let q = with(|p| p.query.clone()).unwrap_or_default();
                let n = results(&q).len();
                if vk == VK_DOWN.0 && n > 0 {
                    with(|p| p.sel = (p.sel + 1) % n);
                    render();
                } else if vk == VK_UP.0 && n > 0 {
                    with(|p| p.sel = (p.sel + n - 1) % n);
                    render();
                } else if vk == VK_RETURN.0 {
                    let sel = with(|p| p.sel).unwrap_or(0);
                    activate(Hit::AppRow(sel.min(n.saturating_sub(1))));
                }
            } else if kind == Some(Kind::Calendar) {
                if vk == VK_LEFT.0 {
                    activate(Hit::CalPrev);
                } else if vk == VK_RIGHT.0 {
                    activate(Hit::CalNext);
                }
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

pub fn destroy() {
    if let Some(h) = with(|p| p.hwnd) {
        unsafe {
            let _ = DestroyWindow(h);
        }
    }
    POP.with(|p| *p.borrow_mut() = None);
}
