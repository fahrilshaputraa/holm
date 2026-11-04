//! Lirik dari LRCLIB (https://lrclib.net), diambil lewat WinHTTP bawaan Windows.
//! Hanya judul/artis/durasi lagu yang dikirim; bisa dimatikan lewat `[lyrics] enabled = false`.

use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use windows::core::PCWSTR;
use windows::Win32::Networking::WinHttp::*;
use windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW;

pub static ENABLED: AtomicBool = AtomicBool::new(true);
static NOTIFY: AtomicU32 = AtomicU32::new(0);
static STATE: Mutex<Option<(u64, Lyrics)>> = Mutex::new(None);

#[derive(Clone, Debug, PartialEq)]
pub enum Lyrics {
    Loading,
    NotFound,
    Instrumental,
    /// (milidetik, teks) terurut menurut waktu.
    Synced(Vec<(u64, String)>),
    Plain(Vec<String>),
}

pub fn set_notify(thread_id: u32) {
    NOTIFY.store(thread_id, Ordering::Relaxed);
}

/// Lirik untuk lagu `key`; None bila belum pernah diminta / dimatikan.
pub fn get(key: u64) -> Option<Lyrics> {
    STATE.lock().ok().and_then(|g| g.as_ref().filter(|(k, _)| *k == key).map(|(_, l)| l.clone()))
}

fn store(key: u64, l: Lyrics) {
    if let Ok(mut g) = STATE.lock() {
        // Abaikan hasil usang bila lagu sudah berganti.
        if matches!(g.as_ref(), Some((k, _)) if *k != key) {
            return;
        }
        *g = Some((key, l));
    }
    let t = NOTIFY.load(Ordering::Relaxed);
    if t != 0 {
        unsafe {
            let _ = PostThreadMessageW(
                t,
                crate::media::WM_MEDIA_CHANGED,
                windows::Win32::Foundation::WPARAM(0),
                windows::Win32::Foundation::LPARAM(0),
            );
        }
    }
}

/// Minta lirik untuk lagu baru (no-op jika sudah diminta untuk `key`).
pub fn request(key: u64, artist: &str, title: &str, duration_ms: u64) {
    if !ENABLED.load(Ordering::Relaxed) || title.is_empty() {
        return;
    }
    if let Ok(mut g) = STATE.lock() {
        if matches!(g.as_ref(), Some((k, _)) if *k == key) {
            return;
        }
        *g = Some((key, Lyrics::Loading));
    }
    let (artist, title) = (artist.to_string(), title.to_string());
    std::thread::spawn(move || store(key, fetch(&artist, &title, duration_ms)));
}

fn encode(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => o.push(b as char),
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
}

/// "Judul (feat. X) - Remastered" -> "Judul", agar pencarian lebih toleran.
fn clean_title(t: &str) -> String {
    let cut = [" (", " [", " - "].iter().filter_map(|p| t.find(p)).min().unwrap_or(t.len());
    t[..cut].trim().to_string()
}

fn http_get(path: &str) -> Option<(u32, Vec<u8>)> {
    let w = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    unsafe {
        let agent = w("HormBar/0.1");
        let session = WinHttpOpen(PCWSTR(agent.as_ptr()), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0);
        if session.is_null() {
            return None;
        }
        let _ = WinHttpSetTimeouts(session, 5000, 5000, 8000, 8000);
        let host = w("lrclib.net");
        let conn = WinHttpConnect(session, PCWSTR(host.as_ptr()), INTERNET_DEFAULT_HTTPS_PORT, 0);
        let mut result = None;
        if !conn.is_null() {
            let verb = w("GET");
            let p = w(path);
            let req = WinHttpOpenRequest(conn, PCWSTR(verb.as_ptr()), PCWSTR(p.as_ptr()), PCWSTR::null(), PCWSTR::null(), null(), WINHTTP_FLAG_SECURE);
            if !req.is_null() {
                if WinHttpSendRequest(req, None, None, 0, 0, 0).is_ok() && WinHttpReceiveResponse(req, null_mut()).is_ok() {
                    let mut status = 0u32;
                    let mut len = std::mem::size_of::<u32>() as u32;
                    let _ = WinHttpQueryHeaders(
                        req,
                        WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                        PCWSTR::null(),
                        Some(&mut status as *mut u32 as *mut _),
                        &mut len,
                        null_mut(),
                    );
                    let mut body = Vec::new();
                    let mut buf = [0u8; 8192];
                    loop {
                        let mut n = 0u32;
                        if WinHttpReadData(req, buf.as_mut_ptr() as *mut _, buf.len() as u32, &mut n).is_err() || n == 0 {
                            break;
                        }
                        body.extend_from_slice(&buf[..n as usize]);
                        if body.len() > 2_000_000 {
                            break;
                        }
                    }
                    result = Some((status, body));
                }
                let _ = WinHttpCloseHandle(req);
            }
            let _ = WinHttpCloseHandle(conn);
        }
        let _ = WinHttpCloseHandle(session);
        result
    }
}

fn from_entry(v: &serde_json::Value) -> Option<Lyrics> {
    if v["instrumental"].as_bool() == Some(true) {
        return Some(Lyrics::Instrumental);
    }
    if let Some(s) = v["syncedLyrics"].as_str().filter(|s| !s.trim().is_empty()) {
        let lines = parse_lrc(s);
        if !lines.is_empty() {
            return Some(Lyrics::Synced(lines));
        }
    }
    v["plainLyrics"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .map(|s| Lyrics::Plain(s.lines().map(|l| l.trim().to_string()).collect()))
}

fn fetch(artist: &str, title: &str, duration_ms: u64) -> Lyrics {
    let mut q = format!("/api/get?artist_name={}&track_name={}", encode(artist), encode(title));
    if duration_ms > 0 {
        q.push_str(&format!("&duration={}", (duration_ms as f64 / 1000.0).round() as u64));
    }
    if let Some((200, body)) = http_get(&q) {
        if let Some(l) = serde_json::from_slice::<serde_json::Value>(&body).ok().and_then(|v| from_entry(&v)) {
            return l;
        }
    }
    // Fallback: pencarian yang lebih toleran terhadap variasi judul.
    let q = format!("/api/search?track_name={}&artist_name={}", encode(&clean_title(title)), encode(artist));
    if let Some((200, body)) = http_get(&q) {
        if let Ok(serde_json::Value::Array(items)) = serde_json::from_slice::<serde_json::Value>(&body) {
            let best = items
                .iter()
                .filter_map(from_entry)
                .find(|l| matches!(l, Lyrics::Synced(_)))
                .or_else(|| items.iter().filter_map(from_entry).next());
            if let Some(l) = best {
                return l;
            }
        }
    }
    Lyrics::NotFound
}

/// Format LRC: "[mm:ss.xx] teks" (boleh beberapa stempel waktu per baris).
pub fn parse_lrc(src: &str) -> Vec<(u64, String)> {
    let mut out = Vec::new();
    for line in src.lines() {
        let mut rest = line.trim();
        let mut stamps = Vec::new();
        while let Some(r) = rest.strip_prefix('[') {
            let Some(end) = r.find(']') else { break };
            let tag = &r[..end];
            rest = r[end + 1..].trim_start();
            let Some((m, s)) = tag.split_once(':') else { break };
            let (Ok(m), Ok(s)) = (m.parse::<u64>(), s.parse::<f64>()) else { break }; // [ar:...], [ti:...] dilewati
            stamps.push(m * 60_000 + (s * 1000.0) as u64);
        }
        for t in stamps {
            out.push((t, rest.to_string()));
        }
    }
    out.sort_by_key(|(t, _)| *t);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lrc_and_skips_metadata() {
        let l = parse_lrc("[ar:Someone]\n[00:01.50] satu\n[00:10.00][01:00.25]dua\n[bad]x\n");
        assert_eq!(l, vec![(1500, "satu".into()), (10000, "dua".into()), (60250, "dua".into())]);
    }

    #[test]
    fn finds_current_line() {
        let l = parse_lrc("[00:01.00]a\n[00:05.00]b");
        assert_eq!(current_line(&l, 500), None);
        assert_eq!(current_line(&l, 1000), Some(0));
        assert_eq!(current_line(&l, 7000), Some(1));
    }

    #[test]
    fn cleans_titles() {
        assert_eq!(clean_title("Yellow (Remastered 2011)"), "Yellow");
        assert_eq!(clean_title("Song - Live"), "Song");
    }

    /// Butuh internet: `cargo test -- --ignored live`
    #[test]
    #[ignore]
    fn live_fetch_from_lrclib() {
        match fetch("Coldplay", "Yellow", 269_000) {
            Lyrics::Synced(v) => {
                println!("synced lines: {}, first: {:?}", v.len(), v.first());
                assert!(v.len() > 10);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}

/// Indeks baris yang sedang dinyanyikan pada `pos_ms` (None sebelum baris pertama).
pub fn current_line(lines: &[(u64, String)], pos_ms: u64) -> Option<usize> {
    match lines.partition_point(|(t, _)| *t <= pos_ms) {
        0 => None,
        n => Some(n - 1),
    }
}
