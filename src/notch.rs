//! Notch / dynamic island ala MacBook di tengah atas layar.
//! - Bentuk "notch" (menyatu dengan bar, telinga cekung) atau "pill" (pulau melayang).
//! - Ringkas: live activity (sampul + visualizer musik, timer yang berjalan).
//! - Terbuka: tab Media, Kalender, Timer/Stopwatch, Sistem; ikon tab di strip atas, ⚙ ke Pengaturan.
//! - Animasi spring (sedikit memantul) saat membuka/menutup; kartu notifikasi Windows.
//! Timer gambar ulang hanya hidup saat ada yang bergerak (lihat `sync_tick`).

use crate::canvas::{Align, Canvas, Font, Image};
use crate::config;
use crate::icons::{self, Icon};
use crate::lyrics::{self, Lyrics};
use crate::media::{self, Command, MediaInfo};
use crate::monitors::Mon;
use crate::notifications::{Kind, Toast};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::*;
use windows::Win32::System::SystemInformation::{GetLocalTime, GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::GetSystemTimes;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const TIMER_ANIM: usize = 1;
const TIMER_CLOSE: usize = 2;
const TIMER_TICK: usize = 3;
const TIMER_NOTIF: usize = 4;
/// Penjaga z-order: AppBar bar/Dock sering menaikkan bar ke atas notch (notch jadi tertutup dan tidak bisa di-hover).
const TIMER_Z: usize = 5;
const FRAME_MS: u32 = 16;

const TIMER_COLOR: u32 = 0xFFFF9F0A;
const DIM: u32 = 0xFF9E9EA8;
const GREEN: u32 = 0xFF30D158;
const RED: u32 = 0xFFFF453A;
const CYAN: u32 = 0xFF5AC8FA;

type Rect = (f32, f32, f32, f32);

#[derive(Clone, Copy, PartialEq, Debug)]
enum Tab {
    Media,
    Calendar,
    Timer,
    System,
}

impl Tab {
    fn parse(s: &str) -> Option<Tab> {
        match s.trim().to_lowercase().as_str() {
            "media" | "music" => Some(Tab::Media),
            "calendar" | "kalender" => Some(Tab::Calendar),
            "timer" | "stopwatch" => Some(Tab::Timer),
            "system" | "sistem" => Some(Tab::System),
            _ => None,
        }
    }
    fn icon(self) -> Icon {
        match self {
            Tab::Media => Icon::Music,
            Tab::Calendar => Icon::Calendar,
            Tab::Timer => Icon::Stopwatch,
            Tab::System => Icon::Chart,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Tab::Media => tr("Beranda", "Home"),
            Tab::Calendar => tr("Kalender", "Calendar"),
            Tab::Timer => "Timer",
            Tab::System => tr("Sistem", "System"),
        }
    }
}

/// Teks sesuai bahasa config (`general.language`).
fn tr(id: &'static str, en: &'static str) -> &'static str {
    if config::current().general.language == "en" {
        en
    } else {
        id
    }
}

/// Glyph dari font ikon Windows (Segoe Fluent Icons, jatuh ke MDL2 Assets).
const GLYPH_FONT: &str = "Segoe Fluent Icons";
const G_PHONE: &str = "\u{E717}";
const G_HANGUP: &str = "\u{E778}";
const G_HEADPHONE: &str = "\u{E7F6}";

/// Tab aktif dari config (urutan dipertahankan, duplikat & nama tak dikenal dibuang).
fn tabs() -> Vec<Tab> {
    let mut v: Vec<Tab> = Vec::new();
    for t in config::current().notch.tabs.iter().filter_map(|s| Tab::parse(s)) {
        if !v.contains(&t) {
            v.push(t);
        }
    }
    if v.is_empty() {
        v.push(Tab::Media);
    }
    v
}

/// Item yang bisa tampil di notch ringkas (kiri/kanan badan notch).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Slot {
    Artwork,
    Visualizer,
    Title,
    Timer,
    Battery,
    Clock,
    Date,
    Cpu,
}

impl Slot {
    fn parse(s: &str) -> Option<Slot> {
        match s.trim().to_lowercase().as_str() {
            "artwork" | "art" | "cover" => Some(Slot::Artwork),
            "visualizer" | "waveform" => Some(Slot::Visualizer),
            "title" | "song" => Some(Slot::Title),
            "timer" | "stopwatch" => Some(Slot::Timer),
            "battery" => Some(Slot::Battery),
            "clock" | "time" => Some(Slot::Clock),
            "date" => Some(Slot::Date),
            "cpu" => Some(Slot::Cpu),
            _ => None,
        }
    }
}

/// (kiri, kanan) dari config; item pertama = paling luar. Item yang muncul di dua sisi hanya dipakai sekali.
fn slots() -> (Vec<Slot>, Vec<Slot>) {
    let n = &config::current().notch;
    let mut seen: Vec<Slot> = Vec::new();
    let mut take = |list: &[String]| {
        let mut out = Vec::new();
        for sl in list.iter().filter_map(|x| Slot::parse(x)) {
            if !seen.contains(&sl) {
                seen.push(sl);
                out.push(sl);
            }
        }
        out
    };
    let left = take(&n.compact_left);
    let right = take(&n.compact_right);
    (left, right)
}

fn has_slot(sl: Slot) -> bool {
    let (l, r) = slots();
    l.contains(&sl) || r.contains(&sl)
}

fn pill() -> bool {
    config::current().notch.style.eq_ignore_ascii_case("pill")
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Tab(Tab),
    Settings,
    Prev,
    Play,
    Next,
    Seek,
    WeekPrev,
    WeekNext,
    WeekToday,
    ClockToggle,
    ClockReset,
    Mode(bool),
    Preset(u32),
    CallAccept,
    CallDecline,
    NotifClose,
}

// ---------- Timer / stopwatch (dibagi semua notch) ----------

#[derive(Default)]
struct Clock {
    stopwatch: bool,
    since: Option<Instant>,
    acc: Duration,
    target: Duration,
    alarm_until: Option<Instant>,
}

impl Clock {
    fn running(&self) -> bool {
        self.since.is_some()
    }
    fn elapsed(&self) -> Duration {
        self.acc + self.since.map(|s| s.elapsed()).unwrap_or_default()
    }
    fn remaining(&self) -> Duration {
        self.target.saturating_sub(self.elapsed())
    }
    /// Ada yang layak ditampilkan sebagai live activity.
    fn active(&self) -> bool {
        self.running() || self.alarm() || self.elapsed() > Duration::ZERO
    }
    fn alarm(&self) -> bool {
        self.alarm_until.map(|t| Instant::now() < t).unwrap_or(false)
    }
    fn text(&self, tenths: bool) -> String {
        let d = if self.stopwatch { self.elapsed() } else if self.running() || self.elapsed() > Duration::ZERO { self.remaining() } else { self.target };
        // Hitung mundur dibulatkan ke atas agar "0:00" tepat saat habis.
        let ms = if self.stopwatch { d.as_millis() as u64 } else { (d.as_millis() as u64).div_ceil(1000) * 1000 };
        let (h, m, s) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60);
        let base = if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") };
        if tenths && self.stopwatch && h == 0 {
            format!("{base}.{}", ms / 100 % 10)
        } else {
            base
        }
    }
    fn toggle(&mut self) {
        self.alarm_until = None;
        match self.since.take() {
            Some(s) => self.acc += s.elapsed(),
            None => {
                if !self.stopwatch && self.remaining().is_zero() {
                    self.acc = Duration::ZERO;
                    if self.target.is_zero() {
                        self.target = Duration::from_secs(5 * 60);
                    }
                }
                self.since = Some(Instant::now());
            }
        }
    }
    fn reset(&mut self) {
        self.since = None;
        self.acc = Duration::ZERO;
        self.alarm_until = None;
    }
}

thread_local! {
    static CLOCK: RefCell<Clock> = RefCell::new(Clock { target: Duration::from_secs(5 * 60), ..Default::default() });
    static CPU: RefCell<Option<(u64, u64)>> = const { RefCell::new(None) };
    static WINS: RefCell<HashMap<isize, State>> = RefCell::new(HashMap::new());
}

fn clock<R>(f: impl FnOnce(&mut Clock) -> R) -> R {
    CLOCK.with(|c| f(&mut c.borrow_mut()))
}

/// Periksa hitung mundur; true bila baru saja habis (bunyikan sekali).
fn check_alarm() -> bool {
    clock(|c| {
        if !c.stopwatch && c.running() && c.remaining().is_zero() {
            c.since = None;
            c.acc = c.target;
            c.alarm_until = Some(Instant::now() + Duration::from_secs(8));
            true
        } else {
            false
        }
    })
}

// ---------- Statistik sistem ----------

#[derive(Clone, Copy, Default)]
struct Sys {
    cpu: f32,
    ram: f32,
    ram_used_gb: f32,
    ram_total_gb: f32,
    battery: Option<(u8, bool)>,
}

fn ft(f: FILETIME) -> u64 {
    ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64
}

fn sample_sys(prev: Sys) -> Sys {
    let mut out = Sys { battery: crate::bar::battery(), ..prev };
    unsafe {
        let (mut i, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
        if GetSystemTimes(Some(&mut i), Some(&mut k), Some(&mut u)).is_ok() {
            let (idle, total) = (ft(i), ft(k) + ft(u));
            if let Some((pi, pt)) = CPU.with(|c| c.replace(Some((idle, total)))) {
                let (di, dt) = (idle.saturating_sub(pi), total.saturating_sub(pt));
                if dt > 0 {
                    out.cpu = (1.0 - di as f32 / dt as f32).clamp(0.0, 1.0);
                }
            }
        }
        let mut m = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
        if GlobalMemoryStatusEx(&mut m).is_ok() {
            let gb = 1024.0 * 1024.0 * 1024.0;
            out.ram_total_gb = m.ullTotalPhys as f32 / gb;
            out.ram_used_gb = (m.ullTotalPhys - m.ullAvailPhys) as f32 / gb;
            out.ram = out.ram_used_gb / out.ram_total_gb.max(0.01);
        }
    }
    out
}

// ---------- Tanggal ----------

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { yoe + era * 400 + 1 } else { yoe + era * 400 }, m, d)
}

/// 0 = Senin ... 6 = Minggu.
fn weekday(days: i64) -> usize {
    (days + 3).rem_euclid(7) as usize
}

struct Names {
    days_short: [&'static str; 7],
    days_long: [&'static str; 7],
    months: [&'static str; 12],
}

fn names() -> Names {
    if config::current().general.language == "en" {
        Names {
            days_short: ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"],
            days_long: ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"],
            months: ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"],
        }
    } else {
        Names {
            days_short: ["Sen", "Sel", "Rab", "Kam", "Jum", "Sab", "Min"],
            days_long: ["Senin", "Selasa", "Rabu", "Kamis", "Jumat", "Sabtu", "Minggu"],
            months: ["Januari", "Februari", "Maret", "April", "Mei", "Juni", "Juli", "Agustus", "September", "Oktober", "November", "Desember"],
        }
    }
}

// ---------- State per notch ----------

struct State {
    mon: Mon,
    target_open: bool,
    /// Posisi spring: 0 = ringkas, 1 = terbuka (boleh sedikit melewati 1 saat memantul).
    x: f32,
    v: f32,
    last: Instant,
    tracking: bool,
    art: Option<(usize, Image)>,
    tint: Option<u32>,
    /// Spring kartu notifikasi (0 = tidak ada, 1 = kartu penuh) dan statusnya.
    nt: f32,
    nv: f32,
    n_target: bool,
    notif: Option<Toast>,
    notif_img: Option<Image>,
    queue: VecDeque<Toast>,
    hover: bool,
    tab: Tab,
    week_off: i64,
    rects: Vec<(Rect, Hit)>,
    hover_hit: Option<Hit>,
    seek_bar: Rect,
    sys: Sys,
    sys_at: Option<Instant>,
    tick_ms: u32,
    born: Instant,
    /// Kanvas dipakai ulang antar frame; hanya diperbesar bila notch melebihi ukurannya.
    canvas: Option<Canvas>,
    /// Notch ringkas sedang menampilkan wajah idle (mata berkedip), atau kartu panggilan (tombol berdenyut).
    idle: bool,
    /// Ikon + nama aplikasi sumber media, di-cache per AppUserModelID.
    source: Option<(String, Option<(String, Image)>)>,
    /// Tombol kartu notifikasi (hit-test saat kartu tampil).
    notif_rects: Vec<(Rect, Hit)>,
}

fn with<R>(hwnd: HWND, f: impl FnOnce(&mut State) -> R) -> Option<R> {
    WINS.with(|w| w.borrow_mut().get_mut(&(hwnd.0 as isize)).map(f))
}

pub fn attach(hwnd: HWND, mon: Mon) {
    unsafe {
        SetTimer(Some(hwnd), TIMER_Z, 150, None);
    }
    CHARGING.with(|c| {
        if c.get().is_none() {
            c.set(crate::bar::battery().map(|b| b.1));
        }
    });
    let tab = Tab::parse(&config::current().notch.default_tab).filter(|t| tabs().contains(t)).unwrap_or(tabs()[0]);
    WINS.with(|w| {
        w.borrow_mut().insert(
            hwnd.0 as isize,
            State {
                mon,
                target_open: false,
                x: 0.0,
                v: 0.0,
                last: Instant::now(),
                tracking: false,
                art: None,
                tint: None,
                nt: 0.0,
                nv: 0.0,
                n_target: false,
                notif: None,
                notif_img: None,
                queue: VecDeque::new(),
                hover: false,
                tab,
                week_off: 0,
                rects: Vec::new(),
                hover_hit: None,
                seek_bar: (0.0, 0.0, 0.0, 0.0),
                sys: Sys::default(),
                sys_at: None,
                tick_ms: 0,
                born: Instant::now(),
                canvas: None,
                idle: false,
                source: None,
                notif_rects: Vec::new(),
            },
        );
    });
}

pub fn detach(hwnd: HWND) {
    WINS.with(|w| w.borrow_mut().remove(&(hwnd.0 as isize)));
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn fmt_time(ms: u64) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Dimensi akhir (badan ringkas, badan terbuka, tinggi ringkas, tinggi terbuka) dalam piksel.
fn dims(s: f32) -> (f32, f32, f32, f32) {
    let n = &config::current().notch;
    let ch = n.compact_height.clamp(18, 48) as f32 * s;
    let eh = (n.expanded_height as f32 * s).max(ch + 96.0 * s);
    (n.compact_width as f32 * s, n.expanded_width.max(360) as f32 * s, ch, eh)
}

fn is_settled_open(hwnd: HWND) -> bool {
    with(hwnd, |st| st.target_open && (st.x - 1.0).abs() < 0.01 && st.v.abs() < 0.05).unwrap_or(false)
}

/// Pilih interval gambar ulang sesuai apa yang sedang bergerak; 0 = mati.
fn sync_tick(hwnd: HWND) {
    let info = media::info();
    let Some((open, tab, anim, cur, call, idle, born, battery)) = with(hwnd, |st| {
        let call = st.n_target && st.notif.as_ref().map(|t| t.kind == Kind::Call).unwrap_or(false);
        (st.target_open && st.x > 0.9, st.tab, st.v.abs() > 0.0 || (st.nt > 0.0 && !call), st.tick_ms, call, st.idle && st.x < 0.05 && st.nt < 0.01, st.born, st.sys.battery)
    }) else {
        return;
    };
    let (running, alarm, active) = clock(|c| (c.running(), c.alarm(), c.active()));
    let mut want = u32::MAX;
    let mut need = |ms: u32| want = want.min(ms);
    if alarm {
        need(60);
    }
    if call {
        need(33); // denyut tombol terima
    }
    if idle && crate::idle::enabled() {
        let ms = crate::idle::next_tick_ms(born.elapsed().as_secs_f32(), battery);
        if ms > 0 {
            need(ms);
        }
    }
    if open {
        match tab {
            Tab::Media if info.playing => need(250),
            Tab::Timer if running => need(100),
            Tab::System => need(1000),
            Tab::Calendar => need(5000),
            _ => {}
        }
    } else {
        if info.active && info.playing && has_slot(Slot::Visualizer) {
            need(70);
        }
        if has_slot(Slot::Timer) {
            if running {
                need(250);
            } else if active {
                need(1000);
            }
        }
        if has_slot(Slot::Cpu) {
            need(2000);
        }
        if has_slot(Slot::Clock) || has_slot(Slot::Date) {
            need(5000);
        }
        if has_slot(Slot::Battery) || info.active && has_slot(Slot::Title) {
            need(30000);
        }
    }
    let want = if want == u32::MAX || anim && want > 1000 { 0 } else { want };
    if want == cur {
        return;
    }
    with(hwnd, |st| st.tick_ms = want);
    unsafe {
        if want == 0 {
            let _ = KillTimer(Some(hwnd), TIMER_TICK);
        } else {
            SetTimer(Some(hwnd), TIMER_TICK, want, None);
        }
    }
}

/// Dipanggil saat info media/lirik berubah.
pub fn on_media(hwnd: HWND) {
    redraw(hwnd);
}

// ---------- Gambar ----------

struct Ctx<'a> {
    c: &'a Canvas,
    s: f32,
    font: String,
    fade: f32,
    tint: u32,
    bg: u32,
    hover: Option<Hit>,
    rects: Vec<(Rect, Hit)>,
}

/// Varian optis Segoe UI Variable: "Display" untuk teks besar (>= 18 DIP), "Text" untuk teks kecil.
fn face(base: &str, size: f32) -> String {
    if size >= 18.0 && base.eq_ignore_ascii_case("Segoe UI Variable Text") {
        "Segoe UI Variable Display".into()
    } else {
        base.to_string()
    }
}

impl Ctx<'_> {
    fn a(&self, col: u32) -> u32 {
        config::with_alpha(col, self.fade * ((col >> 24) as f32 / 255.0))
    }
    fn text(&self, t: &str, r: Rect, size: f32, bold: bool, col: u32, al: Align) {
        self.c.text(t, &Font::new(&face(&self.font, size), size * self.s, bold), r, self.a(col), al);
    }
    /// Glyph font ikon Windows di tengah kotak `r`.
    fn glyph(&self, g: &str, r: Rect, size: f32, col: u32) {
        self.c.text(g, &Font::new(GLYPH_FONT, size * self.s, false), r, self.a(col), Align::Center);
    }
    /// Ikon filled berukuran `size` DIP di tengah kotak `r`.
    fn icon(&self, ic: Icon, r: Rect, size: f32, col: u32) {
        let k = size * self.s;
        icons::draw(self.c, ic, (r.0 + (r.2 - k) / 2.0, r.1 + (r.3 - k) / 2.0, k, k), self.a(col), self.bg);
    }
    fn round(&self, r: Rect, rad: f32, col: u32) {
        let k = rad * self.s;
        self.c.fill_round_rect(r.0, r.1, r.2, r.3, (k, k, k, k), self.a(col));
    }
    fn hit(&mut self, r: Rect, h: Hit) {
        self.rects.push((r, h));
    }
    fn hovered(&self, h: Hit) -> bool {
        self.hover == Some(h)
    }
    /// Tombol bulat dengan glyph; latar muncul saat hover.
    fn button(&mut self, cx: f32, cy: f32, d: f32, ic: Icon, size: f32, col: u32, h: Hit) {
        let r = (cx - d / 2.0, cy - d / 2.0, d, d);
        if self.hovered(h) {
            self.c.fill_ellipse(r.0, r.1, r.2, r.3, self.a(0x26FFFFFF));
        }
        self.icon(ic, r, size, col);
        self.hit(r, h);
    }
}

fn lyric_line(info: &MediaInfo) -> Option<String> {
    if !lyrics::ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    match lyrics::get(info.key)? {
        Lyrics::Synced(lines) => {
            let i = lyrics::current_line(&lines, info.position_ms())?;
            Some(if lines[i].1.is_empty() { "\u{266A}".into() } else { lines[i].1.clone() })
        }
        Lyrics::Plain(lines) => {
            let nonempty: Vec<&String> = lines.iter().filter(|x| !x.is_empty()).collect();
            let frac = if info.duration_ms > 0 { info.position_ms() as f32 / info.duration_ms as f32 } else { 0.0 };
            let i = ((frac * nonempty.len() as f32) as usize).min(nonempty.len().saturating_sub(1));
            nonempty.get(i).map(|s| s.to_string())
        }
        Lyrics::Instrumental => Some("Instrumental".into()),
        _ => None,
    }
}

/// Tinggi isi tiap tab (sebelum dipusatkan vertikal).
const CONTENT_H: f32 = 84.0;

/// Tab Beranda: media (sampul + badge aplikasi sumber + kontrol) | kalender mini | pintasan Timer.
#[allow(clippy::too_many_arguments)]
fn draw_media(x: &mut Ctx, art: Option<&Image>, source: Option<&(String, Image)>, info: &MediaInfo, bx: f32, bw: f32, y0: f32, has_cal: bool, has_timer: bool) -> Rect {
    let s = x.s;
    let pad = 18.0 * s;
    let inner = bw - 2.0 * pad;
    let tile_w = 92.0 * s;
    let cal_w = 176.0 * s;
    let media_w = inner - cal_w - tile_w;
    let ax = bx + pad;
    let media_end = ax + media_w - 14.0 * s;

    // --- Media ---
    let art_s = 76.0 * s;
    match art {
        Some(img) => x.c.draw_image(img, ax, y0, art_s, art_s, 14.0 * s, x.fade),
        None => {
            x.round((ax, y0, art_s, art_s), 14.0, 0xFF232328);
            x.icon(Icon::Music, (ax, y0, art_s, art_s), 30.0, if info.active { x.tint } else { 0xFF55555F });
        }
    }
    // Badge aplikasi sumber di sudut kanan bawah sampul (seperti logo Apple Music/Spotify).
    if let (true, Some((_, icon))) = (info.active, source) {
        let b = 26.0 * s;
        let (bxx, byy) = (ax + art_s - b + 6.0 * s, y0 + art_s - b + 6.0 * s);
        x.round((bxx - 2.0 * s, byy - 2.0 * s, b + 4.0 * s, b + 4.0 * s), 9.0, x.bg | 0xFF00_0000);
        x.c.draw_image(icon, bxx, byy, b, b, 7.0 * s, x.fade);
    }
    let tx = ax + art_s + 14.0 * s;
    let tw = (media_end - tx).max(40.0 * s);
    let (title, artist) = if info.active {
        (if info.title.is_empty() { tr("Tanpa judul", "Untitled").to_string() } else { info.title.clone() }, info.artist.clone())
    } else {
        (tr("Tidak ada media", "Nothing playing").to_string(), tr("Putar musik untuk mengontrolnya", "Play something to control it here").to_string())
    };
    x.text(&title, (tx, y0 + 2.0 * s, tw, 20.0 * s), 15.0, true, 0xFFFFFFFF, Align::Near);
    x.text(&artist, (tx, y0 + 22.0 * s, tw, 17.0 * s), 12.5, false, DIM, Align::Near);
    if info.active {
        match lyric_line(info) {
            Some(line) => x.text(&line, (tx, y0 + 39.0 * s, tw, 16.0 * s), 12.0, false, 0xD8FFFFFF, Align::Near),
            None => {
                if let Some((name, _)) = source {
                    x.text(&format!("{} {name}", tr("dari", "from")), (tx, y0 + 39.0 * s, tw, 16.0 * s), 11.5, false, x.tint, Align::Near);
                }
            }
        }
    }

    // Kontrol di bawah teks, lalu progres di sebelahnya (klik untuk lompat).
    let cy = y0 + art_s - 12.0 * s;
    let ctrl = if info.active { 0xFFFFFFFF } else { 0xFF55555F };
    let play_icon = if info.playing { Icon::Pause } else { Icon::Play };
    x.button(tx + 11.0 * s, cy, 28.0 * s, Icon::Prev, 15.0, ctrl, Hit::Prev);
    x.button(tx + 44.0 * s, cy, 32.0 * s, play_icon, 20.0, ctrl, Hit::Play);
    x.button(tx + 77.0 * s, cy, 28.0 * s, Icon::Next, 15.0, ctrl, Hit::Next);
    let mut bar = (0.0, 0.0, 0.0, 0.0);
    let bar_x = tx + 100.0 * s;
    if info.active && media_end - bar_x > 60.0 * s {
        let pos = info.position_ms();
        let lw = 36.0 * s;
        bar = (bar_x, cy - 2.0 * s, media_end - bar_x - lw - 6.0 * s, 4.0 * s);
        let (bx2, by, bw2, bh) = bar;
        x.round(bar, 2.0, 0x38FFFFFF);
        let frac = if info.duration_ms > 0 { (pos as f32 / info.duration_ms as f32).clamp(0.0, 1.0) } else { 0.0 };
        if frac > 0.0 {
            x.round((bx2, by, (bw2 * frac).max(bh), bh), 2.0, x.tint);
        }
        let right = if info.duration_ms > 0 { format!("-{}", fmt_time(info.duration_ms.saturating_sub(pos))) } else { "LIVE".into() };
        x.text(&right, (media_end - lw, cy - 8.0 * s, lw, 16.0 * s), 10.5, false, DIM, Align::Far);
        x.hit((bx2 - 4.0 * s, by - 8.0 * s, bw2 + 8.0 * s, bh + 16.0 * s), Hit::Seek);
    }

    // --- Pemisah ---
    let div = |x: &Ctx, at: f32| x.c.fill_rect(at, y0 + 6.0 * s, 1.0 * s, CONTENT_H * s - 12.0 * s, x.a(0x1FFFFFFF));
    let cal_x = ax + media_w;
    div(x, cal_x - 8.0 * s);

    // --- Kalender mini: bulan, 5 hari di sekitar hari ini, jam ---
    let n = names();
    let now = unsafe { GetLocalTime() };
    let today = days_from_civil(now.wYear as i64, now.wMonth as u32, now.wDay as u32);
    let cx0 = cal_x + 8.0 * s;
    let cw = cal_w - 24.0 * s;
    let month = &n.months[now.wMonth as usize - 1];
    x.text(&month.chars().take(3).collect::<String>(), (cx0, y0, cw, 20.0 * s), 15.0, true, 0xFFFFFFFF, Align::Near);
    x.text(n.days_long[weekday(today)], (cx0, y0, cw, 20.0 * s), 11.5, false, x.tint, Align::Far);
    let col = cw / 5.0;
    for i in 0..5 {
        let day = today - 2 + i as i64;
        let (_, _, d) = civil_from_days(day);
        let cxx = cx0 + col * i as f32;
        let is_today = i == 2;
        let wd = n.days_short[weekday(day)];
        x.text(&wd.chars().take(2).collect::<String>(), (cxx, y0 + 24.0 * s, col, 13.0 * s), 10.0, is_today, if is_today { 0xFFFFFFFF } else { 0xFF6E6E78 }, Align::Center);
        let r = (cxx + (col - 26.0 * s) / 2.0, y0 + 39.0 * s, 26.0 * s, 26.0 * s);
        if is_today {
            x.c.fill_ellipse(r.0, r.1, r.2, r.3, x.a(x.tint));
        }
        x.text(&d.to_string(), r, 13.0, is_today, if is_today { 0xFFFFFFFF } else { 0xFFD8D8DE }, Align::Center);
    }
    x.text(&format!("{}  ·  {} {}", clock_text(), now.wDay, month), (cx0, y0 + 68.0 * s, cw, 16.0 * s), 11.0, false, DIM, Align::Near);
    if has_cal {
        x.hit((cal_x, y0, cal_w - 16.0 * s, CONTENT_H * s), Hit::Tab(Tab::Calendar));
    }
    div(x, cal_x + cal_w - 8.0 * s);

    // --- Pintasan Timer (lingkaran seperti tombol "Mirror") ---
    let d = 72.0 * s;
    let tcx = bx + bw - pad - tile_w / 2.0 + 4.0 * s;
    let tcy = y0 + CONTENT_H * s / 2.0;
    let tr_ = (tcx - d / 2.0, tcy - d / 2.0, d, d);
    let hit = if has_timer { Hit::Tab(Tab::Timer) } else { Hit::ClockToggle };
    x.c.fill_ellipse(tr_.0, tr_.1, tr_.2, tr_.3, x.a(if x.hovered(hit) { 0xFF34343B } else { 0xFF26262B }));
    let (active, running, stopwatch, text) = clock(|c| (c.active(), c.running(), c.stopwatch, c.text(false)));
    let col = if active { if stopwatch { 0xFFFFFFFF } else { TIMER_COLOR } } else { 0xFFB4B4BE };
    x.icon(Icon::Stopwatch, (tcx - 12.0 * s, tcy - 22.0 * s, 24.0 * s, 24.0 * s), 22.0, col);
    let label = if active { text } else { "Timer".to_string() };
    x.text(&label, (tr_.0, tcy + 6.0 * s, d, 16.0 * s), if active { 12.5 } else { 11.5 }, active && running, col, Align::Center);
    x.hit(tr_, hit);
    bar
}

fn draw_calendar(x: &mut Ctx, bx: f32, bw: f32, y0: f32, week_off: i64) {
    let s = x.s;
    let pad = 20.0 * s;
    let n = names();
    let now = unsafe { GetLocalTime() };
    let today = days_from_civil(now.wYear as i64, now.wMonth as u32, now.wDay as u32);
    let wd = weekday(today);

    // Kiri: tanggal besar.
    let lx = bx + pad;
    x.text(n.days_long[wd], (lx, y0, 120.0 * s, 16.0 * s), 12.5, true, x.tint, Align::Near);
    x.text(&now.wDay.to_string(), (lx - 2.0 * s, y0 + 14.0 * s, 120.0 * s, 42.0 * s), 34.0, true, 0xFFFFFFFF, Align::Near);
    let my = format!("{} {}", n.months[now.wMonth as usize - 1], now.wYear);
    x.text(&my, (lx, y0 + 56.0 * s, 130.0 * s, 16.0 * s), 11.5, false, DIM, Align::Near);

    // Kanan: strip satu minggu (Senin–Minggu) dengan navigasi.
    let gx = lx + 150.0 * s;
    let gw = bx + bw - pad - gx;
    let monday = today - wd as i64 + week_off * 7;
    let col = gw / 7.0;
    for i in 0..7 {
        let day = monday + i as i64;
        let (_, m, d) = civil_from_days(day);
        let cx = gx + col * i as f32;
        let weekend = i >= 5;
        x.text(n.days_short[i], (cx, y0 + 2.0 * s, col, 14.0 * s), 10.5, false, if weekend { 0xFF6E6E78 } else { DIM }, Align::Center);
        let r = (cx + (col - 30.0 * s) / 2.0, y0 + 20.0 * s, 30.0 * s, 30.0 * s);
        let is_today = day == today;
        if is_today {
            x.c.fill_ellipse(r.0, r.1, r.2, r.3, x.a(x.tint));
        }
        let colr = if is_today { 0xFFFFFFFF } else if m != now.wMonth as u32 { 0xFF6E6E78 } else { 0xFFE6E6EA };
        x.text(&d.to_string(), r, 13.5, is_today, colr, Align::Center);
    }
    let (_, m0, d0) = civil_from_days(monday);
    let (_, m1, d1) = civil_from_days(monday + 6);
    let range = if m0 == m1 { format!("{d0}–{d1} {}", n.months[m1 as usize - 1]) } else { format!("{d0} {} – {d1} {}", &n.months[m0 as usize - 1][..3], &n.months[m1 as usize - 1][..3]) };
    let ny = y0 + 62.0 * s;
    x.button(gx + 10.0 * s, ny, 22.0 * s, Icon::ChevronLeft, 11.0, DIM, Hit::WeekPrev);
    let label_r = (gx + 24.0 * s, ny - 8.0 * s, gw - 48.0 * s, 16.0 * s);
    let label = if week_off == 0 { range } else { format!("{range}  ·  Hari ini") };
    x.text(&label, label_r, 11.0, false, if x.hovered(Hit::WeekToday) { 0xFFFFFFFF } else { DIM }, Align::Center);
    if week_off != 0 {
        x.hit(label_r, Hit::WeekToday);
    }
    x.button(gx + gw - 10.0 * s, ny, 22.0 * s, Icon::ChevronRight, 11.0, DIM, Hit::WeekNext);
}

fn draw_timer(x: &mut Ctx, bx: f32, bw: f32, y0: f32) {
    let s = x.s;
    let pad = 20.0 * s;
    let (stopwatch, running, alarm, text) = clock(|c| (c.stopwatch, c.running(), c.alarm(), c.text(true)));
    let color = if stopwatch { 0xFFFFFFFF } else { TIMER_COLOR };
    let lx = bx + pad;
    let big = if alarm { "Waktu habis".to_string() } else { text };
    x.text(&big, (lx, y0 - 2.0 * s, 220.0 * s, 44.0 * s), if alarm { 24.0 } else { 34.0 }, true, color, Align::Near);

    // Pilihan mode.
    let seg = (lx, y0 + 48.0 * s, 176.0 * s, 26.0 * s);
    x.round(seg, 8.0, 0xFF26262B);
    for (j, (label, sw)) in [("Timer", false), ("Stopwatch", true)].into_iter().enumerate() {
        let r = (seg.0 + 2.0 * s + j as f32 * (seg.2 - 4.0 * s) / 2.0, seg.1 + 2.0 * s, (seg.2 - 4.0 * s) / 2.0, seg.3 - 4.0 * s);
        if sw == stopwatch {
            x.round(r, 6.0, 0xFF48484F);
        } else if x.hovered(Hit::Mode(sw)) {
            x.round(r, 6.0, 0x1AFFFFFF);
        }
        x.text(label, r, 11.5, sw == stopwatch, if sw == stopwatch { 0xFFFFFFFF } else { DIM }, Align::Center);
        x.hit(r, Hit::Mode(sw));
    }

    // Kanan: tombol mulai/jeda + reset, lalu preset.
    let rx = bx + bw - pad;
    let cy = y0 + 18.0 * s;
    let d = 38.0 * s;
    let main = (rx - d, cy - d / 2.0, d, d);
    x.c.fill_ellipse(main.0, main.1, main.2, main.3, x.a(config::with_alpha(color, if x.hovered(Hit::ClockToggle) { 0.45 } else { 0.30 })));
    x.icon(if running { Icon::Pause } else { Icon::Play }, main, 17.0, color);
    x.hit(main, Hit::ClockToggle);
    let reset = (main.0 - d - 10.0 * s, main.1, d, d);
    x.c.fill_ellipse(reset.0, reset.1, reset.2, reset.3, x.a(if x.hovered(Hit::ClockReset) { 0xFF4A4A52 } else { 0xFF333339 }));
    x.icon(Icon::Reset, reset, 17.0, 0xFFFFFFFF);
    x.hit(reset, Hit::ClockReset);

    if !stopwatch {
        let presets: [(u32, &str); 4] = [(1, "+1m"), (5, "5m"), (15, "15m"), (25, "25m")];
        let cw = 44.0 * s;
        let mut cx = rx - presets.len() as f32 * (cw + 6.0 * s) + 6.0 * s;
        for (min, label) in presets {
            let r = (cx, y0 + 48.0 * s, cw, 26.0 * s);
            x.round(r, 13.0, if x.hovered(Hit::Preset(min)) { 0xFF46464E } else { 0xFF2C2C32 });
            x.text(label, r, 11.5, false, 0xFFE6E6EA, Align::Center);
            x.hit(r, Hit::Preset(min));
            cx += cw + 6.0 * s;
        }
    }
}

fn gauge(x: &Ctx, cx: f32, cy: f32, frac: f32, color: u32, value: &str, label: &str) {
    let s = x.s;
    let r = 22.0 * s;
    x.c.stroke_arc(cx, cy, r, 0.0, 360.0, 5.0 * s, x.a(0x2EFFFFFF));
    x.c.stroke_arc(cx, cy, r, -90.0, 360.0 * frac.clamp(0.0, 1.0), 5.0 * s, x.a(color));
    x.text(value, (cx - r, cy - r, 2.0 * r, 2.0 * r), 11.5, true, 0xFFFFFFFF, Align::Center);
    x.text(label, (cx - 60.0 * s, cy + r + 6.0 * s, 120.0 * s, 15.0 * s), 11.0, false, DIM, Align::Center);
}

fn draw_system(x: &mut Ctx, sys: &Sys, bx: f32, bw: f32, y0: f32) {
    let s = x.s;
    let cy = y0 + 26.0 * s;
    let mut items: Vec<(f32, u32, String, String)> = Vec::new();
    if let Some((pct, charging)) = sys.battery {
        let col = if charging { 0xFF30D158 } else if pct <= 20 { 0xFFFF453A } else { 0xFF30D158 };
        items.push((pct as f32 / 100.0, col, format!("{pct}%"), if charging { "Mengisi daya".into() } else { "Baterai".into() }));
    }
    items.push((sys.cpu, 0xFF0A84FF, format!("{:.0}%", sys.cpu * 100.0), "CPU".into()));
    items.push((sys.ram, 0xFFBF5AF2, format!("{:.0}%", sys.ram * 100.0), format!("RAM {:.1}/{:.0} GB", sys.ram_used_gb, sys.ram_total_gb)));
    let col_w = bw / items.len() as f32;
    for (i, (frac, color, value, label)) in items.iter().enumerate() {
        gauge(x, bx + col_w * (i as f32 + 0.5), cy, *frac, *color, value, label);
    }
}

struct Compact {
    left: Vec<(Slot, f32)>,
    right: Vec<(Slot, f32)>,
}

const SLOT_GAP: f32 = 8.0;
const SLOT_PAD: f32 = 10.0;

/// Item ringkas yang relevan saat ini beserta lebarnya (piksel).
fn compact_items(info: &MediaInfo, sys: &Sys, ch: f32, s: f32, font: &str) -> Compact {
    let Some(m) = Canvas::new(1, 1) else { return Compact { left: Vec::new(), right: Vec::new() } };
    let f = Font::new(font, 12.0 * s, false);
    let fb = Font::new(font, 12.5 * s, true);
    let (timer_on, timer_text) = clock(|k| (k.active(), k.text(false)));
    let sz = (ch - 10.0 * s).min(20.0 * s);
    let width = |sl: Slot| -> Option<f32> {
        match sl {
            Slot::Artwork => info.active.then_some(sz),
            Slot::Visualizer => info.active.then_some(16.0 * s),
            Slot::Title => info.active.then(|| m.measure(&info.title, &f).min(150.0 * s)).filter(|w| *w > 0.0),
            Slot::Timer => timer_on.then(|| 17.0 * s + m.measure(&timer_text, &fb)),
            Slot::Battery => sys.battery.map(|(p, _)| 25.0 * s + m.measure(&format!("{p}%"), &f)),
            Slot::Clock => Some(m.measure(&clock_text(), &fb)),
            Slot::Date => Some(m.measure(&date_text(), &f)),
            Slot::Cpu => Some(17.0 * s + m.measure(&format!("{:.0}%", sys.cpu * 100.0), &f)),
        }
    };
    let (l, r) = slots();
    let pick = |v: Vec<Slot>| v.into_iter().filter_map(|sl| width(sl).map(|w| (sl, w))).collect();
    let (left, right): (Vec<(Slot, f32)>, Vec<(Slot, f32)>) = (pick(l), pick(r));
    Compact { left, right }
}

fn side_width(v: &[(Slot, f32)], s: f32) -> f32 {
    if v.is_empty() {
        0.0
    } else {
        v.iter().map(|(_, w)| w).sum::<f32>() + SLOT_GAP * s * (v.len() - 1) as f32
    }
}

/// Lebar badan ringkas: minimal `compact_width`, melebar bila isinya butuh ruang lebih.
fn compact_body(cw: f32, items: &Compact, s: f32) -> f32 {
    let (lw, rw) = (side_width(&items.left, s), side_width(&items.right, s));
    let center = if lw > 0.0 && rw > 0.0 { 64.0 * s } else { 24.0 * s };
    cw.max(2.0 * SLOT_PAD * s + lw + rw + center)
}

fn clock_text() -> String {
    let t = unsafe { GetLocalTime() };
    format!("{:02}:{:02}", t.wHour, t.wMinute)
}

fn date_text() -> String {
    let t = unsafe { GetLocalTime() };
    let n = names();
    let wd = weekday(days_from_civil(t.wYear as i64, t.wMonth as u32, t.wDay as u32));
    format!("{} {}", n.days_short[wd], t.wDay)
}

/// Isi saat ringkas: item kiri dari tepi kiri ke dalam, item kanan dari tepi kanan ke dalam.
#[allow(clippy::too_many_arguments)]
fn draw_compact(c: &Canvas, items: &Compact, art: Option<&Image>, info: &MediaInfo, sys: &Sys, ear: f32, body_w: f32, ch: f32, s: f32, f: f32, tint: u32, bg: u32, font: &str, t_anim: f32) {
    let a = |col: u32| config::with_alpha(col, f * ((col >> 24) as f32 / 255.0));
    let sz = (ch - 10.0 * s).min(20.0 * s);
    let cy = ch / 2.0;
    let (stopwatch, alarm, timer_text) = clock(|k| (k.stopwatch, k.alarm(), k.text(false)));
    let small = Font::new(font, 12.0 * s, false);
    let bold = Font::new(font, 12.5 * s, true);
    let draw = |sl: Slot, x: f32, w: f32| match sl {
        Slot::Artwork => match art {
            Some(img) => c.draw_image(img, x, (ch - sz) / 2.0, sz, sz, 5.0 * s, f),
            None => icons::draw(c, Icon::Music, (x, (ch - sz) / 2.0, sz, sz), a(tint), bg),
        },
        Slot::Visualizer => {
            if info.playing {
                for i in 0..4 {
                    let speed = [7.1f32, 9.3, 6.2, 8.4][i];
                    let phase = [0.0f32, 1.3, 2.6, 0.7][i];
                    let lvl = 0.3 + 0.7 * ((t_anim * speed + phase).sin() * 0.5 + 0.5);
                    let bh = (sz * 0.85 * lvl).max(3.0 * s);
                    let k = 1.3 * s;
                    c.fill_round_rect(x + i as f32 * 4.4 * s, cy - bh / 2.0, 2.6 * s, bh, (k, k, k, k), a(tint));
                }
            } else {
                icons::draw(c, Icon::Pause, (x + 2.0 * s, cy - 6.0 * s, 12.0 * s, 12.0 * s), a(DIM), bg);
            }
        }
        Slot::Title => c.text(&info.title, &small, (x, 0.0, w, ch), a(0xE6FFFFFF), Align::Near),
        Slot::Timer => {
            let col = if stopwatch { 0xFFFFFFFF } else { TIMER_COLOR };
            let pulse = if alarm { 0.55 + 0.45 * (t_anim * 6.0).sin().abs() } else { 1.0 };
            let col = a(config::with_alpha(col, pulse));
            icons::draw(c, Icon::Stopwatch, (x, cy - 7.0 * s, 13.0 * s, 13.0 * s), col, bg);
            c.text(&timer_text, &bold, (x + 17.0 * s, 0.0, w - 17.0 * s + 2.0 * s, ch), col, Align::Near);
        }
        Slot::Battery => {
            if let Some((p, charging)) = sys.battery {
                let col = if charging { 0xFF30D158 } else if p <= 20 { 0xFFFF453A } else { 0xFFFFFFFF };
                icons::draw(c, Icon::Battery(p), (x, cy - 10.0 * s, 20.0 * s, 20.0 * s), a(col), bg);
                c.text(&format!("{p}%"), &small, (x + 25.0 * s, 0.0, w - 25.0 * s + 2.0 * s, ch), a(0xE6FFFFFF), Align::Near);
            }
        }
        Slot::Clock => c.text(&clock_text(), &bold, (x, 0.0, w + 2.0 * s, ch), a(0xFFFFFFFF), Align::Near),
        Slot::Date => c.text(&date_text(), &small, (x, 0.0, w + 2.0 * s, ch), a(0xE6FFFFFF), Align::Near),
        Slot::Cpu => {
            icons::draw(c, Icon::Cpu, (x, cy - 7.0 * s, 13.0 * s, 13.0 * s), a(0xFF0A84FF), bg);
            c.text(&format!("{:.0}%", sys.cpu * 100.0), &small, (x + 17.0 * s, 0.0, w - 17.0 * s + 2.0 * s, ch), a(0xE6FFFFFF), Align::Near);
        }
    };
    let mut x = ear + SLOT_PAD * s;
    for (sl, w) in &items.left {
        draw(*sl, x, *w);
        x += w + SLOT_GAP * s;
    }
    let mut x = ear + body_w - SLOT_PAD * s;
    for (sl, w) in &items.right {
        x -= w;
        draw(*sl, x, *w);
        x -= SLOT_GAP * s;
    }
}

/// Ukuran badan kartu (lebar, tinggi total) per jenis kartu; tinggi mengikuti isi, bukan tinggi bar.
fn notif_dims(s: f32, ch: f32, kind: Option<Kind>) -> (f32, f32) {
    match kind {
        Some(Kind::Call) => (450.0 * s, (80.0 * s).max(ch + 24.0 * s)),
        Some(Kind::Bluetooth { .. }) | Some(Kind::Charging { .. }) => (380.0 * s, (60.0 * s).max(ch + 24.0 * s)),
        _ => (410.0 * s, (64.0 * s).max(ch + 24.0 * s)),
    }
}

/// Monogram berwarna (tanpa gambar) di kotak `r`.
#[allow(clippy::too_many_arguments)]
fn monogram(c: &Canvas, toast: &Toast, r: Rect, radius: f32, accent: u32, font: &str, s: f32, a: &dyn Fn(u32) -> u32) {
    c.fill_round_rect(r.0, r.1, r.2, r.3, (radius, radius, radius, radius), a(config::with_alpha(accent, 1.0)));
    let mono = toast.title.chars().chain(toast.app.chars()).find(|ch| ch.is_alphanumeric()).map(|ch| ch.to_uppercase().to_string()).unwrap_or_else(|| "!".into());
    c.text(&mono, &Font::new(&face(font, r.3 / s * 0.45), r.3 * 0.45, true), r, a(0xFFFFFFFF), Align::Center);
}

/// Kartu notifikasi di notch; mengembalikan kotak tombolnya (hit-test).
#[allow(clippy::too_many_arguments)]
fn draw_notif(hwnd: HWND, c: &Canvas, body_w: f32, ear: f32, ch: f32, s: f32, fade: f32, font: &str, accent: u32, bg: u32, t_anim: f32) -> Vec<(Rect, Hit)> {
    let cfg = config::current();
    let Some((toast, hover, hit)) = with(hwnd, |st| st.notif.clone().map(|t| (t, st.hover, st.hover_hit))).flatten() else { return Vec::new() };
    let mut x = Ctx { c, s, font: font.to_string(), fade, tint: accent, bg, hover: hit, rects: Vec::new() };
    let a = |col: u32| config::with_alpha(col, fade * ((col >> 24) as f32 / 255.0));
    let (_, nh) = notif_dims(s, ch, Some(toast.kind));
    let pad = 18.0 * s;
    // Isi dipusatkan pada seluruh tinggi kartu (tanpa strip kosong setinggi bar di atasnya).
    let cy = nh / 2.0;
    let left = ear + pad;
    let right = ear + body_w - pad;
    // Gambar banner (avatar/logo) bila ada.
    let draw_img = |r: Rect, radius: f32| with(hwnd, |st| st.notif_img.as_ref().map(|i| c.draw_image(i, r.0, r.1, r.2, r.3, radius, fade)).is_some()).unwrap_or(false);

    match toast.kind {
        Kind::Message => {
            let isz = 42.0 * s;
            let ir = (left, cy - isz / 2.0, isz, isz);
            if !draw_img(ir, 11.0 * s) {
                monogram(c, &toast, ir, 11.0 * s, accent, font, s, &a);
            }
            let tx = left + isz + 13.0 * s;
            let app = if toast.app.is_empty() { tr("Notifikasi", "Notification").to_string() } else { toast.app.clone() };
            let meta = format!("{app}  ·  {}", tr("sekarang", "now"));
            let meta_w = crate::canvas::measure(&meta, &Font::new(font, 11.0 * s, false)) + 2.0 * s;
            let tw = (right - tx).max(10.0);
            let title_w = (tw - meta_w - 10.0 * s).max(40.0 * s);
            x.text(&toast.title, (tx, cy - 20.0 * s, title_w, 20.0 * s), 14.5, true, 0xFFFFFFFF, Align::Near);
            if hover {
                let d = 20.0 * s;
                let r = (right - d, cy - 20.0 * s, d, d);
                c.fill_ellipse(r.0, r.1, r.2, r.3, a(if hit == Some(Hit::NotifClose) { 0x55FFFFFF } else { 0x33FFFFFF }));
                x.icon(Icon::Close, r, 9.0, 0xFFFFFFFF);
                x.hit(r, Hit::NotifClose);
            } else {
                x.text(&meta, (right - meta_w, cy - 20.0 * s, meta_w, 20.0 * s), 11.0, false, 0xFF8E8E98, Align::Far);
            }
            let body = if cfg.notifications.show_body { toast.body.as_str() } else { "" };
            if !body.is_empty() {
                x.text(body, (tx, cy + 1.0 * s, tw, 19.0 * s), 13.0, false, 0xFFB8B8C2, Align::Near);
            }
        }
        Kind::Call => {
            let isz = 54.0 * s;
            let ir = (left, cy - isz / 2.0, isz, isz);
            if !draw_img(ir, isz / 2.0) {
                monogram(c, &toast, ir, isz / 2.0, accent, font, s, &a);
            }
            let tx = left + isz + 14.0 * s;
            let bd = 46.0 * s;
            let tw = (right - 2.0 * bd - 14.0 * s - tx).max(20.0);
            let sub = if toast.app.is_empty() { tr("Panggilan masuk", "Incoming call").to_string() } else { format!("{}  ·  {}", toast.app, tr("Panggilan masuk", "Incoming call")) };
            x.text(&sub, (tx, cy - 22.0 * s, tw, 16.0 * s), 11.5, false, 0xFF9E9EA8, Align::Near);
            x.text(&toast.title, (tx, cy - 4.0 * s, tw, 28.0 * s), 21.0, true, 0xFFFFFFFF, Align::Near);
            // Tombol: tolak (merah) dan terima (hijau, berdenyut).
            let acc = (right - bd, cy - bd / 2.0, bd, bd);
            let dec = (acc.0 - bd - 14.0 * s, acc.1, bd, bd);
            let pulse = (t_anim * 0.9).fract();
            let pr = bd / 2.0 + pulse * 9.0 * s;
            c.fill_ellipse(acc.0 + bd / 2.0 - pr, acc.1 + bd / 2.0 - pr, 2.0 * pr, 2.0 * pr, a(config::with_alpha(GREEN, 0.35 * (1.0 - pulse))));
            let hv = |h: Hit, col: u32| if hit == Some(h) { config::with_alpha(col, 1.0) } else { config::with_alpha(col, 0.9) };
            c.fill_ellipse(dec.0, dec.1, dec.2, dec.3, a(hv(Hit::CallDecline, RED)));
            c.fill_ellipse(acc.0, acc.1, acc.2, acc.3, a(hv(Hit::CallAccept, GREEN)));
            x.glyph(G_HANGUP, dec, 19.0, 0xFFFFFFFF);
            x.glyph(G_PHONE, acc, 18.0, 0xFFFFFFFF);
            x.hit(dec, Hit::CallDecline);
            x.hit(acc, Hit::CallAccept);
        }
        Kind::Bluetooth { battery, audio } => {
            let rr = 17.0 * s;
            let (rcx, rcy) = (left + rr + 2.0 * s, cy);
            c.stroke_arc(rcx, rcy, rr, 0.0, 360.0, 3.5 * s, a(0x33FFFFFF));
            let frac = battery.map(|b| b as f32 / 100.0).unwrap_or(1.0);
            let col = match battery {
                Some(b) if b <= 20 => RED,
                _ => CYAN,
            };
            c.stroke_arc(rcx, rcy, rr, -90.0, 360.0 * frac, 3.5 * s, a(col));
            x.icon(Icon::Bluetooth, (rcx - rr, rcy - rr, 2.0 * rr, 2.0 * rr), 15.0, 0xFFFFFFFF);
            let tx = rcx + rr + 16.0 * s;
            let tw = (right - 52.0 * s - tx).max(20.0);
            x.text(&toast.title, (tx, cy - 20.0 * s, tw, 21.0 * s), 15.5, true, 0xFFFFFFFF, Align::Near);
            let sub = match battery {
                Some(b) => format!("{}  ·  {b}%", tr("Tersambung", "Connected")),
                None => tr("Tersambung", "Connected").to_string(),
            };
            x.text(&sub, (tx, cy + 1.0 * s, tw, 18.0 * s), 12.5, false, 0xFF9E9EA8, Align::Near);
            // Lingkaran ikon perangkat dengan cincin bercahaya.
            let d = 42.0 * s;
            let (ocx, ocy) = (right - d / 2.0, cy);
            c.fill_ellipse(ocx - d / 2.0 - 4.0 * s, ocy - d / 2.0 - 4.0 * s, d + 8.0 * s, d + 8.0 * s, a(0x225AC8FA));
            c.fill_ellipse(ocx - d / 2.0, ocy - d / 2.0, d, d, a(0xFF12222C));
            c.stroke_arc(ocx, ocy, d / 2.0 - 1.0 * s, 0.0, 360.0, 2.0 * s, a(0xCC5AC8FA));
            let ir = (ocx - d / 2.0, ocy - d / 2.0, d, d);
            if audio {
                x.glyph(G_HEADPHONE, ir, 18.0, 0xFFE6F6FF);
            } else {
                x.icon(Icon::Bluetooth, ir, 18.0, 0xFFE6F6FF);
            }
        }
        Kind::Charging { pct } => {
            let rr = 17.0 * s;
            let (rcx, rcy) = (left + rr + 2.0 * s, cy);
            c.stroke_arc(rcx, rcy, rr, 0.0, 360.0, 3.5 * s, a(0x33FFFFFF));
            c.stroke_arc(rcx, rcy, rr, -90.0, 360.0 * (pct as f32 / 100.0), 3.5 * s, a(GREEN));
            // Petir.
            let u = rr / 12.0;
            let bolt = [(1.5, -8.0), (-5.0, 1.0), (-0.5, 1.0), (-1.5, 8.0), (5.0, -1.0), (0.5, -1.0)];
            let pts: Vec<(f32, f32)> = bolt.iter().map(|&(px, py)| (rcx + px * u, rcy + py * u)).collect();
            c.fill_polygon(&pts, a(0xFFFFFFFF));
            let tx = rcx + rr + 16.0 * s;
            let pw = 80.0 * s;
            let tw = (right - pw - tx).max(20.0);
            x.text(&toast.title, (tx, cy - 20.0 * s, tw, 21.0 * s), 15.5, true, 0xFFFFFFFF, Align::Near);
            x.text(&toast.body, (tx, cy + 1.0 * s, tw, 18.0 * s), 12.5, false, 0xFF9E9EA8, Align::Near);
            x.text(&format!("{pct}%"), (right - pw, cy - 18.0 * s, pw, 36.0 * s), 27.0, false, 0xFFFFFFFF, Align::Far);
        }
    }
    x.rects
}

/// Tampilkan banner notifikasi di notch ini (antre bila sedang menampilkan yang lain; maks 8).
pub fn notify(hwnd: HWND, t: Toast) {
    let cfg = config::current();
    if !cfg.notifications.enabled {
        return;
    }
    // Privasi: bawaan hanya monitor utama yang menampilkan kartu.
    if cfg.notifications.monitors != "all" && !with(hwnd, |st| st.mon.primary).unwrap_or(false) {
        return;
    }
    let _ = with(hwnd, |st| {
        if st.queue.len() >= 8 {
            st.queue.pop_front();
        }
        st.queue.push_back(t);
    });
    advance_notif(hwnd);
}

/// Mulai menampilkan notifikasi berikutnya bila notch sedang tidak menampilkan satu pun.
fn advance_notif(hwnd: HWND) -> bool {
    let started = with(hwnd, |st| {
        if st.notif.is_some() {
            return false;
        }
        let Some(t) = st.queue.pop_front() else { return false };
        // Ikon asli: gambar banner bila tertangkap; kalau tidak, ikon aplikasi yang cocok dengan nama pengirim.
        st.notif_img = match t.kind {
            Kind::Message | Kind::Call => t.icon.as_ref().and_then(|b| Image::from_bytes(b)).or_else(|| crate::popup::app_icon(&t.app)),
            _ => None,
        };
        st.notif = Some(t);
        st.n_target = true;
        st.hover = false;
        st.hover_hit = None;
        st.last = Instant::now();
        true
    })
    .unwrap_or(false);
    if started {
        start_anim(hwnd, false); // panel menutup dulu
        let dur = notif_duration(hwnd);
        unsafe {
            SetTimer(Some(hwnd), TIMER_ANIM, FRAME_MS, None);
            SetTimer(Some(hwnd), TIMER_NOTIF, dur, None);
        }
    }
    started
}

fn dismiss_notif(hwnd: HWND) {
    with(hwnd, |st| {
        st.n_target = false;
        st.last = Instant::now();
    });
    unsafe {
        let _ = KillTimer(Some(hwnd), TIMER_NOTIF);
        SetTimer(Some(hwnd), TIMER_ANIM, FRAME_MS, None);
    }
}

fn notif_active(hwnd: HWND) -> bool {
    with(hwnd, |st| st.notif.is_some() && st.n_target).unwrap_or(false)
}

/// Lama kartu yang sedang tampil: panggilan bertahan hingga 30 dtk, aktivitas sistem sebentar.
fn notif_duration(hwnd: HWND) -> u32 {
    let base = config::current().notifications.duration_ms.max(1000);
    match with(hwnd, |st| st.notif.as_ref().map(|t| t.kind)).flatten() {
        Some(Kind::Call) => 30_000,
        Some(Kind::Bluetooth { .. }) | Some(Kind::Charging { .. }) => base.min(3500),
        _ => base,
    }
}

thread_local! {
    static CHARGING: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Status daya berubah: bila pengisi daya baru saja dicolokkan, tampilkan kartu di semua notch.
/// (Setiap notch menerima siaran yang sama; status bersama mencegah kartu ganda.)
fn on_power() {
    let Some((pct, charging)) = crate::bar::battery() else { return };
    let was = CHARGING.with(|c| c.replace(Some(charging)));
    if was != Some(false) || !charging {
        return;
    }
    let mut t = Toast::message("", tr("Mengisi daya", "Charging"), tr("Pengisi daya tersambung", "Charger connected"));
    t.kind = Kind::Charging { pct };
    let all: Vec<isize> = WINS.with(|w| w.borrow().keys().copied().collect());
    for h in all {
        notify(HWND(h as *mut _), t.clone());
    }
}

pub fn redraw(hwnd: HWND) {
    let cfg = config::current();
    let info = media::info();

    // Sampul: decode ulang (dan hitung warnanya) hanya saat gambar berganti.
    let want = info.thumb.as_ref().map(|t| Arc::as_ptr(t) as usize);
    let Some((mut art, mut tint)) = with(hwnd, |st| (st.art.take(), st.tint)) else { return };
    match (want, &art) {
        (Some(p), Some((q, _))) if p == *q => {}
        (Some(p), _) => {
            art = info.thumb.as_ref().and_then(|t| Image::from_bytes(t)).map(|i| (p, i));
            tint = art.as_ref().and_then(|(_, i)| i.tint());
        }
        (None, _) => {
            art = None;
            tint = None;
        }
    }

    // Statistik sistem: sampel paling sering sekali per detik, hanya saat tab Sistem terlihat.
    // Notch ringkas selalu butuh baterai (isi bawaan saat idle = baterai + jam).
    let compact_sys = true;
    let sys_due = with(hwnd, |st| {
        let visible = (st.tab == Tab::System && st.x > 0.5) || (compact_sys && st.x < 0.5);
        visible && st.sys_at.map(|t| t.elapsed() >= Duration::from_millis(900)).unwrap_or(true)
    })
    .unwrap_or(false);
    if sys_due {
        with(hwnd, |st| {
            st.sys = sample_sys(st.sys);
            st.sys_at = Some(Instant::now());
        });
    }

    let Some((x, nt, mon, tab, week_off, hover, sys, born, kind)) = with(hwnd, |st| (st.x, st.nt, st.mon, st.tab, st.week_off, st.hover_hit, st.sys, st.born, st.notif.as_ref().map(|t| t.kind))) else { return };
    let s = mon.scale;
    let pill = pill();
    let (cw, ew, ch, eh) = dims(s);
    let items = compact_items(&info, &sys, ch, s, &cfg.bar.font);
    let idle = items.left.is_empty() && items.right.is_empty();
    let cw = compact_body(cw, &items, s);
    let ear = if pill { 0.0 } else { 8.0 * s };
    let xc = x.clamp(0.0, 1.15);
    let (nw, nh) = notif_dims(s, ch, kind);
    let nc = nt.clamp(0.0, 1.1);
    let body_w = lerp(lerp(cw, ew, xc), nw, nc).round();
    let h = lerp(lerp(ch, eh, xc), nh, nc).round();
    let w = body_w + 2.0 * ear;
    let t = x.clamp(0.0, 1.0);

    let bg = config::color(&cfg.notch.background);
    let accent = config::color(&cfg.notch.accent);
    let tint_col = if cfg.notch.tint_from_artwork { tint.unwrap_or(accent) } else { accent };
    let mut rects = Vec::new();
    let mut notif_rects = Vec::new();
    let mut seek_bar = (0.0, 0.0, 0.0, 0.0);
    let t_anim = born.elapsed().as_secs_f32();

    // Ikon aplikasi sumber media: dicari sekali per AppUserModelID.
    let source = with(hwnd, |st| {
        if st.source.as_ref().map(|(k, _)| k != &info.source).unwrap_or(true) {
            st.source = Some((info.source.clone(), crate::dock::source_app(&info.source)));
        }
        st.source.take()
    })
    .flatten();

    // Kanvas dipakai ulang; diperbesar (dengan sisa ruang untuk pantulan spring) hanya bila perlu.
    let mut canvas = with(hwnd, |st| st.canvas.take()).flatten();
    if canvas.as_ref().map(|c| c.w < w as i32 || c.h < h as i32).unwrap_or(true) {
        let need_w = (w.max(ew + 2.0 * ear) * 1.12).ceil() as i32;
        let need_h = (h.max(eh).max(ch + 90.0 * s) * 1.12).ceil() as i32;
        canvas = Canvas::new(need_w, need_h);
    }
    if let Some(c) = canvas.as_ref() {
        c.clear();
        let br = lerp(lerp(10.0 * s, 26.0 * s, t), 26.0 * s, nt.clamp(0.0, 1.0));
        if pill {
            let r = lerp(lerp(ch / 2.0, 24.0 * s, t), 26.0 * s, nt.clamp(0.0, 1.0)).min(h / 2.0);
            c.fill_round_rect(0.0, 0.0, w, h, (r, r, r, r), bg);
        } else {
            c.fill_notch(w, h, ear, br, bg);
        }

        if nt > 0.5 {
            notif_rects = draw_notif(hwnd, c, body_w, ear, ch, s, ((nt - 0.5) / 0.4).clamp(0.0, 1.0), &cfg.bar.font, accent, bg, t_anim);
        }

        if t < 0.45 && nt < 0.25 {
            let f = (1.0 - t / 0.45).clamp(0.0, 1.0) * (1.0 - nt / 0.25).clamp(0.0, 1.0);
            if idle {
                if crate::idle::enabled() {
                    let (growth, bloom) = clock(|k| {
                        let used = k.active() && (k.running() || k.alarm());
                        let g = if !used { None } else if k.stopwatch { Some((k.elapsed().as_secs_f32() % 1500.0) / 1500.0) } else { Some(k.elapsed().as_secs_f32() / k.target.as_secs_f32().max(1.0)) };
                        (g, k.alarm())
                    });
                    let inp = crate::idle::Input { ear, body_w, h: ch, s, f, bg, tint: tint_col, t: t_anim, cpu: sys.cpu, battery: sys.battery, growth, bloom, br };
                    crate::idle::draw(c, &inp);
                }
            } else {
                draw_compact(c, &items, art.as_ref().map(|(_, i)| i), &info, &sys, ear, body_w, ch, s, f, tint_col, bg, &cfg.bar.font, t_anim);
            }
        }

        if t > 0.55 && nt < 0.25 {
            let fade = ((t - 0.55) / 0.35).clamp(0.0, 1.0);
            let mut cx = Ctx { c, s, font: cfg.bar.font.clone(), fade, tint: tint_col, bg, hover, rects: Vec::new() };

            // Strip atas: tab berlabel (pil) di kiri, Pengaturan di kanan.
            let tabs = tabs();
            let strip_c = ch / 2.0 + if pill { 3.0 * s } else { 1.0 * s };
            if tabs.len() > 1 {
                let f = Font::new(&cfg.bar.font, 12.0 * s, true);
                let mut tx = ear + 14.0 * s;
                let ph = (ch - 6.0 * s).min(24.0 * s);
                for tb in tabs.iter() {
                    let lw = crate::canvas::measure(tb.label(), &f);
                    let pw = 10.0 * s + 14.0 * s + 5.0 * s + lw + 11.0 * s;
                    let r = (tx, strip_c - ph / 2.0, pw, ph);
                    let on = *tb == tab;
                    if on {
                        cx.round(r, ph / 2.0 / s, 0x33FFFFFF);
                    } else if cx.hovered(Hit::Tab(*tb)) {
                        cx.round(r, ph / 2.0 / s, 0x1AFFFFFF);
                    }
                    let col = if on { 0xFFFFFFFF } else { DIM };
                    cx.icon(tb.icon(), (tx + 10.0 * s, r.1, 14.0 * s, ph), 13.0, col);
                    cx.text(tb.label(), (tx + 29.0 * s, r.1, lw + 4.0 * s, ph), 12.0, true, col, Align::Near);
                    cx.hit(r, Hit::Tab(*tb));
                    tx += pw + 4.0 * s;
                }
            }
            let d = (ch - 6.0 * s).min(26.0 * s);
            cx.button(ear + body_w - 14.0 * s - d / 2.0, strip_c, d, Icon::Gear, 14.0, DIM, Hit::Settings);

            let y0 = ch + ((h - ch) - CONTENT_H * s) / 2.0 - 2.0 * s;
            let y0 = y0.max(ch + 4.0 * s);
            match tab {
                Tab::Media => {
                    let src = source.as_ref().and_then(|(_, v)| v.as_ref());
                    seek_bar = draw_media(&mut cx, art.as_ref().map(|(_, i)| i), src, &info, ear, body_w, y0, tabs.contains(&Tab::Calendar), tabs.contains(&Tab::Timer))
                }
                Tab::Calendar => draw_calendar(&mut cx, ear, body_w, y0, week_off),
                Tab::Timer => draw_timer(&mut cx, ear, body_w, y0),
                Tab::System => draw_system(&mut cx, &sys, ear, body_w, y0),
            }
            rects = cx.rects;
        }

        let y = mon.y + if pill { (4.0 * s).round() as i32 } else { 0 };
        c.present_size(hwnd, Some((mon.x + ((mon.w as f32 - w) / 2.0).round() as i32, y)), w as i32, h as i32);
    }
    with(hwnd, |st| {
        st.art = art;
        st.tint = tint;
        st.rects = rects;
        st.notif_rects = notif_rects;
        st.seek_bar = seek_bar;
        st.canvas = canvas;
        st.idle = idle;
        st.source = source;
    });
    sync_tick(hwnd);
}

fn start_anim(hwnd: HWND, open: bool) {
    let was = with(hwnd, |st| {
        let w = std::mem::replace(&mut st.target_open, open);
        st.last = Instant::now();
        w
    })
    .unwrap_or(open);
    if was == open {
        return;
    }
    if !open {
        with(hwnd, |st| {
            st.hover_hit = None;
            st.week_off = 0;
        });
    }
    unsafe {
        SetTimer(Some(hwnd), TIMER_ANIM, FRAME_MS, None);
    }
}

/// Satu langkah fisika spring (semi-implicit Euler).
fn step(hwnd: HWND) {
    let speed = 180.0 / config::current().notch.animation_ms.max(40) as f32;
    let (k, c) = (380.0 * speed * speed, 24.0 * speed);
    let done = with(hwnd, |st| {
        let now = Instant::now();
        let dt = now.duration_since(st.last).as_secs_f32().clamp(0.001, 0.05);
        st.last = now;
        let goal = if st.target_open { 1.0 } else { 0.0 };
        let a = k * (goal - st.x) - c * st.v;
        st.v += a * dt;
        st.x += st.v * dt;
        let open_done = (goal - st.x).abs() < 0.002 && st.v.abs() < 0.02;
        if open_done {
            st.x = goal;
            st.v = 0.0;
        }
        let ngoal = if st.n_target { 1.0 } else { 0.0 };
        let na = k * (ngoal - st.nt) - c * st.nv;
        st.nv += na * dt;
        st.nt += st.nv * dt;
        let notif_done = (ngoal - st.nt).abs() < 0.002 && st.nv.abs() < 0.02;
        if notif_done {
            st.nt = ngoal;
            st.nv = 0.0;
        }
        open_done && notif_done
    })
    .unwrap_or(true);
    redraw(hwnd);
    if done {
        // Kartu sudah menyusut sepenuhnya: lepas dan tampilkan antrean berikutnya.
        let gone = with(hwnd, |st| st.notif.is_some() && !st.n_target && st.nt == 0.0).unwrap_or(false);
        let mut next = false;
        if gone {
            with(hwnd, |st| {
                st.notif = None;
                st.notif_img = None;
            });
            next = advance_notif(hwnd);
        }
        if !next {
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_ANIM);
            }
            sync_tick(hwnd);
        }
    }
}

fn track_leave(hwnd: HWND) {
    let need = with(hwnd, |st| !std::mem::replace(&mut st.tracking, true)).unwrap_or(false);
    if need {
        let mut tme = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: hwnd,
            dwHoverTime: 0,
        };
        unsafe {
            let _ = TrackMouseEvent(&mut tme);
        }
    }
}

fn hit_at(hwnd: HWND, x: f32, y: f32) -> Option<Hit> {
    with(hwnd, |st| st.rects.iter().rev().find(|(r, _)| x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3).map(|(_, h)| *h)).flatten()
}

/// Semua notch ikut digambar ulang (timer dibagi bersama).
fn redraw_all() {
    let all: Vec<isize> = WINS.with(|w| w.borrow().keys().copied().collect());
    for h in all {
        redraw(HWND(h as *mut _));
    }
}

fn switch_tab(hwnd: HWND, dir: i32) {
    let list = tabs();
    with(hwnd, |st| {
        let i = list.iter().position(|t| *t == st.tab).unwrap_or(0) as i32;
        st.tab = list[(i + dir).rem_euclid(list.len() as i32) as usize];
        st.hover_hit = None;
    });
    redraw(hwnd);
}

fn click(hwnd: HWND, mx: f32, my: f32) {
    let Some((open, s)) = with(hwnd, |st| (st.target_open, st.mon.scale)) else { return };
    if notif_active(hwnd) {
        let toast = with(hwnd, |st| st.notif.clone()).flatten();
        let hit = with(hwnd, |st| st.notif_rects.iter().rev().find(|(r, _)| mx >= r.0 && mx < r.0 + r.2 && my >= r.1 && my < r.1 + r.3).map(|(_, h)| *h)).flatten();
        use crate::notifications::{activate, invoke_action, ACCEPT_WORDS, DECLINE_WORDS};
        match (hit, toast) {
            (Some(Hit::NotifClose), _) => {}
            (Some(Hit::CallAccept), Some(t)) => match t.action_like(ACCEPT_WORDS) {
                Some(a) => invoke_action(a),
                None => activate(),
            },
            (Some(Hit::CallDecline), Some(t)) => {
                if let Some(a) = t.action_like(DECLINE_WORDS) {
                    invoke_action(a);
                }
            }
            // Aktivitas sistem: klik hanya menutup kartu.
            (_, Some(t)) if matches!(t.kind, Kind::Bluetooth { .. } | Kind::Charging { .. }) => {}
            // Klik di bagian lain kartu panggilan tidak menutupnya (agar tidak salah tekan).
            (_, Some(t)) if t.kind == Kind::Call => return,
            _ => activate(), // teruskan klik ke banner aslinya
        }
        dismiss_notif(hwnd);
        return;
    }
    if open && is_settled_open(hwnd) {
        let Some(hit) = hit_at(hwnd, mx, my) else {
            // Klik di strip atas yang kosong menutup; area isi lainnya tidak berbuat apa-apa.
            if my < dims(s).2 {
                start_anim(hwnd, false);
            }
            return;
        };
        match hit {
            Hit::Tab(t) => {
                with(hwnd, |st| st.tab = t);
            }
            Hit::Settings => {
                start_anim(hwnd, false);
                crate::handle_command(crate::IDM_SETTINGS);
            }
            Hit::Prev => drop(media::send(Command::Previous)),
            Hit::Play => drop(media::send(Command::TogglePlayPause)),
            Hit::Next => drop(media::send(Command::Next)),
            Hit::Seek => {
                let info = media::info();
                let bar = with(hwnd, |st| st.seek_bar).unwrap_or_default();
                if info.active && info.duration_ms > 0 && bar.2 > 0.0 {
                    let frac = ((mx - bar.0) / bar.2).clamp(0.0, 1.0);
                    media::send(Command::Seek((frac * info.duration_ms as f32) as u64));
                }
            }
            Hit::WeekPrev => {
                with(hwnd, |st| st.week_off -= 1);
            }
            Hit::WeekNext => {
                with(hwnd, |st| st.week_off += 1);
            }
            Hit::WeekToday => {
                with(hwnd, |st| st.week_off = 0);
            }
            Hit::ClockToggle => clock(Clock::toggle),
            Hit::ClockReset => clock(Clock::reset),
            Hit::Mode(sw) => clock(|c| {
                if c.stopwatch != sw {
                    c.reset();
                    c.stopwatch = sw;
                }
            }),
            Hit::Preset(min) => clock(|c| {
                c.stopwatch = false;
                if min == 1 {
                    c.target += Duration::from_secs(60);
                    if !c.running() && c.elapsed() >= c.target {
                        c.acc = Duration::ZERO;
                    }
                } else {
                    c.reset();
                    c.target = Duration::from_secs(min as u64 * 60);
                    c.since = Some(Instant::now());
                }
            }),
            Hit::CallAccept | Hit::CallDecline | Hit::NotifClose => {}
        }
        redraw_all();
        return;
    }
    start_anim(hwnd, !open);
}

pub unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEMOVE => {
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_CLOSE);
            }
            track_leave(hwnd);
            if notif_active(hwnd) {
                // Kursor di atas kartu: tahan kedaluwarsa dan tampilkan tombol X.
                let first = with(hwnd, |st| !std::mem::replace(&mut st.hover, true)).unwrap_or(false);
                let (mx, my) = ((lparam.0 & 0xFFFF) as i16 as f32, ((lparam.0 >> 16) & 0xFFFF) as i16 as f32);
                let changed = with(hwnd, |st| {
                    let h = st.notif_rects.iter().rev().find(|(r, _)| mx >= r.0 && mx < r.0 + r.2 && my >= r.1 && my < r.1 + r.3).map(|(_, h)| *h);
                    std::mem::replace(&mut st.hover_hit, h) != h
                })
                .unwrap_or(false);
                if first {
                    unsafe {
                        let _ = KillTimer(Some(hwnd), TIMER_NOTIF);
                    }
                }
                if first || changed {
                    redraw(hwnd);
                }
                return LRESULT(0);
            }
            if config::current().notch.expand_on_hover {
                start_anim(hwnd, true);
            }
            if is_settled_open(hwnd) {
                let x = (lparam.0 & 0xFFFF) as i16 as f32;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32;
                let h = hit_at(hwnd, x, y);
                if with(hwnd, |st| std::mem::replace(&mut st.hover_hit, h) != h).unwrap_or(false) {
                    redraw(hwnd);
                }
            }
            LRESULT(0)
        }
        0x02A3 => {
            // WM_MOUSELEAVE
            with(hwnd, |st| {
                st.tracking = false;
                st.hover_hit = None;
            });
            if notif_active(hwnd) {
                with(hwnd, |st| st.hover = false);
                let dur = notif_duration(hwnd);
                unsafe {
                    SetTimer(Some(hwnd), TIMER_NOTIF, dur, None);
                }
                redraw(hwnd);
                return LRESULT(0);
            }
            if with(hwnd, |st| st.target_open).unwrap_or(false) {
                let d = config::current().notch.close_delay_ms;
                unsafe {
                    SetTimer(Some(hwnd), TIMER_CLOSE, d.max(1), None);
                }
            }
            redraw(hwnd);
            LRESULT(0)
        }
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            // Gulir di atas notch terbuka = pindah tab (seperti swipe).
            if is_settled_open(hwnd) {
                let delta = ((wparam.0 >> 16) & 0xFFFF) as i16;
                let dir = if (msg == WM_MOUSEWHEEL) == (delta < 0) { 1 } else { -1 };
                switch_tab(hwnd, dir);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let x = (lparam.0 & 0xFFFF) as i16 as f32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32;
            click(hwnd, x, y);
            LRESULT(0)
        }
        0x0218 => {
            // WM_POWERBROADCAST / PBT_APMPOWERSTATUSCHANGE: kartu "Mengisi daya" saat pengisi daya dicolokkan.
            if wparam.0 == 0x000A {
                on_power();
            }
            LRESULT(1)
        }
        WM_RBUTTONUP => {
            crate::show_menu(hwnd);
            LRESULT(0)
        }
        WM_TIMER => {
            match wparam.0 {
                TIMER_ANIM => step(hwnd),
                TIMER_CLOSE => {
                    unsafe {
                        let _ = KillTimer(Some(hwnd), TIMER_CLOSE);
                    }
                    start_anim(hwnd, false);
                }
                TIMER_NOTIF => dismiss_notif(hwnd),
                TIMER_Z => {
                    // Jangan merebut posisi atas dari menu/popup yang sedang terbuka (mereka sengaja di atas notch).
                    if !crate::FULLSCREEN.load(std::sync::atomic::Ordering::Relaxed) {
                        unsafe {
                            if crate::menu::is_open() || crate::popup::any_open() {
                                // Menu/popup sengaja di atas notch: jangan rebut posisinya, cukup pastikan menu bar
                                // berada DI BAWAH notch (bar sering naik saat diklik dan menutupi notch).
                                for b in crate::bars() {
                                    let _ = SetWindowPos(b, Some(hwnd), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                                }
                            } else {
                                let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                            }
                        }
                    }
                }
                TIMER_TICK => {
                    if check_alarm() {
                        unsafe {
                            let _ = windows::Win32::System::Diagnostics::Debug::MessageBeep(MB_ICONASTERISK);
                        }
                        redraw_all();
                    } else {
                        redraw(hwnd);
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_roundtrip_and_weekday() {
        let d = days_from_civil(2026, 11, 4);
        assert_eq!(civil_from_days(d), (2026, 11, 4));
        assert_eq!(weekday(d), 2, "4 Nov 2026 hari Rabu");
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29) + 1), (2024, 3, 1));
    }

    #[test]
    fn countdown_text_rounds_up() {
        let c = Clock { target: Duration::from_secs(300), acc: Duration::from_millis(1500), ..Default::default() };
        assert_eq!(c.text(false), "4:59");
        let sw = Clock { stopwatch: true, acc: Duration::from_millis(61_250), ..Default::default() };
        assert_eq!(sw.text(true), "1:01.2");
    }

    #[test]
    fn tab_names() {
        assert_eq!(Tab::parse(" Media "), Some(Tab::Media));
        assert_eq!(Tab::parse("system"), Some(Tab::System));
        assert_eq!(Tab::parse("weather"), None);
    }
}
