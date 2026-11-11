//! Notifikasi Windows di notch.
//!
//! Banner dideteksi REALTIME lewat UI Automation (teks banner; Windows baru memasukkan toast ke daftar
//! notifikasi setelah bannernya selesai, ±8 dtk kemudian, jadi daftar itu terlalu lambat sebagai sumber).
//! Logo asli aplikasi pengirim diambil dari `UserNotificationListener` (API resmi): pembaca latar
//! menyimpan peta nama aplikasi -> logo dari daftar notifikasi, dan banner baru memakainya. Bila tidak ada,
//! jatuh ke gambar banner yang disalin dari layar, lalu ikon Start Menu (notch.rs), lalu monogram.
//! Listener tidak pernah menampilkan notifikasi sendiri, jadi Jangan Ganggu tetap dihormati.
//! Klik kartu = Invoke banner aslinya, atau Win+N bila banner sudah hilang. Teks hanya hidup di memori.

use std::collections::HashMap;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};
use windows::core::Interface;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_APP};
use windows_core::implement;

pub const WM_NOTIFY_TOAST: u32 = WM_APP + 30;

/// Jenis kartu di notch: notifikasi biasa, panggilan masuk, atau aktivitas sistem (perangkat Bluetooth, pengisi daya).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Message,
    Call,
    Bluetooth { battery: Option<u8>, audio: bool },
    Charging { pct: u8 },
}

#[derive(Clone)]
pub struct Toast {
    pub app: String,
    pub title: String,
    pub body: String,
    /// Logo asli aplikasi pengirim dari UserNotificationListener (bila sudah diketahui).
    pub icon: Option<Vec<u8>>,
    /// Gambar di dalam banner (BMP) bila ada: avatar kontak / gambar pesan.
    pub image: Option<Vec<u8>>,
    /// Nama tombol aksi di banner aslinya (mis. "Accept", "Decline"); diklik lewat `invoke_action`.
    pub actions: Vec<String>,
    pub kind: Kind,
}

impl Toast {
    pub fn message(app: &str, title: &str, body: &str) -> Toast {
        Toast { app: app.into(), title: title.into(), body: body.into(), icon: None, image: None, actions: Vec::new(), kind: Kind::Message }
    }

    /// Tombol aksi yang cocok dengan salah satu kata kunci (huruf kecil).
    pub fn action_like(&self, words: &[&str]) -> Option<&str> {
        self.actions.iter().find(|a| {
            let l = a.to_lowercase();
            words.iter().any(|w| l.contains(w))
        }).map(|s| s.as_str())
    }
}

pub const ACCEPT_WORDS: &[&str] = &["accept", "answer", "terima", "jawab", "angkat", "pick up"];
pub const DECLINE_WORDS: &[&str] = &["decline", "reject", "tolak", "ignore", "abaikan", "hang up", "end call", "akhiri"];

/// Banner panggilan masuk (WhatsApp, Phone Link, Teams, Zoom, Telegram, ...)?
fn is_call(app: &str, title: &str, body: &str, actions: &[String]) -> bool {
    let has = |words: &[&str]| actions.iter().any(|a| {
        let l = a.to_lowercase();
        words.iter().any(|w| l.contains(w))
    });
    if has(ACCEPT_WORDS) && has(DECLINE_WORDS) {
        return true;
    }
    let text = format!("{app} {title} {body}").to_lowercase();
    ["incoming call", "incoming voice call", "incoming video call", "is calling", "calling you", "panggilan masuk", "panggilan suara masuk", "panggilan video masuk", "sedang menelepon", "memanggil anda"]
        .iter()
        .any(|k| text.contains(k))
}

#[derive(Default)]
struct Signal {
    scan: bool,
    invoke: bool,
    action: Option<String>,
}

static SIGNAL: Mutex<Signal> = Mutex::new(Signal { scan: false, invoke: false, action: None });
static CV: Condvar = Condvar::new();

/// Minta pemindaian cepat (ada jendela baru yang mungkin sebuah banner).
pub fn kick() {
    if let Ok(mut g) = SIGNAL.lock() {
        g.scan = true;
        CV.notify_one();
    }
}

/// Teruskan klik ke banner asli (atau buka Notification Center).
pub fn activate() {
    if let Ok(mut g) = SIGNAL.lock() {
        g.invoke = true;
        CV.notify_one();
    }
}

/// Klik tombol aksi bernama `name` di banner asli (mis. Terima/Tolak panggilan).
pub fn invoke_action(name: &str) {
    if let Ok(mut g) = SIGNAL.lock() {
        g.action = Some(name.to_string());
        CV.notify_one();
    }
}

/// Peta nama aplikasi (huruf kecil) -> logo asli dari UserNotificationListener (None = aplikasi tanpa logo).
static LOGOS: Mutex<Option<HashMap<String, Option<Vec<u8>>>>> = Mutex::new(None);

fn logo_for(app: &str) -> Option<Vec<u8>> {
    let key = app.trim().to_lowercase();
    if key.is_empty() {
        return None;
    }
    LOGOS.lock().ok()?.as_ref()?.get(&key).cloned().flatten()
}

pub fn start(main_tid: u32, poll_ms: u32) {
    std::thread::spawn(move || unsafe { worker(main_tid, poll_ms) });
    std::thread::spawn(logo_worker);
}

/// Log diagnostik ke notif-debug.txt bila HOLM_DEBUG diset.
fn dbg(msg: &str) {
    if std::env::var_os("HOLM_DEBUG").is_some() {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(crate::config::dir().join("notif-debug.txt")) {
            let _ = writeln!(f, "{msg}");
        }
    }
}

fn clean(s: &str) -> String {
    s.replace(['\u{200e}', '\u{200f}'], "").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Baca isi stream (logo) menjadi byte gambar.
fn read_ref(r: windows::Storage::Streams::RandomAccessStreamReference) -> Option<Vec<u8>> {
    use windows::Storage::Streams::DataReader;
    let stream = r.OpenReadAsync().ok()?.join().ok()?;
    let size = stream.Size().ok()? as u32;
    if size == 0 || size > 4_000_000 {
        return None;
    }
    let reader = DataReader::CreateDataReader(&stream).ok()?;
    reader.LoadAsync(size).ok()?.join().ok()?;
    let mut buf = vec![0u8; size as usize];
    reader.ReadBytes(&mut buf).ok()?;
    Some(buf)
}

/// Latar: kumpulkan logo asli tiap aplikasi dari daftar notifikasi Windows (izin diminta sekali).
fn logo_worker() {
    use windows::UI::Notifications::Management::{UserNotificationListener, UserNotificationListenerAccessStatus};
    use windows::UI::Notifications::NotificationKinds;
    use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
    let Ok(l) = UserNotificationListener::Current() else { return };
    let access = l.RequestAccessAsync().and_then(|o| o.join());
    dbg(&format!("logo: akses listener = {access:?}"));
    if !matches!(access, Ok(UserNotificationListenerAccessStatus::Allowed)) {
        return; // tanpa izin: jatuh ke gambar banner / ikon Start Menu
    }
    if let Ok(mut g) = LOGOS.lock() {
        g.get_or_insert_with(HashMap::new);
    }
    loop {
        if let Ok(list) = l.GetNotificationsAsync(NotificationKinds::Toast).and_then(|o| o.join()) {
            for n in list {
                let Some(disp) = n.AppInfo().ok().and_then(|a| a.DisplayInfo().ok()) else { continue };
                let Some(name) = disp.DisplayName().ok().map(|s| clean(&s.to_string()).to_lowercase()) else { continue };
                let known = LOGOS.lock().ok().and_then(|g| g.as_ref().map(|m| m.contains_key(&name))).unwrap_or(false);
                if known || name.is_empty() {
                    continue;
                }
                // Nama -> AppUserModelID yang pasti benar: ikon resolusi tinggi diambil dari shell (apps.rs).
                if let Ok(id) = n.AppInfo().and_then(|a| a.AppUserModelId()) {
                    crate::apps::remember(&name, &id.to_string());
                }
                let logo = disp.GetLogo(windows::Foundation::Size { Width: 64.0, Height: 64.0 }).ok().and_then(read_ref);
                dbg(&format!("logo: '{name}' -> {}", if logo.is_some() { "ada" } else { "tidak ada" }));
                if let Ok(mut g) = LOGOS.lock() {
                    g.get_or_insert_with(HashMap::new).insert(name, logo);
                }
            }
        }
        std::thread::sleep(Duration::from_secs(3));
    }
}

#[implement(IUIAutomationEventHandler)]
struct Handler;

impl IUIAutomationEventHandler_Impl for Handler_Impl {
    fn HandleAutomationEvent(&self, _sender: windows_core::Ref<'_, IUIAutomationElement>, _eventid: UIA_EVENT_ID) -> windows_core::Result<()> {
        kick();
        Ok(())
    }
}

struct Scanner {
    uia: IUIAutomation,
    /// Hash (judul+isi) banner yang sudah dikirim, dengan waktunya, agar banner yang masih tampil tidak berulang.
    seen: Vec<(u64, Instant)>,
    /// Elemen banner terakhir, untuk Invoke saat kartu diklik.
    last: Option<IUIAutomationElement>,
    procs: HashMap<u32, String>,
    main_tid: u32,
}

unsafe fn worker(main_tid: u32, poll_ms: u32) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let Ok(uia) = CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER) else { return };
        if let Ok(root) = uia.GetRootElement() {
            let h: IUIAutomationEventHandler = Handler.into();
            let _ = uia.AddAutomationEventHandler(UIA_Window_WindowOpenedEventId, &root, TreeScope_Subtree, None::<&IUIAutomationCacheRequest>, &h);
        }
        let mut sc = Scanner { uia, seen: Vec::new(), last: None, procs: HashMap::new(), main_tid };
        loop {
            let (mut burst, mut single, mut invoke) = (false, false, false);
            let action: Option<String>;
            {
                let Ok(mut g) = SIGNAL.lock() else { return };
                if !g.scan && !g.invoke && g.action.is_none() {
                    let dur = if poll_ms > 0 { Duration::from_millis(poll_ms as u64) } else { Duration::from_secs(3600) };
                    match CV.wait_timeout(g, dur) {
                        Ok((g2, to)) => {
                            g = g2;
                            single = to.timed_out() && poll_ms > 0;
                        }
                        Err(_) => return,
                    }
                }
                burst |= std::mem::take(&mut g.scan);
                invoke |= std::mem::take(&mut g.invoke);
                action = g.action.take();
            }
            if let Some(a) = action {
                sc.invoke_action(&a);
            }
            if invoke {
                sc.invoke();
            }
            if burst {
                // Banner masuk dengan animasi geser; pindai beberapa kali selama ±2 dtk.
                for d in [0u64, 200, 300, 500, 800] {
                    std::thread::sleep(Duration::from_millis(d));
                    sc.scan();
                }
            } else if single {
                sc.scan();
            }
        }
    }
}

fn bstr(r: windows::core::Result<windows::core::BSTR>) -> String {
    r.map(|b| b.to_string()).unwrap_or_default()
}

fn hash(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

impl Scanner {
    /// Nama exe (huruf kecil) proses `pid`, di-cache.
    fn image(&mut self, pid: u32) -> String {
        self.procs
            .entry(pid)
            .or_insert_with(|| unsafe {
                let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return String::new() };
                let mut buf = [0u16; 520];
                let mut len = buf.len() as u32;
                let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
                let _ = CloseHandle(h);
                if !ok {
                    return String::new();
                }
                let full = String::from_utf16_lossy(&buf[..len as usize]).to_lowercase();
                full.rsplit('\\').next().unwrap_or("").to_string()
            })
            .clone()
    }

    fn scan(&mut self) {
        unsafe {
            self.seen.retain(|(_, t)| t.elapsed() < Duration::from_secs(20));
            let (Ok(root), Ok(cond)) = (self.uia.GetRootElement(), self.uia.CreateTrueCondition()) else { return };
            let Ok(kids) = root.FindAll(TreeScope_Children, &cond) else { return };
            for i in 0..kids.Length().unwrap_or(0) {
                let Ok(win) = kids.GetElement(i) else { continue };
                let class = bstr(win.CurrentClassName());
                if matches!(class.as_str(), "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "Progman" | "WorkerW" | "HolmBarClass" | "HolmNotchClass" | "HolmDockClass" | "HolmPopupClass") {
                    continue;
                }
                let img = self.image(win.CurrentProcessId().unwrap_or(0) as u32);
                // Panggilan WhatsApp (dan sejenis) tampil sebagai jendela aplikasi sendiri, bukan banner toast:
                // jendela kecil milik aplikasi pengirim yang punya tombol Terima/Accept.
                if img.contains("whatsapp") || img.contains("telegram") || img.contains("signal") {
                    let small = win.CurrentBoundingRectangle().map(|r| (1..=800).contains(&(r.right - r.left)) && (1..=800).contains(&(r.bottom - r.top))).unwrap_or(false);
                    if small {
                        self.read_toast(&win, &cond, true);
                    }
                    continue;
                }
                let host = match img.as_str() {
                    "shellexperiencehost.exe" | "shellhost.exe" => true,
                    // explorer.exe juga punya jendela biasa (folder): hanya host XAML/toast-nya yang dipindai.
                    "explorer.exe" => ["CoreWindow", "Xaml", "Toast", "Notification", "Island"].iter().any(|k| class.contains(k)),
                    _ => false,
                };
                if !host {
                    continue;
                }
                let wname = bstr(win.CurrentName()).to_lowercase();
                if class.contains("NotificationCenter") || wname.contains("notification center") || wname.contains("pusat notifikasi") {
                    continue; // riwayat, bukan banner
                }
                self.scan_host(&win, &cond);
            }
        }
    }

    unsafe fn scan_host(&mut self, win: &IUIAutomationElement, cond: &IUIAutomationCondition) {
        unsafe {
            let Ok(all) = win.FindAll(TreeScope_Descendants, cond) else { return };
            let n = all.Length().unwrap_or(0).min(400);
            for i in 0..n {
                let Ok(e) = all.GetElement(i) else { continue };
                if !bstr(e.CurrentAutomationId()).starts_with("NormalToastView") {
                    continue;
                }
                self.read_toast(&e, cond, false);
                return;
            }
        }
    }

    unsafe fn read_toast(&mut self, toast: &IUIAutomationElement, cond: &IUIAutomationCondition, call_window: bool) {
        unsafe {
            let mut texts: Vec<String> = Vec::new();
            let mut actions: Vec<String> = Vec::new();
            let mut image: Option<IUIAutomationElement> = None;
            let mut best = 0i32;
            if let Ok(kids) = toast.FindAll(TreeScope_Descendants, cond) {
                for i in 0..kids.Length().unwrap_or(0).min(200) {
                    let Ok(k) = kids.GetElement(i) else { continue };
                    let ct = k.CurrentControlType().unwrap_or_default();
                    if ct == UIA_ButtonControlTypeId {
                        let n = bstr(k.CurrentName()).trim().to_string();
                        let l = n.to_lowercase();
                        let generic = ["close", "tutup", "dismiss", "setting", "pengaturan", "option", "opsi", "more", "lainnya"].iter().any(|w| l.contains(w));
                        if !n.is_empty() && !generic && !actions.contains(&n) {
                            actions.push(n);
                        }
                    } else if ct == UIA_TextControlTypeId {
                        let t = bstr(k.CurrentName()).trim().to_string();
                        if !t.is_empty() && !texts.contains(&t) {
                            texts.push(t);
                        }
                    } else if ct == UIA_ImageControlTypeId {
                        if let Ok(r) = k.CurrentBoundingRectangle() {
                            let (w, h) = (r.right - r.left, r.bottom - r.top);
                            let square = w > 0 && h > 0 && (w as f32 / h as f32 - 1.0).abs() < 0.25;
                            if square && (24..=256).contains(&w) && w > best {
                                best = w;
                                image = Some(k);
                            }
                        }
                    }
                }
            }
            if call_window {
                dbg(&format!("jendela panggilan?: teks={texts:?} tombol={actions:?}"));
            }
            // Jendela panggilan: hanya dianggap bila ada tombol Terima; tanpa itu (jendela chat biasa) abaikan.
            if call_window && !actions.iter().any(|a| ACCEPT_WORDS.iter().any(|w| a.to_lowercase().contains(w))) {
                return;
            }
            if texts.is_empty() {
                let name = bstr(toast.CurrentName());
                texts = name.split(['\r', '\n']).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            }
            let (app, title, body) = match texts.len() {
                0 => return,
                1 => (String::new(), texts[0].clone(), String::new()),
                2 => (String::new(), texts[0].clone(), texts[1].clone()),
                _ => (texts[0].clone(), texts[1].clone(), texts[2..].join("  ")),
            };
            let key = hash(&format!("{title}\u{1}{body}"));
            if self.seen.iter().any(|(k, _)| *k == key) {
                self.last = Some(toast.clone());
                return;
            }
            self.seen.push((key, Instant::now()));
            self.last = Some(toast.clone());

            // Ikon aplikasi dicari dari namanya di thread utama (apps.rs); di sini hanya logo listener + gambar banner.
            let icon = logo_for(&app);
            // Tunggu animasi geser selesai agar gambar tidak terpotong, lalu salin pikselnya.
            let image = image.and_then(|im| {
                std::thread::sleep(Duration::from_millis(450));
                capture(im.CurrentBoundingRectangle().ok()?)
            });
            dbg(&format!("banner: app='{app}' judul='{title}' logo_listener={} gambar={} teks={}", icon.is_some(), image.is_some(), texts.len()));
            let kind = if call_window || is_call(&app, &title, &body, &actions) { Kind::Call } else { Kind::Message };
            let t = Box::new(Toast { app, title, body, icon, image, actions, kind });
            let _ = PostThreadMessageW(self.main_tid, WM_NOTIFY_TOAST, WPARAM(0), LPARAM(Box::into_raw(t) as isize));
        }
    }

    /// Tekan tombol bernama `name` di dalam banner terakhir.
    fn invoke_action(&mut self, name: &str) {
        unsafe {
            let Some(el) = &self.last else { return };
            let Ok(cond) = self.uia.CreateTrueCondition() else { return };
            let Ok(all) = el.FindAll(TreeScope_Descendants, &cond) else { return };
            for i in 0..all.Length().unwrap_or(0).min(200) {
                let Ok(k) = all.GetElement(i) else { continue };
                if k.CurrentControlType().unwrap_or_default() != UIA_ButtonControlTypeId || bstr(k.CurrentName()).trim() != name {
                    continue;
                }
                if let Ok(ip) = k.GetCurrentPattern(UIA_InvokePatternId).and_then(|p| p.cast::<IUIAutomationInvokePattern>()) {
                    let _ = ip.Invoke();
                }
                return;
            }
        }
    }

    fn invoke(&mut self) {
        unsafe {
            if let Some(el) = &self.last {
                if let Ok(p) = el.GetCurrentPattern(UIA_InvokePatternId) {
                    if let Ok(ip) = p.cast::<IUIAutomationInvokePattern>() {
                        if ip.Invoke().is_ok() {
                            return;
                        }
                    }
                }
            }
            // Banner sudah hilang: buka Notification Center (Win+N di Windows 11).
            let mk = |vk: u16, up: bool| INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VIRTUAL_KEY(vk), wScan: 0, dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) }, time: 0, dwExtraInfo: 0 } },
            };
            let v = [mk(0x5B, false), mk(0x4E, false), mk(0x4E, true), mk(0x5B, true)];
            SendInput(&v, std::mem::size_of::<INPUT>() as i32);
        }
    }
}

/// Salin piksel layar pada `r` menjadi file BMP 32-bit (alpha dibuat opak).
unsafe fn capture(r: RECT) -> Option<Vec<u8>> {
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    if w <= 0 || h <= 0 || w > 512 || h > 512 {
        return None;
    }
    unsafe {
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(Some(screen));
        let mut bi: BITMAPINFO = std::mem::zeroed();
        bi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let out = match CreateDIBSection(Some(dc), &bi, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(bmp) => {
                let old = SelectObject(dc, bmp.into());
                let ok = BitBlt(dc, 0, 0, w, h, Some(screen), r.left, r.top, SRCCOPY).is_ok();
                let mut px = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize).to_vec();
                SelectObject(dc, old);
                let _ = DeleteObject(bmp.into());
                if ok {
                    px.chunks_exact_mut(4).for_each(|p| p[3] = 255);
                    let hdr = std::slice::from_raw_parts(&bi.bmiHeader as *const _ as *const u8, std::mem::size_of::<BITMAPINFOHEADER>());
                    let total = 14 + hdr.len() + px.len();
                    let mut f = Vec::with_capacity(total);
                    f.extend_from_slice(b"BM");
                    f.extend_from_slice(&(total as u32).to_le_bytes());
                    f.extend_from_slice(&[0, 0, 0, 0]);
                    f.extend_from_slice(&((14 + hdr.len()) as u32).to_le_bytes());
                    f.extend_from_slice(hdr);
                    f.extend_from_slice(&px);
                    Some(f)
                } else {
                    None
                }
            }
            Err(_) => None,
        };
        let _ = DeleteDC(dc);
        ReleaseDC(None, screen);
        out
    }
}

#[cfg(test)]
mod probe {
    /// Uji eksplorasi (manual): apakah UserNotificationListener boleh dipakai aplikasi desktop ini?
    #[test]
    #[ignore]
    fn listener_access_and_contents() {
        use windows::UI::Notifications::Management::*;
        use windows::UI::Notifications::*;
        use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
        unsafe {
            let _ = RoInitialize(RO_INIT_MULTITHREADED);
        }
        let l = UserNotificationListener::Current().expect("listener");
        println!("status awal: {:?}", l.GetAccessStatus());
        let r = l.RequestAccessAsync().and_then(|o| o.join());
        println!("RequestAccess: {:?}", r);
        if let Ok(UserNotificationListenerAccessStatus::Allowed) = r {
            let list = l.GetNotificationsAsync(NotificationKinds::Toast).and_then(|o| o.join()).expect("get");
            println!("jumlah notifikasi di pusat notifikasi: {}", list.Size().unwrap_or(0));
            for n in list {
                let app = n.AppInfo().and_then(|a| a.DisplayInfo()).and_then(|d| d.DisplayName()).map(|s| s.to_string()).unwrap_or_default();
                let texts: Vec<String> = n
                    .Notification()
                    .and_then(|x| x.Visual())
                    .and_then(|v| v.GetBinding(&KnownNotificationBindings::ToastGeneric().unwrap()))
                    .and_then(|b| b.GetTextElements())
                    .map(|t| t.into_iter().filter_map(|e| e.Text().ok().map(|s| s.to_string())).collect())
                    .unwrap_or_default();
                let logo = n.AppInfo().and_then(|a| a.DisplayInfo()).and_then(|d| d.GetLogo(windows::Foundation::Size { Width: 64.0, Height: 64.0 })).is_ok();
                println!("- app='{app}' logo={logo} teks={texts:?}");
            }
        }
    }
}
