//! Cuaca dari Open-Meteo (gratis, tanpa akun/API key) untuk animasi notch.
//! Privasi: hanya NAMA KOTA dari config (`weather.city`) yang dikirim untuk dicari koordinatnya, lalu koordinat
//! kota itu untuk perkiraan cuaca. Tanpa kota, tidak ada permintaan apa pun. Dibaca ulang tiap 20 menit.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Weather {
    /// Sedang hujan (atau gerimis/badai) di kota itu.
    pub raining: bool,
    /// Intensitas 0..1 (perkiraan dari mm/jam).
    pub intensity: f32,
    pub storm: bool,
}

static NOW: Mutex<Option<Weather>> = Mutex::new(None);
static CITY: Mutex<String> = Mutex::new(String::new());
static NOTIFY: AtomicU32 = AtomicU32::new(0);

/// Cuaca terakhir; None = belum ada data atau kota tidak diisi.
pub fn current() -> Option<Weather> {
    *NOW.lock().ok()?
}

/// Kota diubah lewat config: dipakai pada pembacaan berikutnya (segera).
pub fn set_city(city: &str) {
    let changed = CITY.lock().map(|mut c| {
        let ch = c.as_str() != city.trim();
        *c = city.trim().to_string();
        ch
    });
    if changed.unwrap_or(false) {
        if let Ok(mut n) = NOW.lock() {
            *n = None;
        }
        KICK.1.notify_one();
    }
}

static KICK: (Mutex<()>, std::sync::Condvar) = (Mutex::new(()), std::sync::Condvar::new());

fn get_json(host: &str, path: &str) -> Option<serde_json::Value> {
    let (status, body) = crate::lyrics::https_get(host, path)?;
    if status != 200 {
        return None;
    }
    serde_json::from_slice(&body).ok()
}

fn locate(city: &str) -> Option<(f64, f64)> {
    let v = get_json("geocoding-api.open-meteo.com", &format!("/v1/search?count=1&format=json&name={}", crate::lyrics::encode(city)))?;
    let r = v.get("results")?.get(0)?;
    Some((r.get("latitude")?.as_f64()?, r.get("longitude")?.as_f64()?))
}

fn fetch(lat: f64, lon: f64) -> Option<Weather> {
    let v = get_json("api.open-meteo.com", &format!("/v1/forecast?latitude={lat:.3}&longitude={lon:.3}&current=precipitation,weather_code&timezone=auto"))?;
    let cur = v.get("current")?;
    let mm = cur.get("precipitation").and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
    let code = cur.get("weather_code").and_then(|x| x.as_i64()).unwrap_or(0);
    // Kode WMO: 51–67 gerimis/hujan, 80–82 hujan lokal, 95–99 badai petir.
    let rain_code = matches!(code, 51..=67 | 80..=82 | 95..=99);
    Some(Weather { raining: rain_code || mm > 0.05, intensity: (mm / 4.0).clamp(0.15, 1.0), storm: matches!(code, 95..=99) })
}

pub fn start(main_tid: u32) {
    NOTIFY.store(main_tid, Ordering::Relaxed);
    std::thread::spawn(|| {
        let mut located: Option<(String, (f64, f64))> = None;
        loop {
            let city = CITY.lock().map(|c| c.clone()).unwrap_or_default();
            let mut next = Duration::from_secs(20 * 60);
            if city.is_empty() {
                located = None;
            } else {
                if located.as_ref().map(|(c, _)| c != &city).unwrap_or(true) {
                    located = locate(&city).map(|p| (city.clone(), p));
                }
                let w = located.as_ref().and_then(|(_, (la, lo))| fetch(*la, *lo));
                if w.is_none() {
                    next = Duration::from_secs(3 * 60); // jaringan gagal: coba lagi lebih cepat
                }
                let changed = NOW.lock().map(|mut n| std::mem::replace(&mut *n, w) != w).unwrap_or(false);
                if changed {
                    crate::status::post();
                }
            }
            let start = Instant::now();
            if let Ok(g) = KICK.0.lock() {
                let _ = KICK.1.wait_timeout(g, next.saturating_sub(start.elapsed()));
            }
        }
    });
}
