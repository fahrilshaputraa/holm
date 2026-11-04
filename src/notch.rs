//! Notch / dynamic island ala MacBook di tengah atas layar.
//! - Bentuk menyatu dengan menu bar (telinga cekung di kiri/kanan atas).
//! - Animasi spring (sedikit memantul) saat membuka/menutup.
//! - Terbuka: sampul, judul/artis, lirik tersinkron, progres (klik untuk seek), kontrol.
//! Timer hanya hidup saat ada animasi, atau saat terbuka + musik diputar (untuk progres/lirik).

use crate::canvas::{Align, Canvas, Font, Image};
use crate::config;
use crate::lyrics::{self, Lyrics};
use crate::media::{self, Command, MediaInfo};
use crate::monitors::Mon;
use crate::notifications::Toast;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;
use windows::Win32::Foundation::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const TIMER_ANIM: usize = 1;
const TIMER_CLOSE: usize = 2;
const TIMER_PROGRESS: usize = 3;
const TIMER_NOTIF: usize = 4;
const FRAME_MS: u32 = 16;
const PROGRESS_MS: u32 = 250;

type Rect = (f32, f32, f32, f32);

/// State satu notch (satu per monitor).
struct State {
    mon: Mon,
    target_open: bool,
    /// Posisi spring: 0 = ringkas, 1 = terbuka (boleh sedikit melewati 1 saat memantul).
    x: f32,
    v: f32,
    last: Instant,
    tracking: bool,
    art: Option<(usize, Image)>,
    /// Spring kartu notifikasi (0 = tidak ada, 1 = kartu penuh) dan statusnya.
    nt: f32,
    nv: f32,
    n_target: bool,
    notif: Option<Toast>,
    notif_img: Option<Image>,
    queue: VecDeque<Toast>,
    hover: bool,
}

thread_local! {
    static WINS: RefCell<HashMap<isize, State>> = RefCell::new(HashMap::new());
}

fn with<R>(hwnd: HWND, f: impl FnOnce(&mut State) -> R) -> Option<R> {
    WINS.with(|w| w.borrow_mut().get_mut(&(hwnd.0 as isize)).map(f))
}

pub fn attach(hwnd: HWND, mon: Mon) {
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
                nt: 0.0,
                nv: 0.0,
                n_target: false,
                notif: None,
                notif_img: None,
                queue: VecDeque::new(),
                hover: false,
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

struct Layout {
    art: Rect,
    title: Rect,
    artist: Rect,
    lyric1: Rect,
    lyric2: Rect,
    time_l: Rect,
    time_r: Rect,
    bar: Rect,
    prev: (f32, f32),
    play: (f32, f32),
    next: (f32, f32),
}

/// Tata letak isi saat terbuka. Koordinat dalam jendela; badan notch mulai di x = ear.
fn layout(body_w: f32, ear: f32, ch: f32, s: f32) -> Layout {
    let bx = ear;
    let pad = 22.0 * s;
    let y0 = ch + 6.0 * s;
    let iw = body_w - 2.0 * pad;
    let art_s = 64.0 * s;
    let tx = bx + pad + art_s + 14.0 * s;
    let tw = bx + body_w - pad - tx;
    let ty = y0 + 130.0 * s;
    let cy = y0 + 160.0 * s;
    let cx = bx + body_w / 2.0;
    Layout {
        art: (bx + pad, y0, art_s, art_s),
        title: (tx, y0 + 6.0 * s, tw, 22.0 * s),
        artist: (tx, y0 + 30.0 * s, tw, 18.0 * s),
        lyric1: (bx + pad, y0 + 78.0 * s, iw, 24.0 * s),
        lyric2: (bx + pad, y0 + 102.0 * s, iw, 18.0 * s),
        time_l: (bx + pad, ty - 8.0 * s, 40.0 * s, 16.0 * s),
        time_r: (bx + body_w - pad - 40.0 * s, ty - 8.0 * s, 40.0 * s, 16.0 * s),
        bar: (bx + pad + 46.0 * s, ty - 2.0 * s, iw - 92.0 * s, 4.0 * s),
        prev: (cx - 72.0 * s, cy),
        play: (cx, cy),
        next: (cx + 72.0 * s, cy),
    }
}

fn fmt_time(ms: u64) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Dimensi akhir (badan ringkas, badan terbuka, tinggi ringkas, tinggi terbuka) dalam piksel.
fn dims(s: f32) -> (f32, f32, f32, f32) {
    let n = &config::current().notch;
    (n.compact_width as f32 * s, n.expanded_width as f32 * s, n.compact_height as f32 * s, n.expanded_height as f32 * s)
}

fn is_settled_open(hwnd: HWND) -> bool {
    with(hwnd, |st| st.target_open && (st.x - 1.0).abs() < 0.01 && st.v.abs() < 0.05).unwrap_or(false)
}

/// Timer progres/lirik hanya saat terbuka penuh dan musik diputar.
fn sync_progress_timer(hwnd: HWND) {
    let want = is_settled_open(hwnd) && media::info().playing;
    unsafe {
        if want {
            SetTimer(Some(hwnd), TIMER_PROGRESS, PROGRESS_MS, None);
        } else {
            let _ = KillTimer(Some(hwnd), TIMER_PROGRESS);
        }
    }
}

/// Dipanggil saat info media/lirik berubah.
pub fn on_media(hwnd: HWND) {
    redraw(hwnd);
    sync_progress_timer(hwnd);
}

fn draw_lyrics(c: &Canvas, l: &Layout, info: &MediaInfo, fade: f32, s: f32, font: &str) {
    let big = Font::new(font, 14.5 * s, true);
    let small = Font::new(font, 12.0 * s, false);
    let a = |col: u32| config::with_alpha(col, fade * ((col >> 24) as f32 / 255.0));
    let (mut line1, mut line2) = (String::new(), String::new());
    match lyrics::get(info.key) {
        None => {}
        Some(Lyrics::Loading) => line1 = "Memuat lirik...".into(),
        Some(Lyrics::NotFound) => line1 = "Lirik tidak ditemukan".into(),
        Some(Lyrics::Instrumental) => line1 = "Instrumental".into(),
        Some(Lyrics::Synced(lines)) => {
            let pos = info.position_ms();
            match lyrics::current_line(&lines, pos) {
                Some(i) => {
                    line1 = if lines[i].1.is_empty() { "\u{266A}".into() } else { lines[i].1.clone() };
                    line2 = lines.get(i + 1).map(|(_, t)| t.clone()).unwrap_or_default();
                }
                None => {
                    line1 = "\u{266A}".into();
                    line2 = lines.first().map(|(_, t)| t.clone()).unwrap_or_default();
                }
            }
        }
        Some(Lyrics::Plain(lines)) => {
            // Tanpa stempel waktu: perkiraan baris dari persentase lagu.
            let n = lines.iter().filter(|x| !x.is_empty()).count().max(1);
            let nonempty: Vec<&String> = lines.iter().filter(|x| !x.is_empty()).collect();
            let frac = if info.duration_ms > 0 { info.position_ms() as f32 / info.duration_ms as f32 } else { 0.0 };
            let i = ((frac * n as f32) as usize).min(n - 1);
            line1 = nonempty.get(i).map(|s| s.to_string()).unwrap_or_default();
            line2 = nonempty.get(i + 1).map(|s| s.to_string()).unwrap_or_default();
        }
    }
    let dim = matches!(lyrics::get(info.key), None | Some(Lyrics::Loading | Lyrics::NotFound | Lyrics::Instrumental));
    let c1 = if dim { 0xFF8E8E98 } else { 0xFFFFFFFF };
    c.text(&line1, &big, l.lyric1, a(c1), Align::Center);
    c.text(&line2, &small, l.lyric2, a(0xFF7A7A85), Align::Center);
}

/// Ukuran badan kartu notifikasi (lebar, tinggi total termasuk strip bar di atasnya).
fn notif_dims(s: f32, ch: f32) -> (f32, f32) {
    (372.0 * s, ch + 66.0 * s)
}

/// Pusat tombol tutup (X) pada kartu notifikasi.
fn notif_close(ear: f32, body_w: f32, ch: f32, s: f32) -> (f32, f32) {
    (ear + body_w - 18.0 * s, ch + 14.0 * s)
}

fn draw_notif(hwnd: HWND, c: &Canvas, body_w: f32, ear: f32, ch: f32, s: f32, fade: f32, font: &str, accent: u32) {
    let cfg = config::current();
    let Some((toast, hover)) = with(hwnd, |st| st.notif.clone().map(|t| (t, st.hover))).flatten() else { return };
    let a = |col: u32| config::with_alpha(col, fade * ((col >> 24) as f32 / 255.0));
    let (pad, isz) = (16.0 * s, 46.0 * s);
    let (_, nh) = notif_dims(s, ch);
    let iy = ch + (nh - ch - isz) / 2.0;
    let ix = ear + pad;
    let r = 11.0 * s;

    let drawn = with(hwnd, |st| match &st.notif_img {
        Some(img) => {
            c.draw_image(img, ix, iy, isz, isz, r, fade);
            true
        }
        None => false,
    })
    .unwrap_or(false);
    if !drawn {
        // Tanpa gambar banner: monogram dari nama aplikasi/judul.
        c.fill_round_rect(ix, iy, isz, isz, (r, r, r, r), a(config::with_alpha(accent, 0.85)));
        let mono = toast.app.chars().chain(toast.title.chars()).find(|ch| ch.is_alphanumeric()).map(|ch| ch.to_uppercase().to_string()).unwrap_or_else(|| "!".into());
        c.text(&mono, &Font::new(font, 21.0 * s, true), (ix, iy, isz, isz), a(0xFFFFFFFF), Align::Center);
    }

    let tx = ix + isz + 12.0 * s;
    let tw = (ear + body_w - pad - tx - 22.0 * s).max(10.0);
    let app = if toast.app.is_empty() { "Notifikasi" } else { toast.app.as_str() };
    c.text(app, &Font::new(font, 10.5 * s, false), (tx, iy - 3.0 * s, tw, 14.0 * s), a(0xFF8E8E98), Align::Near);
    c.text(&toast.title, &Font::new(font, 13.5 * s, true), (tx, iy + 11.0 * s, tw, 19.0 * s), a(0xFFFFFFFF), Align::Near);
    if cfg.notifications.show_body && !toast.body.is_empty() {
        c.text(&toast.body, &Font::new(font, 12.0 * s, false), (tx, iy + 30.0 * s, tw, 17.0 * s), a(0xFFB4B4BE), Align::Near);
    }

    if hover {
        let (cx, cy) = notif_close(ear, body_w, ch, s);
        let d = 18.0 * s;
        c.fill_ellipse(cx - d / 2.0, cy - d / 2.0, d, d, a(0x40FFFFFF));
        c.text("\u{00D7}", &Font::new(font, 14.0 * s, true), (cx - d / 2.0, cy - d / 2.0 - 1.0 * s, d, d), a(0xFFFFFFFF), Align::Center);
    }
}

/// Tampilkan banner notifikasi di notch ini (antre bila sedang menampilkan yang lain; maks 8).
pub fn notify(hwnd: HWND, t: Toast) {
    if !config::current().notifications.enabled {
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
        st.notif_img = t.icon.as_ref().and_then(|b| Image::from_bytes(b));
        st.notif = Some(t);
        st.n_target = true;
        st.hover = false;
        st.last = Instant::now();
        true
    })
    .unwrap_or(false);
    if started {
        start_anim(hwnd, false); // panel media menutup dulu
        let dur = config::current().notifications.duration_ms.max(1000);
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

pub fn redraw(hwnd: HWND) {
    let cfg = config::current();
    let info = media::info();

    // Sampul: decode ulang hanya saat gambar berganti.
    let want = info.thumb.as_ref().map(|t| Arc::as_ptr(t) as usize);
    let Some(mut art) = with(hwnd, |st| st.art.take()) else { return };
    match (want, &art) {
        (Some(p), Some((q, _))) if p == *q => {}
        (Some(p), _) => art = info.thumb.as_ref().and_then(|t| Image::from_bytes(t)).map(|i| (p, i)),
        (None, _) => art = None,
    }

    let Some((x, nt, mon)) = with(hwnd, |st| (st.x, st.nt, st.mon)) else { return };
    let s = mon.scale;
    let (cw, ew, ch, eh) = dims(s);
    let ear = 8.0 * s;
    let xc = x.clamp(0.0, 1.15);
    let (nw, nh) = notif_dims(s, ch);
    let nc = nt.clamp(0.0, 1.1);
    let body_w = lerp(lerp(cw, ew, xc), nw, nc).round();
    let h = lerp(lerp(ch, eh, xc), nh, nc).round();
    let w = body_w + 2.0 * ear;
    let t = x.clamp(0.0, 1.0);

    let bg = config::color(&cfg.notch.background);
    let accent = config::color(&cfg.notch.accent);

    if let Some(c) = Canvas::new(w as i32, h as i32) {
        c.fill_notch(w, h, ear, lerp(lerp(10.0 * s, 28.0 * s, t), 22.0 * s, nt.clamp(0.0, 1.0)), bg);

        if nt > 0.5 {
            draw_notif(hwnd, &c, body_w, ear, ch, s, ((nt - 0.5) / 0.4).clamp(0.0, 1.0), &cfg.bar.font, accent);
        }

        if t < 0.45 && nt < 0.25 {
            // Ringkas: sampul kecil di kiri, indikator di kanan (hanya bila ada media).
            let f = (1.0 - t / 0.45).clamp(0.0, 1.0);
            if info.active {
                let sz = 20.0 * s;
                let (ax, ay) = (ear + 9.0 * s, (ch - sz) / 2.0);
                match &art {
                    Some((_, img)) => c.draw_image(img, ax, ay, sz, sz, 5.0 * s, f),
                    None => c.fill_round_rect(ax, ay, sz, sz, (5.0 * s, 5.0 * s, 5.0 * s, 5.0 * s), config::with_alpha(accent, f)),
                }
                let by = ch / 2.0;
                let rx = ear + body_w - 12.0 * s;
                if info.playing {
                    for (i, bh) in [8.0f32, 15.0, 11.0, 6.0].iter().enumerate() {
                        let bh = bh * s;
                        c.fill_round_rect(rx - 16.0 * s + i as f32 * 4.5 * s, by - bh / 2.0, 2.5 * s, bh, (1.2 * s, 1.2 * s, 1.2 * s, 1.2 * s), config::with_alpha(accent, f));
                    }
                } else {
                    let col = config::with_alpha(0xFF9E9EA8, f);
                    c.fill_round_rect(rx - 10.0 * s, by - 5.0 * s, 3.0 * s, 10.0 * s, (1.0 * s, 1.0 * s, 1.0 * s, 1.0 * s), col);
                    c.fill_round_rect(rx - 4.0 * s, by - 5.0 * s, 3.0 * s, 10.0 * s, (1.0 * s, 1.0 * s, 1.0 * s, 1.0 * s), col);
                }
            }
        }

        if t > 0.55 && nt < 0.25 {
            let fade = ((t - 0.55) / 0.35).clamp(0.0, 1.0);
            let a = |col: u32| config::with_alpha(col, fade * ((col >> 24) as f32 / 255.0));
            let l = layout(body_w, ear, ch, s);
            let title_f = Font::new(&cfg.bar.font, 15.0 * s, true);
            let sub_f = Font::new(&cfg.bar.font, 12.5 * s, false);
            let time_f = Font::new(&cfg.bar.font, 10.5 * s, false);

            match &art {
                Some((_, img)) => c.draw_image(img, l.art.0, l.art.1, l.art.2, l.art.3, 11.0 * s, fade),
                None => {
                    c.fill_round_rect(l.art.0, l.art.1, l.art.2, l.art.3, (11.0 * s, 11.0 * s, 11.0 * s, 11.0 * s), a(0xFF26262B));
                    let (cx, cy) = (l.art.0 + l.art.2 / 2.0, l.art.1 + l.art.3 / 2.0);
                    c.fill_polygon(&[(cx - 7.0 * s, cy - 10.0 * s), (cx - 7.0 * s, cy + 10.0 * s), (cx + 10.0 * s, cy)], a(config::with_alpha(accent, 0.9)));
                }
            }

            let (title, artist) = if info.active {
                (if info.title.is_empty() { "Tanpa judul".to_string() } else { info.title.clone() }, info.artist.clone())
            } else {
                ("Tidak ada media".to_string(), "Putar musik atau video untuk mengontrolnya di sini".to_string())
            };
            c.text(&title, &title_f, l.title, a(0xFFFFFFFF), Align::Near);
            c.text(&artist, &sub_f, l.artist, a(0xFF9E9EA8), Align::Near);

            if info.active {
                draw_lyrics(&c, &l, &info, fade, s, &cfg.bar.font);

                // Progres.
                let pos = info.position_ms();
                let (bx, by, bw, bh) = l.bar;
                c.fill_round_rect(bx, by, bw, bh, (bh / 2.0, bh / 2.0, bh / 2.0, bh / 2.0), a(0x40FFFFFF));
                let frac = if info.duration_ms > 0 { pos as f32 / info.duration_ms as f32 } else { 0.0 };
                if frac > 0.0 {
                    let fw = (bw * frac).max(bh);
                    c.fill_round_rect(bx, by, fw, bh, (bh / 2.0, bh / 2.0, bh / 2.0, bh / 2.0), a(0xFFFFFFFF));
                }
                c.text(&fmt_time(pos), &time_f, l.time_l, a(0xFF9E9EA8), Align::Near);
                let right = if info.duration_ms > 0 { format!("-{}", fmt_time(info.duration_ms.saturating_sub(pos))) } else { "LIVE".into() };
                c.text(&right, &time_f, l.time_r, a(0xFF9E9EA8), Align::Far);
            }

            // Kontrol transport.
            let ctrl = if info.active { 0xFFFFFFFF } else { 0xFF55555F };
            let u = 9.0 * s;
            let (px, py) = l.prev;
            c.fill_round_rect(px - u - 1.0 * s, py - u, 2.5 * s, 2.0 * u, (1.0 * s, 1.0 * s, 1.0 * s, 1.0 * s), a(ctrl));
            c.fill_polygon(&[(px + u, py - u), (px + u, py + u), (px - u + 3.0 * s, py)], a(ctrl));
            let (nx, ny) = l.next;
            c.fill_round_rect(nx + u - 1.5 * s, ny - u, 2.5 * s, 2.0 * u, (1.0 * s, 1.0 * s, 1.0 * s, 1.0 * s), a(ctrl));
            c.fill_polygon(&[(nx - u, ny - u), (nx - u, ny + u), (nx + u - 3.0 * s, ny)], a(ctrl));
            let (qx, qy) = l.play;
            let pu = 13.0 * s;
            if info.playing {
                c.fill_round_rect(qx - pu * 0.72, qy - pu, pu * 0.55, 2.0 * pu, (2.0 * s, 2.0 * s, 2.0 * s, 2.0 * s), a(ctrl));
                c.fill_round_rect(qx + pu * 0.17, qy - pu, pu * 0.55, 2.0 * pu, (2.0 * s, 2.0 * s, 2.0 * s, 2.0 * s), a(ctrl));
            } else {
                c.fill_polygon(&[(qx - pu * 0.72, qy - pu), (qx - pu * 0.72, qy + pu), (qx + pu, qy)], a(ctrl));
            }
        }

        c.present(hwnd, Some((mon.x + ((mon.w as f32 - w) / 2.0).round() as i32, mon.y)));
    }
    with(hwnd, |st| st.art = art);
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
    unsafe {
        let _ = KillTimer(Some(hwnd), TIMER_PROGRESS);
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
            sync_progress_timer(hwnd);
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

fn near(p: (f32, f32), x: f32, y: f32, r: f32) -> bool {
    (x - p.0).abs() <= r && (y - p.1).abs() <= r
}

fn click(hwnd: HWND, mx: f32, my: f32) {
    let Some((open, s)) = with(hwnd, |st| (st.target_open, st.mon.scale)) else { return };
    if notif_active(hwnd) {
        let (nw, _) = notif_dims(s, dims(s).2);
        let (cx, cy) = notif_close(8.0 * s, nw, dims(s).2, s);
        if !near((cx, cy), mx, my, 12.0 * s) {
            crate::notifications::activate(); // teruskan klik ke banner aslinya
        }
        dismiss_notif(hwnd);
        return;
    }
    if open && is_settled_open(hwnd) {
        let (_, ew, ch, _) = dims(s);
        let ear = 8.0 * s;
        let l = layout(ew, ear, ch, s);
        let r = 22.0 * s;
        if near(l.prev, mx, my, r) {
            media::send(Command::Previous);
            return;
        }
        if near(l.play, mx, my, r) {
            media::send(Command::TogglePlayPause);
            return;
        }
        if near(l.next, mx, my, r) {
            media::send(Command::Next);
            return;
        }
        let (bx, by, bw, bh) = l.bar;
        let info = media::info();
        if info.active && info.duration_ms > 0 && mx >= bx - 4.0 * s && mx <= bx + bw + 4.0 * s && (my - (by + bh / 2.0)).abs() <= 10.0 * s {
            let frac = ((mx - bx) / bw).clamp(0.0, 1.0);
            media::send(Command::Seek((frac * info.duration_ms as f32) as u64));
            return;
        }
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
                if first {
                    unsafe {
                        let _ = KillTimer(Some(hwnd), TIMER_NOTIF);
                    }
                    redraw(hwnd);
                }
                return LRESULT(0);
            }
            if config::current().notch.expand_on_hover {
                start_anim(hwnd, true);
            }
            LRESULT(0)
        }
        0x02A3 => {
            // WM_MOUSELEAVE
            with(hwnd, |st| st.tracking = false);
            if notif_active(hwnd) {
                with(hwnd, |st| st.hover = false);
                let dur = config::current().notifications.duration_ms.max(1000);
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
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let x = (lparam.0 & 0xFFFF) as i16 as f32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32;
            click(hwnd, x, y);
            LRESULT(0)
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
                TIMER_PROGRESS => {
                    if is_settled_open(hwnd) && media::info().playing {
                        redraw(hwnd);
                    } else {
                        sync_progress_timer(hwnd);
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
