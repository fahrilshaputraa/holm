//! Media "Now Playing" lewat Windows SMTC. Berbasis event: thread latar hanya bangun
//! saat sesi/metadata/status/timeline berubah, lalu memberi tahu thread UI.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use windows::Foundation::TypedEventHandler;
use windows::Media::Control::*;
use windows::Storage::Streams::DataReader;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_APP};

pub const WM_MEDIA_CHANGED: u32 = WM_APP + 1;

#[derive(Clone)]
pub struct MediaInfo {
    pub active: bool,
    pub playing: bool,
    pub title: String,
    pub artist: String,
    /// Identitas lagu (hash judul+artis); berubah saat lagu berganti.
    pub key: u64,
    pub duration_ms: u64,
    /// Posisi saat `stamp`; gunakan `position_ms()` untuk posisi sekarang.
    pos_ms: u64,
    stamp: Instant,
    /// Bytes gambar sampul (PNG/JPEG) bila tersedia.
    pub thumb: Option<Arc<Vec<u8>>>,
    /// AppUserModelID / nama exe aplikasi pemutar (mis. "Spotify.exe", "MSEdge").
    pub source: String,
}

impl Default for MediaInfo {
    fn default() -> Self {
        MediaInfo {
            active: false,
            playing: false,
            title: String::new(),
            artist: String::new(),
            key: 0,
            duration_ms: 0,
            pos_ms: 0,
            stamp: Instant::now(),
            thumb: None,
            source: String::new(),
        }
    }
}

impl PartialEq for MediaInfo {
    fn eq(&self, o: &Self) -> bool {
        self.active == o.active
            && self.playing == o.playing
            && self.key == o.key
            && self.source == o.source
            && self.duration_ms == o.duration_ms
            && self.pos_ms == o.pos_ms
            && match (&self.thumb, &o.thumb) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            }
    }
}

impl MediaInfo {
    /// Posisi sekarang, diinterpolasi sejak pembacaan terakhir saat sedang diputar.
    pub fn position_ms(&self) -> u64 {
        let mut p = self.pos_ms;
        if self.playing {
            p += self.stamp.elapsed().as_millis() as u64;
        }
        if self.duration_ms > 0 {
            p = p.min(self.duration_ms);
        }
        p
    }
}

static NOTIFY: AtomicU32 = AtomicU32::new(0);
static INFO: Mutex<Option<MediaInfo>> = Mutex::new(None);
static SESSION: Mutex<Option<Hooked>> = Mutex::new(None);
static THUMB_CACHE: Mutex<(u64, Option<Arc<Vec<u8>>>)> = Mutex::new((0, None));

struct Hooked {
    session: GlobalSystemMediaTransportControlsSession,
    tokens: [i64; 3],
}

pub fn info() -> MediaInfo {
    INFO.lock().ok().and_then(|g| g.clone()).unwrap_or_default()
}

fn post() {
    let t = NOTIFY.load(Ordering::Relaxed);
    if t != 0 {
        unsafe {
            let _ = PostThreadMessageW(t, WM_MEDIA_CHANGED, WPARAM(0), LPARAM(0));
        }
    }
}

fn publish(info: MediaInfo) {
    if info.active {
        crate::lyrics::request(info.key, &info.artist, &info.title, info.duration_ms);
    }
    if let Ok(mut g) = INFO.lock() {
        if g.as_ref() == Some(&info) {
            return;
        }
        *g = Some(info);
    }
    post();
}

fn fetch_thumb(props: &GlobalSystemMediaTransportControlsSessionMediaProperties) -> Option<Arc<Vec<u8>>> {
    let stream = props.Thumbnail().ok()?.OpenReadAsync().ok()?.join().ok()?;
    let size = stream.Size().ok()? as u32;
    if size == 0 || size > 8_000_000 {
        return None;
    }
    let reader = DataReader::CreateDataReader(&stream).ok()?;
    reader.LoadAsync(size).ok()?.join().ok()?;
    let mut buf = vec![0u8; size as usize];
    reader.ReadBytes(&mut buf).ok()?;
    Some(Arc::new(buf))
}

/// FILETIME (100 ns sejak 1601) saat ini.
fn now_filetime() -> i64 {
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    (d.as_nanos() / 100) as i64 + 116_444_736_000_000_000
}

fn read(session: &GlobalSystemMediaTransportControlsSession) -> windows::core::Result<MediaInfo> {
    let props = session.TryGetMediaPropertiesAsync()?.join()?;
    let status = session.GetPlaybackInfo()?.PlaybackStatus()?;
    let playing = status == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing;
    let title = props.Title()?.to_string();
    let artist = props.Artist()?.to_string();

    let mut h = DefaultHasher::new();
    (&title, &artist).hash(&mut h);
    let key = h.finish();

    let (mut duration_ms, mut pos_ms) = (0u64, 0u64);
    if let Ok(tl) = session.GetTimelineProperties() {
        let ticks = |r: windows::core::Result<windows::Foundation::TimeSpan>| r.map(|t| t.Duration).unwrap_or(0);
        let start = ticks(tl.StartTime());
        let end = ticks(tl.EndTime());
        let mut pos = ticks(tl.Position()) - start;
        if playing {
            // Posisi dilaporkan pada LastUpdatedTime; maju sejak itu.
            if let Ok(last) = tl.LastUpdatedTime() {
                pos += (now_filetime() - last.UniversalTime).max(0);
            }
        }
        duration_ms = ((end - start).max(0) / 10_000) as u64;
        pos_ms = (pos.max(0) / 10_000) as u64;
        if duration_ms > 0 {
            pos_ms = pos_ms.min(duration_ms);
        }
    }

    // Sampul: ambil ulang hanya jika lagu berganti atau sebelumnya belum ada.
    let thumb = {
        let cached = THUMB_CACHE.lock().ok().and_then(|c| if c.0 == key { c.1.clone() } else { None });
        match cached {
            Some(t) => Some(t),
            None => {
                let t = fetch_thumb(&props);
                if let Ok(mut c) = THUMB_CACHE.lock() {
                    *c = (key, t.clone());
                }
                t
            }
        }
    };

    let source = session.SourceAppUserModelId().map(|s| s.to_string()).unwrap_or_default();
    Ok(MediaInfo { active: true, playing, title, artist, key, duration_ms, pos_ms, stamp: Instant::now(), thumb, source })
}

fn refresh_session(session: &GlobalSystemMediaTransportControlsSession) {
    match read(session) {
        Ok(i) => publish(i),
        Err(_) => publish(MediaInfo::default()),
    }
}

fn attach(manager: &GlobalSystemMediaTransportControlsSessionManager) {
    let current = manager.GetCurrentSession().ok();
    let Ok(mut slot) = SESSION.lock() else { return };

    if let Some(old) = slot.take() {
        let _ = old.session.RemoveMediaPropertiesChanged(old.tokens[0]);
        let _ = old.session.RemovePlaybackInfoChanged(old.tokens[1]);
        let _ = old.session.RemoveTimelinePropertiesChanged(old.tokens[2]);
    }
    let Some(session) = current else {
        drop(slot);
        publish(MediaInfo::default());
        return;
    };

    let s1 = session.clone();
    let t0 = session
        .MediaPropertiesChanged(&TypedEventHandler::new(move |_, _| {
            refresh_session(&s1);
            Ok(())
        }))
        .unwrap_or(0);
    let s2 = session.clone();
    let t1 = session
        .PlaybackInfoChanged(&TypedEventHandler::new(move |_, _| {
            refresh_session(&s2);
            Ok(())
        }))
        .unwrap_or(0);
    let s3 = session.clone();
    let t2 = session
        .TimelinePropertiesChanged(&TypedEventHandler::new(move |_, _| {
            refresh_session(&s3);
            Ok(())
        }))
        .unwrap_or(0);

    refresh_session(&session);
    *slot = Some(Hooked { session, tokens: [t0, t1, t2] });
}

/// Mulai pemantauan; thread UI `thread_id` menerima WM_MEDIA_CHANGED saat ada perubahan.
pub fn start(thread_id: u32) {
    NOTIFY.store(thread_id, Ordering::Relaxed);
    crate::lyrics::set_notify(thread_id);
    std::thread::spawn(|| {
        unsafe {
            let _ = RoInitialize(RO_INIT_MULTITHREADED);
        }
        let Ok(op) = GlobalSystemMediaTransportControlsSessionManager::RequestAsync() else { return };
        let Ok(manager) = op.join() else { return };

        attach(&manager);
        let m = manager.clone();
        let _ = manager.CurrentSessionChanged(&TypedEventHandler::new(move |_, _| {
            attach(&m);
            Ok(())
        }));
        // Handler berjalan di thread pool; thread ini hanya menjaga `manager` tetap hidup.
        loop {
            std::thread::park();
        }
    });
}

pub enum Command {
    TogglePlayPause,
    Previous,
    Next,
    /// Lompat ke posisi (milidetik).
    Seek(u64),
}

/// Kirim perintah ke sesi aktif. Mengembalikan false bila tidak ada sesi.
pub fn send(cmd: Command) -> bool {
    let Ok(slot) = SESSION.lock() else { return false };
    let Some(h) = slot.as_ref() else { return false };
    match cmd {
        Command::TogglePlayPause => drop(h.session.TryTogglePlayPauseAsync()),
        Command::Previous => drop(h.session.TrySkipPreviousAsync()),
        Command::Next => drop(h.session.TrySkipNextAsync()),
        Command::Seek(ms) => drop(h.session.TryChangePlaybackPositionAsync(ms as i64 * 10_000)),
    }
    true
}
