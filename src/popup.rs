//! Popup bergaya macOS (kaca gelap, sudut membulat) yang muncul di bawah item bar:
//! kalender, volume, Wi-Fi, Bluetooth, dan peluncur pencarian aplikasi (Spotlight).
//! Satu jendela dipakai ulang. Menutup saat kehilangan fokus atau Esc.

use crate::canvas::{Align, Canvas, Font, Image};
use crate::icons::{self, Icon};
use crate::config;
use crate::control;
use crate::media;
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
    Tray,
    Control,
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
    TrayIcon(u64),
    WifiToggle,
    WifiNet(usize),
    PwSubmit,
    PwCancel,
    BtToggle,
    BtDev,
    CcWifi,
    CcWifiOpen,
    CcBt,
    CcBtOpen,
    CcAirplane,
    CcDnd,
    CcBright,
    CcVol,
    CcPlay,
    CcNext,
    CcSettings,
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
    drag_hit: Option<Hit>,
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
    /// Sampul lagu untuk kartu Now Playing di Control Center (di-decode ulang hanya saat berganti).
    static CC_ART: RefCell<Option<(usize, Image)>> = const { RefCell::new(None) };
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
    /// Ikon filled di kotak `r` (potongan memakai warna kartu `bg`).
    fn ic(&self, icon: Icon, r: Rect, col: u32, bg: u32) {
        if let Some(c) = self.c {
            icons::draw(c, icon, r, col, bg);
        }
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
        Kind::Tray => build_tray(ui, w),
        Kind::Control => build_control(ui, w),
    }
}

const TRAY_COLS: usize = 6;
const TRAY_CELL: f32 = 40.0;

/// Kotak ikon tray tersembunyi (seperti overflow taskbar), dengan tooltip ikon yang disorot.
fn build_tray(ui: &mut Ui, w: f32) -> f32 {
    let (s, pad) = (ui.s, 12.0 * ui.s);
    let (_, items) = crate::tray::split();
    let cell = TRAY_CELL * s;
    let cols = items.len().clamp(1, TRAY_COLS);
    let x0 = (w - cols as f32 * cell) / 2.0;
    let mut tip = String::new();
    for (i, v) in items.iter().enumerate() {
        let r = (x0 + (i % cols) as f32 * cell, pad + (i / cols) as f32 * cell, cell, cell);
        let hit = Hit::TrayIcon(v.id);
        if ui.hovered(hit) {
            ui.round((r.0 + 3.0 * s, r.1 + 3.0 * s, r.2 - 6.0 * s, r.3 - 6.0 * s), 9.0, 0x26FFFFFF);
            tip = v.tip.lines().next().unwrap_or("").to_string();
        }
        if let (Some(c), Some(img)) = (ui.c, &v.image) {
            let isz = 18.0 * s;
            c.draw_image(img, r.0 + (cell - isz) / 2.0, r.1 + (cell - isz) / 2.0, isz, isz, 0.0, 1.0);
        }
        ui.hit(r, hit);
    }
    let rows = items.len().div_ceil(cols).max(1);
    let mut y = pad + rows as f32 * cell;
    if items.is_empty() {
        ui.text("Tidak ada ikon tersembunyi", (pad, pad, w - 2.0 * pad, cell), 12.5, false, DIM, Align::Center);
    }
    // Baris tooltip: nama ikon yang disorot.
    ui.text(if tip.is_empty() { "Klik kanan untuk menu aplikasi" } else { &tip }, (pad, y + 2.0 * s, w - 2.0 * pad, 18.0 * s), 11.5, false, if tip.is_empty() { 0xFF6E6E78 } else { WHITE }, Align::Center);
    y += 24.0 * s;
    y + pad - 6.0 * s
}

// ---------- Control Center ----------

const CARD: u32 = 0x16FFFFFF;
const CARD_BG: u32 = 0xFF2A2A2E; // perkiraan warna kartu di atas latar popup (untuk potongan ikon)
const OFF_CIRCLE: u32 = 0xFF4A4A52;

/// Baris kiri kartu konektivitas: lingkaran ikon (aksi `toggle`) + dua baris teks (aksi `open`).
#[allow(clippy::too_many_arguments)]
fn cc_row(ui: &mut Ui, r: Rect, icon: Icon, on: bool, title: &str, sub: &str, toggle: Hit, open: Hit) {
    let s = ui.s;
    let d = 34.0 * s;
    let circ = (r.0 + 10.0 * s, r.1 + (r.3 - d) / 2.0, d, d);
    let hov = ui.hovered(toggle) || ui.hovered(open);
    ui.circle(circ.0 + d / 2.0, circ.1 + d / 2.0, d / 2.0, if on { ui.accent } else if hov { 0xFF5A5A62 } else { OFF_CIRCLE });
    let k = 18.0 * s;
    ui.ic(icon, (circ.0 + (d - k) / 2.0, circ.1 + (d - k) / 2.0, k, k), WHITE, if on { ui.accent } else { OFF_CIRCLE });
    let tx = circ.0 + d + 10.0 * s;
    let tw = r.0 + r.2 - tx - 6.0 * s;
    ui.text(title, (tx, r.1 + r.3 / 2.0 - 17.0 * s, tw, 18.0 * s), 13.0, true, WHITE, Align::Near);
    ui.text(sub, (tx, r.1 + r.3 / 2.0 + 1.0 * s, tw, 16.0 * s), 11.0, false, DIM, Align::Near);
    ui.hit((tx - 4.0 * s, r.1, r.0 + r.2 - tx + 4.0 * s, r.3), open);
    ui.hit(circ, toggle); // ditambahkan terakhir: menang atas area teks
}

/// Ubin kanan: lingkaran ikon di kiri + label dua baris.
fn cc_tile(ui: &mut Ui, r: Rect, icon: Icon, on: bool, line1: &str, line2: &str, h: Hit) {
    let s = ui.s;
    ui.round(r, 14.0, if ui.hovered(h) { 0x24FFFFFF } else { CARD });
    let d = 32.0 * s;
    let circ = (r.0 + 10.0 * s, r.1 + (r.3 - d) / 2.0, d, d);
    ui.circle(circ.0 + d / 2.0, circ.1 + d / 2.0, d / 2.0, if on { ui.accent } else { OFF_CIRCLE });
    let k = 17.0 * s;
    ui.ic(icon, (circ.0 + (d - k) / 2.0, circ.1 + (d - k) / 2.0, k, k), WHITE, if on { ui.accent } else { OFF_CIRCLE });
    let tx = circ.0 + d + 8.0 * s;
    let tw = r.0 + r.2 - tx - 4.0 * s;
    let (y1, y2) = if line2.is_empty() { (r.1 + r.3 / 2.0 - 9.0 * s, 0.0) } else { (r.1 + r.3 / 2.0 - 18.0 * s, r.1 + r.3 / 2.0) };
    ui.text(line1, (tx, y1, tw, 18.0 * s), 12.5, true, WHITE, Align::Near);
    if !line2.is_empty() {
        ui.text(line2, (tx, y2, tw, 18.0 * s), 12.5, true, WHITE, Align::Near);
    }
    ui.hit(r, h);
}

/// Kartu slider besar ala macOS: label di atas, trek bulat dengan ikon di dalamnya.
fn cc_slider(ui: &mut Ui, r: Rect, label: &str, icon: Icon, frac: f32, slider: Hit, icon_hit: Option<Hit>) {
    let s = ui.s;
    ui.round(r, 14.0, CARD);
    ui.text(label, (r.0 + 14.0 * s, r.1 + 8.0 * s, r.2 - 28.0 * s, 18.0 * s), 12.0, true, WHITE, Align::Near);
    let track = (r.0 + 10.0 * s, r.1 + r.3 - 10.0 * s - 28.0 * s, r.2 - 20.0 * s, 28.0 * s);
    let frac = frac.clamp(0.0, 1.0);
    ui.round(track, 14.0, 0x30FFFFFF);
    let fw = (track.2 * frac).max(track.3);
    ui.round((track.0, track.1, fw, track.3), 14.0, 0xFFF4F4F6);
    let k = 16.0 * s;
    let over_fill = fw > 30.0 * s;
    ui.ic(icon, (track.0 + 7.0 * s, track.1 + (track.3 - k) / 2.0, k, k), if over_fill { 0xFF6A6A72 } else { 0xFFC8C8D0 }, if over_fill { 0xFFF4F4F6 } else { 0xFF4A4A50 });
    ui.hit(track, slider);
    if let Some(h) = icon_hit {
        ui.hit((track.0, track.1, 30.0 * s, track.3), h);
    }
}

fn build_control(ui: &mut Ui, w: f32) -> f32 {
    let (s, pad, gap) = (ui.s, 14.0 * ui.s, 10.0 * ui.s);
    let inner = w - 2.0 * pad;
    let mut y = pad;
    let mods = config::current().control.modules.clone();
    let on_m = |m: &str| mods.iter().any(|x| x.eq_ignore_ascii_case(m));

    // Konektivitas (kiri) + Jangan Ganggu / Proyeksi (kanan); hanya modul yang dipilih di Pengaturan.
    let (lw, row_h) = (inner * 0.58, 52.0 * s);
    let wifi_on = radios::is_on(Which::Wifi);
    let bt_on = radios::is_on(Which::Bluetooth);
    let air = control::airplane();
    let mut rows: Vec<(Icon, bool, &str, String, Hit, Hit)> = Vec::new();
    if on_m("wifi") {
        let ssid = WIFI.with(|v| v.borrow().nets.iter().find(|n| n.connected).map(|n| n.ssid.clone()));
        let sub = match wifi_on {
            Some(true) => ssid.unwrap_or_else(|| "Menyala".into()),
            Some(false) => "Mati".into(),
            None => "Tidak tersedia".into(),
        };
        rows.push((Icon::Wifi, wifi_on == Some(true), "Wi-Fi", sub, Hit::CcWifi, Hit::CcWifiOpen));
    }
    if on_m("bluetooth") {
        let dev = radios::bt_devices().into_iter().find(|d| d.connected).map(|d| d.name);
        let sub = match bt_on {
            Some(true) => dev.unwrap_or_else(|| "Menyala".into()),
            Some(false) => "Mati".into(),
            None => "Tidak tersedia".into(),
        };
        rows.push((Icon::Bluetooth, bt_on == Some(true), "Bluetooth", sub, Hit::CcBt, Hit::CcBtOpen));
    }
    if on_m("airplane") {
        rows.push((Icon::Airplane, air, "Mode Pesawat", if air { "Menyala".into() } else { "Mati".into() }, Hit::CcAirplane, Hit::CcAirplane));
    }
    let mut tiles: Vec<(Icon, bool, &str, &str, Hit)> = Vec::new();
    if on_m("dnd") {
        tiles.push((Icon::Moon, control::dnd(), "Jangan", "Ganggu", Hit::CcDnd));
    }
    if on_m("cast") {
        tiles.push((Icon::Monitor, false, "Proyeksi", "", Hit::Uri("ms-settings-connectabledevices:devicediscovery")));
    }
    let have_card = !rows.is_empty();
    let card_h = if have_card { row_h * rows.len().max(2) as f32 + 12.0 * s } else { 0.0 };
    if have_card {
        let card = (pad, y, if tiles.is_empty() { inner } else { lw }, card_h);
        ui.round(card, 14.0, CARD);
        let n = rows.len();
        let top = y + (card_h - n as f32 * row_h) / 2.0;
        for (i, (icon, on, title, sub, toggle, open)) in rows.into_iter().enumerate() {
            cc_row(ui, (card.0, top + i as f32 * row_h, card.2, row_h), icon, on, title, &sub, toggle, open);
        }
    }
    let drew_tiles = !tiles.is_empty();
    if !tiles.is_empty() {
        let n = tiles.len() as f32;
        if have_card {
            let (tx, tw) = (pad + lw + gap, inner - lw - gap);
            let th = (card_h - gap * (n - 1.0)) / n;
            for (i, (icon, on, l1, l2, h)) in tiles.into_iter().enumerate() {
                cc_tile(ui, (tx, y + i as f32 * (th + gap), tw, th), icon, on, l1, l2, h);
            }
        } else {
            let tw = (inner - gap * (n - 1.0)) / n;
            for (i, (icon, on, l1, l2, h)) in tiles.into_iter().enumerate() {
                cc_tile(ui, (pad + i as f32 * (tw + gap), y, tw, 64.0 * s), icon, on, l1, l2, h);
            }
        }
    }
    if have_card {
        y += card_h + gap;
    } else if drew_tiles {
        y += 64.0 * s + gap;
    }

    // Kecerahan (hanya layar internal) dan suara.
    if let (true, Some(b)) = (on_m("display"), control::brightness()) {
        cc_slider(ui, (pad, y, inner, 70.0 * s), "Layar", Icon::Sun, b as f32 / 100.0, Hit::CcBright, None);
        y += 70.0 * s + gap;
    }
    if on_m("sound") {
        let v = status::volume().unwrap_or(status::Volume { level: 0.0, muted: false });
        let lvl = if v.muted { 0.0 } else { v.level };
        let sp = if v.muted || lvl <= 0.001 { 0 } else if lvl < 0.34 { 1 } else if lvl < 0.67 { 2 } else { 3 };
        cc_slider(ui, (pad, y, inner, 70.0 * s), "Suara", Icon::Speaker(sp), lvl, Hit::CcVol, Some(Hit::VolMute));
        y += 70.0 * s + gap;
    }

    // Now Playing.
    if on_m("media") {
        y = cc_now_playing(ui, pad, y, inner, gap);
    }

    // Baris bawah: baterai + pengaturan HolmBar.
    let fy = y + 2.0 * s;
    if let Some((pct, charging)) = crate::bar::battery() {
        ui.ic(Icon::Battery(pct), (pad + 4.0 * s, fy + 3.0 * s, 20.0 * s, 20.0 * s), if charging { 0xFF30D158 } else if pct <= 20 { 0xFFFF453A } else { WHITE }, 0xFF1E1E22);
        ui.text(&format!("{pct}%{}", if charging { "  ·  Mengisi" } else { "" }), (pad + 30.0 * s, fy, 150.0 * s, 26.0 * s), 12.0, false, DIM, Align::Near);
    }
    let gr = (pad + inner - 28.0 * s, fy, 28.0 * s, 26.0 * s);
    if ui.hovered(Hit::CcSettings) {
        ui.round(gr, 8.0, 0x26FFFFFF);
    }
    ui.ic(Icon::Gear, (gr.0 + 6.0 * s, gr.1 + 4.0 * s, 18.0 * s, 18.0 * s), DIM, 0xFF1E1E22);
    ui.hit(gr, Hit::CcSettings);
    y + 26.0 * s + pad
}

/// Kartu Now Playing; mengembalikan y berikutnya.
fn cc_now_playing(ui: &mut Ui, pad: f32, y: f32, inner: f32, gap: f32) -> f32 {
    let s = ui.s;
    let info = media::info();
    let np = (pad, y, inner, 64.0 * s);
    ui.round(np, 14.0, CARD);
    let asz = 44.0 * s;
    let art_r = (np.0 + 10.0 * s, np.1 + (np.3 - asz) / 2.0, asz, asz);
    if let Some(c) = ui.c {
        let want = info.thumb.as_ref().map(|t| std::sync::Arc::as_ptr(t) as usize);
        CC_ART.with(|a| {
            let mut a = a.borrow_mut();
            match (want, a.as_ref().map(|(p, _)| *p)) {
                (Some(p), Some(q)) if p == q => {}
                (Some(p), _) => *a = info.thumb.as_ref().and_then(|t| Image::from_bytes(t)).map(|i| (p, i)),
                (None, _) => *a = None,
            }
            match a.as_ref() {
                Some((_, img)) => c.draw_image(img, art_r.0, art_r.1, art_r.2, art_r.3, 8.0 * s, 1.0),
                None => {
                    c.fill_round_rect(art_r.0, art_r.1, art_r.2, art_r.3, (8.0 * s, 8.0 * s, 8.0 * s, 8.0 * s), 0xFF3A3A40);
                    icons::draw(c, Icon::Music, (art_r.0 + 11.0 * s, art_r.1 + 11.0 * s, 22.0 * s, 22.0 * s), 0xFF8E8E98, 0xFF3A3A40);
                }
            }
        });
    }
    let bx = np.0 + np.2 - 10.0 * s;
    let (bd, by) = (34.0 * s, np.1 + np.3 / 2.0);
    let tx = art_r.0 + asz + 10.0 * s;
    let tw = (bx - 2.0 * bd - 6.0 * s - tx).max(40.0 * s);
    if info.active {
        ui.text(if info.title.is_empty() { "Tanpa judul" } else { &info.title }, (tx, by - 18.0 * s, tw, 18.0 * s), 13.0, true, WHITE, Align::Near);
        ui.text(&info.artist, (tx, by + 1.0 * s, tw, 16.0 * s), 11.0, false, DIM, Align::Near);
        for (i, (h, icon)) in [(Hit::CcPlay, if info.playing { Icon::Pause } else { Icon::Play }), (Hit::CcNext, Icon::Next)].into_iter().enumerate() {
            let r = (bx - (2 - i) as f32 * bd, by - bd / 2.0, bd, bd);
            if ui.hovered(h) {
                ui.circle(r.0 + bd / 2.0, r.1 + bd / 2.0, bd / 2.0, 0x26FFFFFF);
            }
            let k = 15.0 * s;
            ui.ic(icon, (r.0 + (bd - k) / 2.0, r.1 + (bd - k) / 2.0, k, k), WHITE, CARD_BG);
            ui.hit(r, h);
        }
    } else {
        ui.text("Tidak ada media", (tx, by - 9.0 * s, tw + bd * 2.0, 18.0 * s), 13.0, false, DIM, Align::Near);
    }
    y + np.3 + gap
}

/// Daftar ikon tray berubah: segarkan popup bila sedang terbuka.
pub fn on_tray_changed() {
    if is_open(Kind::Tray) {
        render();
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

/// Ikon aplikasi dari nama tampilannya (mis. nama pengirim notifikasi), dicocokkan dengan Start Menu.
pub fn app_icon(name: &str) -> Option<Image> {
    let q = name.trim().to_lowercase();
    if q.len() < 2 {
        return None;
    }
    ensure_apps();
    let path = APPS.with(|a| {
        a.borrow()
            .0
            .iter()
            .filter(|x| x.lower == q || x.lower.starts_with(&q) || q.starts_with(&x.lower) || x.lower.contains(&q))
            .min_by_key(|x| (if x.lower == q { 0 } else { 1 }, x.lower.len()))
            .map(|x| x.path.clone())
    })?;
    let w: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let mut info = SHFILEINFOW::default();
    unsafe {
        let r = SHGetFileInfoW(PCWSTR(w.as_ptr()), Default::default(), Some(&mut info), std::mem::size_of::<SHFILEINFOW>() as u32, SHGFI_ICON | SHGFI_LARGEICON);
        if r == 0 || info.hIcon.0.is_null() {
            return None;
        }
        let img = Image::from_hicon(info.hIcon);
        let _ = DestroyIcon(info.hIcon);
        img
    }
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
        Kind::Tray => (crate::tray::split().1.len().clamp(1, TRAY_COLS) as f32 * TRAY_CELL + 24.0).max(220.0),
        Kind::Control => 344.0,
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
        let class: Vec<u16> = "HolmPopupClass".encode_utf16().chain(std::iter::once(0)).collect();
        let h = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            PCWSTR(class.as_ptr()),
            windows::core::w!("HolmBar Popup"),
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
                drag_hit: None,
                cal_off: 0,
                query: String::new(),
                sel: 0,
                last_closed: None,
            })
        });
        Some(h)
    }
}

/// Popup apa pun sedang tampil (dipakai penjaga z-order notch agar tidak menutupinya).
pub fn any_open() -> bool {
    with(|p| unsafe { IsWindowVisible(p.hwnd) }.as_bool()).unwrap_or(false)
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
        Kind::Control => {
            WIFI.with(|v| v.borrow_mut().nets = status::networks());
            status::wifi_scan();
            radios::refresh();
            radios::refresh_bt_devices();
            control::refresh_brightness();
            unsafe {
                SetTimer(Some(h), TIMER_RESCAN, 2500, None);
            }
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
    } else if is_open(Kind::Control) && !with(|p| p.dragging).unwrap_or(false) {
        WIFI.with(|v| v.borrow_mut().nets = status::networks());
        render();
    }
}

fn hit_at(x: f32, y: f32) -> Option<Hit> {
    with(|p| p.rects.iter().rev().find(|(r, _)| x >= r.0 && x <= r.0 + r.2 && y >= r.1 && y <= r.1 + r.3).map(|(_, h)| *h)).flatten()
}

fn slider_value(x: f32, hit: Hit) -> Option<f32> {
    with(|p| {
        p.rects.iter().find(|(_, h)| *h == hit).map(|(r, _)| {
            // Slider popup Suara punya padding 8 DIP di rect-nya; slider Control Center memakai rect trek apa adanya.
            let pad = if hit == Hit::VolSlider { 8.0 * p.mon.scale } else { 0.0 };
            let (tx, tw) = (r.0 + pad, r.2 - 2.0 * pad);
            ((x - tx) / tw).clamp(0.0, 1.0)
        })
    })
    .flatten()
}

/// Terapkan nilai slider saat diklik/diseret.
fn drag_apply(hit: Option<Hit>, x: f32) {
    let Some(h) = hit else { return };
    let Some(v) = slider_value(x, h) else { return };
    match h {
        Hit::VolSlider => status::change_volume(v),
        Hit::CcVol => {
            status::change_volume(v);
            render();
        }
        Hit::CcBright => {
            control::set_brightness((v * 100.0).round().max(3.0) as u32);
            render();
        }
        _ => {}
    }
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
        Hit::TrayIcon(id) => {
            hide();
            let mut pt = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut pt);
            }
            crate::tray::click(id, false, false, (pt.x, pt.y));
        }
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
        Hit::CcWifi => activate(Hit::WifiToggle),
        Hit::CcBt => activate(Hit::BtToggle),
        Hit::CcWifiOpen | Hit::CcBtOpen => {
            if let Some((ax, mon)) = with(|p| (p.anchor_x, p.mon)) {
                toggle(if h == Hit::CcWifiOpen { Kind::Wifi } else { Kind::Bluetooth }, ax, mon);
            }
        }
        Hit::CcAirplane => control::set_airplane(!control::airplane()),
        Hit::CcDnd => {
            control::set_dnd(!control::dnd());
            render();
        }
        Hit::CcPlay => {
            media::send(media::Command::TogglePlayPause);
        }
        Hit::CcNext => {
            media::send(media::Command::Next);
        }
        Hit::CcSettings => {
            hide();
            crate::handle_command(crate::IDM_SETTINGS);
        }
        Hit::CcBright | Hit::CcVol => {}
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
                let h = with(|p| p.drag_hit).flatten();
                drag_apply(h, x);
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
            let h = hit_at(x, y);
            if matches!(h, Some(Hit::VolSlider | Hit::CcVol | Hit::CcBright)) {
                with(|p| {
                    p.dragging = true;
                    p.drag_hit = h;
                });
                unsafe {
                    SetCapture(hwnd);
                }
                drag_apply(h, x);
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            let (x, y) = xy(lparam);
            if let Some(Hit::TrayIcon(id)) = hit_at(x, y) {
                hide();
                let mut pt = POINT::default();
                unsafe {
                    let _ = GetCursorPos(&mut pt);
                }
                crate::tray::click(id, true, false, (pt.x, pt.y));
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
                    if is_open(Kind::Wifi) || is_open(Kind::Control) {
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

#[cfg(test)]
mod icon_probe {
    #[test]
    #[ignore]
    fn start_menu_match_for_powershell() {
        crate::canvas::startup();
        super::ensure_apps();
        let q = "windows powershell 5.1";
        super::APPS.with(|a| {
            let a = a.borrow();
            println!("apps di Start Menu: {}", a.0.len());
            for x in a.0.iter().filter(|x| x.lower == q || x.lower.starts_with(q) || q.starts_with(&x.lower) || x.lower.contains(q)) {
                println!("  cocok: '{}' -> {}", x.name, x.path);
            }
        });
        println!("app_icon = {}", super::app_icon("Windows PowerShell 5.1").is_some());
    }
}
