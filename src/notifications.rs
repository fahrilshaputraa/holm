//! Notifikasi Windows di notch. Banner toast yang sedang tampil dibaca lewat UI Automation
//! (bukan API riwayat notifikasi, jadi tidak butuh izin khusus): aplikasi, judul, isi, dan gambar
//! pada banner (avatar/logo) disalin dari layar. Riwayat dan banner yang disenyapkan DND tidak terbaca.
//! Pemicu: event WindowOpened UIA + event jendela dari main (kick) + pemindaian cadangan berkala.
//! Teks hanya hidup di memori. Klik kartu = Invoke banner aslinya, atau Win+N bila banner sudah hilang.

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

#[derive(Clone)]
pub struct Toast {
    pub app: String,
    pub title: String,
    pub body: String,
    /// Gambar banner (BMP) bila ada: avatar/logo.
    pub icon: Option<Vec<u8>>,
}

#[derive(Default)]
struct Signal {
    scan: bool,
    invoke: bool,
}

static SIGNAL: Mutex<Signal> = Mutex::new(Signal { scan: false, invoke: false });
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

pub fn start(main_tid: u32, poll_ms: u32) {
    std::thread::spawn(move || unsafe { worker(main_tid, poll_ms) });
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
            {
                let Ok(mut g) = SIGNAL.lock() else { return };
                if !g.scan && !g.invoke {
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
                if matches!(class.as_str(), "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "Progman" | "WorkerW" | "HormBarClass" | "HormNotchClass" | "HormDockClass" | "HormPopupClass") {
                    continue;
                }
                let img = self.image(win.CurrentProcessId().unwrap_or(0) as u32);
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
                self.read_toast(&e, cond);
                return;
            }
        }
    }

    unsafe fn read_toast(&mut self, toast: &IUIAutomationElement, cond: &IUIAutomationCondition) {
        unsafe {
            let mut texts: Vec<String> = Vec::new();
            let mut image: Option<IUIAutomationElement> = None;
            let mut best = 0i32;
            if let Ok(kids) = toast.FindAll(TreeScope_Descendants, cond) {
                for i in 0..kids.Length().unwrap_or(0).min(200) {
                    let Ok(k) = kids.GetElement(i) else { continue };
                    let ct = k.CurrentControlType().unwrap_or_default();
                    if ct == UIA_TextControlTypeId {
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

            // Tunggu animasi geser selesai agar gambar tidak terpotong, lalu salin pikselnya.
            let icon = image.and_then(|im| {
                std::thread::sleep(Duration::from_millis(450));
                capture(im.CurrentBoundingRectangle().ok()?)
            });
            let t = Box::new(Toast { app, title, body, icon });
            let _ = PostThreadMessageW(self.main_tid, WM_NOTIFY_TOAST, WPARAM(0), LPARAM(Box::into_raw(t) as isize));
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
