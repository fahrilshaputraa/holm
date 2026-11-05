//! Backend Control Center: kecerahan layar internal, mode pesawat, dan Jangan Ganggu.
//!
//! - Kecerahan: WMI `root\wmi` lewat PowerShell tersembunyi (hanya ada di layar internal laptop;
//!   monitor eksternal/PC desktop = tidak ada slider). Pemanggilan dikumpulkan: hanya nilai terbaru
//!   yang dikirim, satu proses pada satu waktu.
//! - Mode pesawat: mematikan/menyalakan radio Wi-Fi dan Bluetooth (lihat `radios`).
//! - Jangan Ganggu: `HKCU\...\PushNotifications\ToastEnabled` (sama dengan sakelar
//!   "Dapatkan notifikasi dari aplikasi" di Pengaturan Windows). Nilai ini tersimpan di Windows,
//!   bukan di HolmBar, jadi tetap berlaku setelah HolmBar ditutup.

use crate::radios::{self, Which};
use std::os::windows::process::CommandExt;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Mutex;
use windows::core::{w, PCWSTR};
use windows::Win32::System::Registry::*;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

static BRIGHTNESS: Mutex<Option<u32>> = Mutex::new(None);
static TARGET: AtomicI32 = AtomicI32::new(-1);
static RUNNING: AtomicBool = AtomicBool::new(false);

fn powershell(script: &str) -> Option<String> {
    let out = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", script])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Kecerahan terakhir yang diketahui (0..100); None = layar ini tidak mendukung.
pub fn brightness() -> Option<u32> {
    BRIGHTNESS.lock().ok().and_then(|b| *b)
}

/// Baca kecerahan di thread latar lalu beri tahu UI.
pub fn refresh_brightness() {
    std::thread::spawn(|| {
        let v = powershell("(Get-CimInstance -Namespace root/wmi -ClassName WmiMonitorBrightness -ErrorAction SilentlyContinue | Select-Object -First 1).CurrentBrightness").and_then(|s| s.lines().next().and_then(|l| l.trim().parse::<u32>().ok()));
        if let Ok(mut g) = BRIGHTNESS.lock() {
            // Jangan menimpa nilai yang sedang digeser pengguna.
            if TARGET.load(Ordering::SeqCst) < 0 {
                *g = v.map(|x| x.min(100));
            }
        }
        crate::status::post();
    });
}

/// Atur kecerahan (0..100). Tampilan langsung berubah; perintah ke Windows dikumpulkan di thread latar.
pub fn set_brightness(v: u32) {
    let v = v.min(100);
    if let Ok(mut g) = BRIGHTNESS.lock() {
        *g = Some(v);
    }
    TARGET.store(v as i32, Ordering::SeqCst);
    if RUNNING.swap(true, Ordering::SeqCst) {
        return; // thread yang berjalan akan mengambil target terbaru
    }
    std::thread::spawn(|| {
        loop {
            let t = TARGET.swap(-1, Ordering::SeqCst);
            if t < 0 {
                break;
            }
            let _ = powershell(&format!("Get-CimInstance -Namespace root/wmi -ClassName WmiMonitorBrightnessMethods | Invoke-CimMethod -MethodName WmiSetBrightness -Arguments @{{Timeout=1;Brightness={t}}} | Out-Null"));
        }
        RUNNING.store(false, Ordering::SeqCst);
    });
}

/// Mode pesawat dianggap aktif bila semua radio yang ada (Wi-Fi/Bluetooth) mati.
pub fn airplane() -> bool {
    let (wifi, bt) = (radios::is_on(Which::Wifi), radios::is_on(Which::Bluetooth));
    (wifi.is_some() || bt.is_some()) && wifi != Some(true) && bt != Some(true)
}

pub fn set_airplane(on: bool) {
    radios::set(Which::Wifi, !on);
    radios::set(Which::Bluetooth, !on);
}

const PUSH_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\PushNotifications");

/// Jangan Ganggu aktif = banner notifikasi aplikasi dimatikan di Windows.
pub fn dnd() -> bool {
    unsafe {
        let (mut v, mut size) = (1u32, 4u32);
        let r = RegGetValueW(HKEY_CURRENT_USER, PUSH_KEY, w!("ToastEnabled"), RRF_RT_REG_DWORD, None, Some(&mut v as *mut u32 as *mut _), Some(&mut size));
        r.is_ok() && v == 0
    }
}

pub fn set_dnd(on: bool) {
    unsafe {
        let mut key = HKEY::default();
        if RegCreateKeyExW(HKEY_CURRENT_USER, PUSH_KEY, None, PCWSTR::null(), REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, None, &mut key, None).is_err() {
            return;
        }
        let v: u32 = if on { 0 } else { 1 };
        let _ = RegSetValueExW(key, w!("ToastEnabled"), None, REG_DWORD, Some(&v.to_le_bytes()));
        let _ = RegCloseKey(key);
    }
    crate::status::post();
}
