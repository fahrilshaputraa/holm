//! Animasi notch saat idle (tidak ada musik, timer, atau notifikasi). Semua vektor, warna polos.
//! - `rain`:  hujan di kaca (tetes yang meluncur + garis hujan), hanya saat hujan di kota pilihan.
//! - `cat`:   Holm si kucing mengintip, berkedip, tidur di malam hari, berpayung saat hujan.
//! - `aquarium`: ikan berenang; tinggi air = sisa baterai.
//! - `fireflies`: kunang-kunang berkelip; malam lebih banyak.
//! - `sky`:   matahari/bulan bergerak di busur langit mengikuti jam asli; malam ada bintang berkelip.
//! - `plant`: tunas yang tumbuh selama Timer berjalan, berbunga saat selesai, layu saat baterai lemah.
//! - `pulse`: garis detak (EKG) yang kecepatannya mengikuti beban CPU.
//! - `stars`: langit malam diam; sesekali bintang jatuh melintas.
//! - `wave`:  gelombang yang mengikuti suara yang sedang diputar; datar saat sunyi.
//! `next_tick_ms` memilih kapan perlu digambar ulang agar hemat (diam = jarang, bergerak = ~30 fps).

use crate::canvas::Canvas;
use crate::config;
use std::cell::Cell;
use std::f32::consts::PI;
use windows::Win32::System::SystemInformation::GetLocalTime;

/// Data dari notch untuk menggambar satu frame.
pub struct Input {
    pub ear: f32,
    pub body_w: f32,
    pub h: f32,
    pub s: f32,
    /// Opasitas (fade saat notch membuka/menutup).
    pub f: f32,
    pub bg: u32,
    pub tint: u32,
    /// Detik sejak notch dibuat (jam animasi).
    pub t: f32,
    pub cpu: f32,
    pub battery: Option<(u8, bool)>,
    /// Kemajuan Timer 0..1 bila sedang dipakai.
    pub growth: Option<f32>,
    pub bloom: bool,
    /// Radius sudut bawah badan notch (isi yang menyentuh dasar ikut membulat).
    pub br: f32,
}

thread_local! {
    /// Level suara yang diperhalus (naik cepat, turun pelan).
    static LEVEL: Cell<f32> = const { Cell::new(0.0) };
}

pub fn enabled() -> bool {
    !matches!(kind().as_str(), "off" | "")
}

fn kind() -> String {
    config::current().notch.idle_animation.trim().to_lowercase()
}

/// Hemat daya: baterai hampir habis (< 5%) tanpa pengisi daya atau aplikasi layar penuh = tanpa gerakan.
/// (Dulu < 20%: animasi tampak rusak/diam di laptop yang baterainya memang sering rendah.)
fn low_power(battery: Option<(u8, bool)>) -> bool {
    crate::FULLSCREEN.load(std::sync::atomic::Ordering::Relaxed) || matches!(battery, Some((p, false)) if p < 5)
}

fn hour() -> f32 {
    let t = unsafe { GetLocalTime() };
    t.wHour as f32 + t.wMinute as f32 / 60.0
}

/// Angka semu acak tetap per indeks (posisi bintang, jadwal meteor).
fn rnd(i: u32) -> f32 {
    let mut x = i.wrapping_mul(0x9E37_79B9).wrapping_add(0x7F4A_7C15);
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    (x % 10_000) as f32 / 10_000.0
}

/// Interval gambar ulang berikutnya (ms) untuk animasi aktif.
pub fn next_tick_ms(t: f32, battery: Option<(u8, bool)>) -> u32 {
    if low_power(battery) {
        return 30_000;
    }
    match kind().as_str() {
        "sky" => {
            let h = hour();
            if (6.0..18.0).contains(&h) {
                30_000 // matahari bergeser sangat pelan
            } else {
                120 // bintang berkelip
            }
        }
        "plant" => 90,
        "rain" => {
            if rain_state().is_some() {
                33
            } else {
                60_000 // cuaca berubah = notch digambar ulang lewat WM_STATUS_CHANGED
            }
        }
        "cat" | "aquarium" | "fireflies" => 50,
        "pulse" => 33,
        "stars" => {
            let (start, dur) = meteor_window(t);
            if t >= start && t <= start + dur {
                16
            } else {
                (((start - t) * 1000.0) as u32).clamp(16, 30_000)
            }
        }
        "wave" => {
            if LEVEL.with(|l| l.get()) > 0.01 || crate::status::audio_peak() > 0.01 {
                33
            } else {
                400 // cukup untuk mendeteksi suara mulai
            }
        }
        _ => 0,
    }
}

/// (mulai, durasi) bintang jatuh pada siklus 30 dtk berjalan (atau berikutnya bila sudah lewat).
fn meteor_window(t: f32) -> (f32, f32) {
    let dur = 0.9;
    let k = (t / 30.0).floor();
    let start = |k: f32| k * 30.0 + 4.0 + rnd(k as u32) * 18.0;
    let s0 = start(k);
    if t > s0 + dur {
        (start(k + 1.0), dur)
    } else {
        (s0, dur)
    }
}

pub fn draw(c: &Canvas, i: &Input) {
    let a = |col: u32| config::with_alpha(col, i.f * ((col >> 24) as f32 / 255.0));
    let still = low_power(i.battery);
    let t = if still { 0.0 } else { i.t };
    c.clip(i.ear, 0.0, i.body_w, i.h);
    match kind().as_str() {
        "sky" => sky(c, i, t, &a),
        "plant" => plant(c, i, t, &a),
        "pulse" => pulse(c, i, t, &a),
        "stars" => stars(c, i, t, &a),
        "wave" => wave(c, i, t, still, &a),
        "rain" => rain(c, i, t, &a),
        "cat" => cat(c, i, t, &a),
        "aquarium" => aquarium(c, i, t, &a),
        "fireflies" => fireflies(c, i, t, &a),
        _ => {}
    }
    c.reset_clip();
}

fn sky(c: &Canvas, i: &Input, t: f32, a: &dyn Fn(u32) -> u32) {
    let s = i.s;
    let pad = 16.0 * s;
    let (x0, w, h) = (i.ear + pad, i.body_w - 2.0 * pad, i.h);
    let hr = hour();
    let day = (6.0..18.0).contains(&hr);
    let frac = if day { (hr - 6.0) / 12.0 } else { (hr - 18.0).rem_euclid(24.0) / 12.0 };
    // Garis cakrawala tipis.
    c.fill_rect(x0, h * 0.8, w, 1.0 * s, a(0x26FFFFFF));
    let cx = x0 + frac * w;
    let cy = h * 0.78 - (frac * PI).sin() * h * 0.5;
    let r = 4.5 * s;
    if day {
        c.fill_ellipse(cx - r * 1.7, cy - r * 1.7, r * 3.4, r * 3.4, a(0x2EFAC775));
        c.fill_ellipse(cx - r, cy - r, 2.0 * r, 2.0 * r, a(0xFFFAC775));
    } else {
        for k in 0..7u32 {
            let sx = x0 + rnd(k * 3 + 1) * w;
            let sy = h * 0.12 + rnd(k * 3 + 2) * h * 0.55;
            let tw = 0.35 + 0.65 * (t * (0.6 + rnd(k) * 0.9) + k as f32).sin().abs();
            let d = (1.0 + rnd(k * 7) * 0.7) * s;
            c.fill_ellipse(sx - d / 2.0, sy - d / 2.0, d, d, a(config::with_alpha(0xFFFFFFFF, tw)));
        }
        // Bulan sabit: lingkaran terang ditutup lingkaran warna latar yang bergeser.
        c.fill_ellipse(cx - r, cy - r, 2.0 * r, 2.0 * r, a(0xFFE8E6DF));
        let o = (2.2 * s, -1.6 * s);
        c.fill_ellipse(cx - r * 0.9 + o.0, cy - r * 0.9 + o.1, r * 1.8, r * 1.8, a(config::with_alpha(i.bg, 1.0)));
    }
}

fn leaf(c: &Canvas, x: f32, y: f32, dir: f32, len: f32, col: u32) {
    c.fill_polygon(&[(x, y), (x + dir * len * 0.5, y - len * 0.42), (x + dir * len, y - len * 0.1), (x + dir * len * 0.45, y + len * 0.18)], col);
}

fn plant(c: &Canvas, i: &Input, t: f32, a: &dyn Fn(u32) -> u32) {
    let s = i.s;
    let g = i.growth.unwrap_or(0.3).clamp(0.0, 1.0);
    let wilt = matches!(i.battery, Some((p, false)) if p <= 20);
    let cx = i.ear + i.body_w / 2.0;
    let h = i.h;
    // Pot.
    let (pt, pb, ph) = (14.0 * s, 10.0 * s, 7.0 * s);
    let py = h - ph - 1.5 * s;
    c.fill_polygon(&[(cx - pt / 2.0, py), (cx + pt / 2.0, py), (cx + pb / 2.0, py + ph), (cx - pb / 2.0, py + ph)], a(0xFFBA7517));
    c.fill_rect(cx - pt / 2.0 - 1.0 * s, py - 1.5 * s, pt + 2.0 * s, 2.5 * s, a(0xFF854F0B));
    // Batang: tumbuh mengikuti Timer, bergoyang pelan; layu = ujung menunduk.
    let sh = ((4.0 + 13.0 * g) * s).min(py - 2.0 * s);
    let sway = (t * 1.6).sin() * 1.2 * s * (0.4 + g);
    let droop = if wilt { 5.0 * s } else { 0.0 };
    let base = (cx, py - 1.0 * s);
    let mid = (cx + sway * 0.4, py - sh * 0.55);
    let top = (cx + sway + droop, py - sh + if wilt { 3.0 * s } else { 0.0 });
    let green = if wilt { 0xFFA08A4A } else { 0xFF639922 };
    c.polyline(&[base, mid, top], 1.7 * s, a(green));
    let leaf_col = a(if wilt { 0xFFB9A25E } else { 0xFF97C459 });
    if g > 0.25 {
        leaf(c, mid.0, mid.1 + sh * 0.12, -1.0, 6.0 * s, leaf_col);
    }
    if g > 0.55 {
        let p = (cx + sway * 0.7, py - sh * 0.8);
        leaf(c, p.0, p.1, 1.0, 5.5 * s, leaf_col);
    }
    if i.bloom || g >= 1.0 {
        for k in 0..5 {
            let ang = k as f32 / 5.0 * 2.0 * PI + t * 0.3;
            let (px, py2) = (top.0 + ang.cos() * 2.6 * s, top.1 + ang.sin() * 2.6 * s);
            c.fill_ellipse(px - 2.1 * s, py2 - 2.1 * s, 4.2 * s, 4.2 * s, a(0xFFED93B1));
        }
        c.fill_ellipse(top.0 - 1.7 * s, top.1 - 1.7 * s, 3.4 * s, 3.4 * s, a(0xFFFAC775));
    } else if g > 0.85 {
        c.fill_ellipse(top.0 - 1.8 * s, top.1 - 2.2 * s, 3.6 * s, 3.6 * s, a(0xFFED93B1));
    } else {
        c.fill_ellipse(top.0 - 1.4 * s, top.1 - 1.4 * s, 2.8 * s, 2.8 * s, leaf_col);
    }
}

/// Bentuk satu detak (P, QRS, T) untuk fase 0..1.
fn ecg(p: f32) -> f32 {
    let bump = |p: f32, a: f32, b: f32, hgt: f32| if p > a && p < b { ((p - a) / (b - a) * PI).sin() * hgt } else { 0.0 };
    let tri = |p: f32, a: f32, m: f32, b: f32, hgt: f32| {
        if p > a && p <= m {
            (p - a) / (m - a) * hgt
        } else if p > m && p < b {
            (b - p) / (b - m) * hgt
        } else {
            0.0
        }
    };
    bump(p, 0.08, 0.18, 0.13) + tri(p, 0.25, 0.27, 0.29, -0.14) + tri(p, 0.29, 0.32, 0.35, 1.0) + tri(p, 0.35, 0.37, 0.40, -0.32) + bump(p, 0.52, 0.68, 0.24)
}

fn pulse(c: &Canvas, i: &Input, t: f32, a: &dyn Fn(u32) -> u32) {
    let s = i.s;
    let pad = 14.0 * s;
    let (x0, x1, h) = (i.ear + pad, i.ear + i.body_w - pad, i.h);
    let base = h * 0.56;
    let amp = h * 0.34;
    let bps = (55.0 + i.cpu.clamp(0.0, 1.0) * 100.0) / 60.0;
    let period = 46.0 * s;
    let col = if i.cpu > 0.8 { 0xFFFF9F0A } else { 0xFF30D158 };
    let step = 1.2 * s;
    let mut pts = Vec::new();
    let mut x = x0;
    while x <= x1 {
        let u = (x - x0) / period - t * bps;
        pts.push((x, base - ecg(u.rem_euclid(1.0)) * amp));
        x += step;
    }
    c.polyline(&pts, 1.6 * s, a(col));
    // Ujung meredup ke warna latar (kotak tembus pandang bertingkat, tanpa gradien).
    for k in 0..4 {
        let wk = 3.0 * s;
        let al = 0.75 - k as f32 * 0.2;
        c.fill_rect(x0 - 2.0 * s + k as f32 * wk, 0.0, wk, h, a(config::with_alpha(i.bg, al)));
        c.fill_rect(x1 - (k + 1) as f32 * wk + 2.0 * s, 0.0, wk, h, a(config::with_alpha(i.bg, al)));
    }
}

fn stars(c: &Canvas, i: &Input, t: f32, a: &dyn Fn(u32) -> u32) {
    let s = i.s;
    let pad = 14.0 * s;
    let (x0, w, h) = (i.ear + pad, i.body_w - 2.0 * pad, i.h);
    for k in 0..9u32 {
        let sx = x0 + rnd(k * 5 + 11) * w;
        let sy = h * 0.15 + rnd(k * 5 + 12) * h * 0.65;
        let d = (0.9 + rnd(k * 5 + 13) * 0.8) * s;
        let al = 0.45 + rnd(k * 5 + 14) * 0.45;
        c.fill_ellipse(sx - d / 2.0, sy - d / 2.0, d, d, a(config::with_alpha(0xFFFFFFFF, al)));
    }
    let (start, dur) = meteor_window(t);
    if t >= start && t <= start + dur {
        let p = (t - start) / dur;
        let k = (start / 30.0) as u32;
        let sx = x0 + w * (0.15 + rnd(k + 99) * 0.4);
        let (dx, dy) = (w * 0.38, h * 0.55);
        let hx = sx + dx * p;
        let hy = h * 0.12 + dy * p;
        let len = 16.0 * s;
        let n = (dx * dx + dy * dy).sqrt();
        let (ux, uy) = (dx / n, dy / n);
        let fade = (1.0 - p).min(p * 4.0).clamp(0.0, 1.0);
        c.line(hx - ux * len, hy - uy * len, hx, hy, 1.2 * s, a(config::with_alpha(0xFFFFFFFF, 0.35 * fade)));
        c.line(hx - ux * len * 0.45, hy - uy * len * 0.45, hx, hy, 1.4 * s, a(config::with_alpha(0xFFFFFFFF, 0.8 * fade)));
        c.fill_ellipse(hx - 1.3 * s, hy - 1.3 * s, 2.6 * s, 2.6 * s, a(config::with_alpha(0xFFFFFFFF, fade)));
    }
}

fn wave(c: &Canvas, i: &Input, t: f32, still: bool, a: &dyn Fn(u32) -> u32) {
    let s = i.s;
    let pad = 16.0 * s;
    let (x0, x1, h) = (i.ear + pad, i.ear + i.body_w - pad, i.h);
    let mid = h * 0.5;
    let peak = if still { 0.0 } else { crate::status::audio_peak() };
    let lvl = LEVEL.with(|l| {
        let v = peak.max(l.get() * 0.86);
        l.set(if v < 0.005 { 0.0 } else { v });
        l.get()
    });
    if lvl < 0.01 {
        c.fill_rect(x0, mid - 0.5 * s, x1 - x0, 1.0 * s, a(0x40FFFFFF));
        return;
    }
    let amp = (lvl.sqrt() * h * 0.36).min(h * 0.4);
    let step = 1.5 * s;
    for (k, (al, ph, sc, freq)) in [(0.45f32, 1.7f32, 0.6f32, 1.35f32), (1.0, 0.0, 1.0, 1.0)].into_iter().enumerate() {
        let mut pts = Vec::new();
        let mut x = x0;
        while x <= x1 {
            let xr = (x - x0) / (x1 - x0);
            let env = (xr * PI).sin();
            let y = mid + ((x - x0) / (11.0 * s) * freq + t * (6.0 + k as f32 * 1.5) + ph).sin() * amp * sc * env;
            pts.push((x, y));
            x += step;
        }
        c.polyline(&pts, if k == 1 { 1.6 * s } else { 1.2 * s }, a(config::with_alpha(i.tint, al)));
    }
}

// ---------- Ide 1–4: hujan di kaca, Holm si kucing, akuarium, kunang-kunang ----------

/// Hujan ditampilkan? Tanpa kota (tidak ada data cuaca) = hiasan, selalu hujan ringan.
fn rain_state() -> Option<(f32, bool)> {
    if config::current().weather.city.trim().is_empty() {
        return Some((0.5, false));
    }
    crate::weather::current().filter(|w| w.raining).map(|w| (w.intensity, w.storm))
}

fn frac(x: f32) -> f32 {
    x - x.floor()
}

/// Garis hujan miring di belakang (fungsi murni dari waktu, tanpa state).
#[allow(clippy::too_many_arguments)]
fn streaks(c: &Canvas, x0: f32, w: f32, h: f32, s: f32, t: f32, n: u32, a: &dyn Fn(u32) -> u32) {
    for k in 0..n {
        let len = (5.0 + rnd(k + 300) * 6.0) * s;
        let speed = (55.0 + rnd(k + 200) * 35.0) * s;
        let y = frac(rnd(k) + t * speed / (h + len)) * (h + len) - len;
        let x = x0 + rnd(k + 100) * (w + 20.0 * s) - (y + len) * 0.3;
        c.line(x, y, x - len * 0.3, y + len, 0.9 * s, a(0x38AABED7));
    }
}

/// Satu tetes air di kaca: badan transparan, tepi terang, dan titik pantulan.
fn glass_drop(c: &Canvas, x: f32, y: f32, r: f32, s: f32, a: &dyn Fn(u32) -> u32) {
    c.fill_ellipse(x - r * 0.9, y - r, r * 1.8, r * 2.0, a(0x478CA0B4));
    c.stroke_arc(x, y, r * 0.92, 20.0, 130.0, 0.8 * s, a(0x8CD2E1F0));
    let d = (r * 0.45).max(0.9 * s);
    c.fill_ellipse(x - r * 0.35 - d / 2.0, y - r * 0.4 - d / 2.0, d, d, a(0xE6FFFFFF));
}

fn rain(c: &Canvas, i: &Input, t: f32, a: &dyn Fn(u32) -> u32) {
    let Some((intensity, storm)) = rain_state() else { return };
    let s = i.s;
    let (x0, w, h) = (i.ear, i.body_w, i.h);
    streaks(c, x0, w, h, s, t, (8.0 + 22.0 * intensity) as u32, a);
    // Tetes di kaca: diam sebentar, lalu sebagian meluncur turun dan muncul lagi di tempat baru.
    for k in 0..(10.0 + 14.0 * intensity) as u32 {
        let period = 5.0 + rnd(k + 400) * 7.0;
        let u = (t + rnd(k + 500) * period) / period;
        let cycle = u.floor() as u32;
        let p = frac(u);
        let r = (1.0 + rnd(k * 31 + cycle) * 1.8) * s;
        let x = x0 + 6.0 * s + rnd(k * 17 + cycle * 7) * (w - 12.0 * s);
        let y0 = rnd(k * 13 + cycle * 3) * h;
        let slides = r > 2.0 * s;
        let sliding = slides && p > 0.65;
        let y = if sliding { y0 + ((p - 0.65) / 0.35).powi(2) * (h + 6.0 * s) } else { y0 };
        if sliding {
            // Jejak tetes kecil di belakangnya.
            for j in 1..4 {
                let ty = y - j as f32 * 2.6 * s;
                if ty > y0 {
                    glass_drop(c, x + (j as f32 * 1.3).sin() * 0.4 * s, ty, 0.6 * s, s, a);
                }
            }
        }
        glass_drop(c, x, y, r, s, a);
    }
    if storm && frac(t / 7.0) < 0.012 {
        c.fill_rect(x0, 0.0, w, h, a(0x2EDCE6FF));
    }
}

fn cat(c: &Canvas, i: &Input, t: f32, a: &dyn Fn(u32) -> u32) {
    let s = i.s;
    let (x0, w, h) = (i.ear, i.body_w, i.h);
    let k = h / 58.0; // geometri digambar untuk tinggi 58, diskalakan ke tinggi notch
    let lw = (1.6 * k).max(0.8);
    let raining = !config::current().weather.city.trim().is_empty() && rain_state().is_some();
    if raining {
        streaks(c, x0, w, h, s, t, 12, a);
    }
    let hr = hour();
    let sleeping = !(6.0..22.0).contains(&hr);
    let cx = x0 + w / 2.0;
    let cy = h - 6.0 * k + (t / 0.9).sin() * 1.5 * k;
    let p = |x: f32, y: f32| (cx + x * k, cy + y * k);
    // Kepala + telinga.
    c.fill_polygon(&[p(-26.0, -14.0), p(-20.0, -34.0), p(-8.0, -22.0), p(8.0, -22.0), p(20.0, -34.0), p(26.0, -14.0), p(26.0, 10.0), p(-26.0, 10.0)], a(0xFF2C2C2A));
    c.fill_polygon(&[p(-20.0, -30.0), p(-13.0, -22.5), p(-22.0, -20.0)], a(0xFF72243E));
    c.fill_polygon(&[p(20.0, -30.0), p(13.0, -22.5), p(22.0, -20.0)], a(0xFF72243E));
    if sleeping {
        // Mata terpejam + "z" kecil melayang.
        for ex in [-11.0, 11.0] {
            let (ox, oy) = p(ex, -10.0);
            c.stroke_arc(ox, oy - 1.5 * k, 3.6 * k, 20.0, 140.0, lw, a(0xFFCFCFD4));
        }
        let z = frac(t / 2.4);
        let (zx, zy) = p(32.0 + z * 10.0, -22.0 - z * 14.0);
        let zs = 5.0 * k;
        c.polyline(&[(zx, zy), (zx + zs, zy), (zx, zy + zs), (zx + zs, zy + zs)], lw * 0.8, a(config::with_alpha(0xFFFFFFFF, 0.85 * (1.0 - z))));
    } else {
        // Mata berkedip tiap ~3,8 dtk.
        let blink = frac(t / 3.8) < 0.035;
        for ex in [-11.0, 11.0] {
            let (ox, oy) = p(ex, -10.0);
            let (ew, eh) = (7.2 * k, if blink { 2.4 * k } else { 12.0 * k });
            c.fill_ellipse(ox - ew / 2.0, oy - eh / 2.0, ew, eh, a(0xFFFAC775));
            if !blink {
                let (pw, ph) = (3.2 * k, 10.0 * k);
                c.fill_ellipse(ox - pw / 2.0, oy - ph / 2.0, pw, ph, a(0xFF16161A));
            }
        }
    }
    // Hidung + kumis.
    c.fill_polygon(&[p(-2.5, -3.0), p(2.5, -3.0), p(0.0, 0.0)], a(0xFFF4C0D1));
    for sgn in [-1.0f32, 1.0] {
        for (y0, y1) in [(-2.0, -5.0), (0.0, 1.0)] {
            let (ax, ay) = p(sgn * 6.0, y0);
            let (bx, by) = p(sgn * 22.0, y1);
            c.line(ax, ay, bx, by, lw * 0.5, a(0xFF5F5E5A));
        }
    }
    // Payung saat hujan (cuaca asli).
    if raining {
        let (ux, uy) = p(44.0, -18.0);
        let ur = 14.0 * k;
        c.fill_round_rect(ux - ur, uy - ur, 2.0 * ur, ur, (ur, ur, 0.0, 0.0), a(0xFFE24B4A));
        c.line(ux, uy, ux, uy + 20.0 * k, lw, a(0xFF888780));
        c.stroke_arc(ux - 3.0 * k, uy + 20.0 * k, 3.0 * k, 0.0, 180.0, lw, a(0xFF888780));
    }
}

fn aquarium(c: &Canvas, i: &Input, t: f32, a: &dyn Fn(u32) -> u32) {
    let s = i.s;
    let (x0, w, h) = (i.ear, i.body_w, i.h);
    // Tinggi air = sisa baterai (tanpa baterai: tiga perempat).
    let lvl = i.battery.map(|b| b.0 as f32 / 100.0).unwrap_or(0.75);
    let top = h * (1.0 - (0.12 + 0.78 * lvl));
    let wob = (t * 2.0).sin() * 0.6 * s;
    // Sudut bawah air mengikuti lengkung sudut bawah notch (bukan kotak).
    let br = i.br.min((h - top - wob).max(0.0));
    c.fill_round_rect(x0, top + wob, w, h - top - wob, (0.0, 0.0, br, br), a(0xFF0C447C));
    let mut pts = Vec::new();
    let mut x = x0;
    while x <= x0 + w {
        pts.push((x, top + wob + ((t * 2.5) + (x - x0) / (9.0 * s)).sin() * 0.8 * s));
        x += 2.0 * s;
    }
    c.polyline(&pts, 1.0 * s, a(0xFF378ADD));
    // Ikan: bolak-balik dengan kecepatan berbeda, sedikit naik-turun.
    let fish = [(0xFFFAC775u32, 0.55f32, 0.0f32, 0.55f32), (0xFFF0997B, 0.4, 0.37, 0.35), (0xFF85B7EB, 0.3, 0.71, 0.75)];
    let pad = 10.0 * s;
    for (col, sp, off, yr) in fish {
        let u = frac(t * sp / (w / (40.0 * s)) + off) * 2.0;
        let (pos, dir) = if u < 1.0 { (u, 1.0) } else { (2.0 - u, -1.0) };
        let fx = x0 + pad + pos * (w - 2.0 * pad);
        let water = (h - top).max(6.0 * s);
        let fy = (top + water * (0.25 + 0.6 * yr) + (t * 1.7 + off * 9.0).sin() * 1.0 * s).min(h - 3.0 * s);
        let (bw, bh) = (4.2 * s, 2.4 * s);
        c.fill_ellipse(fx - bw, fy - bh, 2.0 * bw, 2.0 * bh, a(col));
        c.fill_polygon(&[(fx - dir * bw * 0.8, fy), (fx - dir * bw * 1.6, fy - bh * 0.9), (fx - dir * bw * 1.6, fy + bh * 0.9)], a(col));
        c.fill_ellipse(fx + dir * bw * 0.45 - 0.5 * s, fy - 0.9 * s, 1.0 * s, 1.0 * s, a(0xFF04342C));
    }
    // Gelembung naik.
    for k in 0..4u32 {
        let span = (h - top).max(1.0);
        let by = h - frac(t / (2.5 + rnd(k) * 1.5) + rnd(k + 9)) * span;
        let bx = x0 + w * (0.15 + 0.23 * k as f32) + (t * 2.0 + k as f32).sin() * 1.0 * s;
        let d = 1.6 * s;
        c.fill_ellipse(bx - d / 2.0, by - d / 2.0, d, d, a(0x99E6F1FB));
    }
}

fn fireflies(c: &Canvas, i: &Input, t: f32, a: &dyn Fn(u32) -> u32) {
    let s = i.s;
    let (x0, w, h) = (i.ear, i.body_w, i.h);
    let hr = hour();
    let n = if (6.0..18.0).contains(&hr) { 5 } else { 9 };
    for k in 0..n {
        let x = x0 + frac(rnd(k + 700) + t * 0.012 * (0.5 + rnd(k + 710))) * w + (t * 1.3 + k as f32).sin() * 3.0 * s;
        let y = h * (0.25 + 0.5 * rnd(k + 720)) + (t * 0.7 + k as f32 * 1.7).cos() * h * 0.15;
        let glow = 0.35 + 0.65 * ((t * 2.2 * (0.6 + rnd(k + 730)) + k as f32).sin()).max(0.0);
        let halo = 3.2 * s;
        c.fill_ellipse(x - halo, y - halo, 2.0 * halo, 2.0 * halo, a(config::with_alpha(0xFFFAC775, 0.22 * glow)));
        let d = 1.4 * s;
        c.fill_ellipse(x - d / 2.0, y - d / 2.0, d, d, a(config::with_alpha(0xFFFAEEDA, glow)));
    }
}
