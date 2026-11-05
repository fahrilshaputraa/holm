//! Status sistem nyata untuk bar: volume (Core Audio) dan Wi-Fi (WLAN API).
//! Keduanya berbasis event; thread/callback hanya memposting WM_STATUS_CHANGED ke bar.

use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::sync::Mutex;
use windows_core::implement;
use windows::Win32::Foundation::*;
use windows::Win32::Media::Audio::Endpoints::*;
use windows::Win32::Media::Audio::*;
use windows::Win32::NetworkManagement::WiFi::*;
use windows::Win32::System::Com::*;
use windows::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_APP};

pub const WM_STATUS_CHANGED: u32 = WM_APP + 3;

static NOTIFY: AtomicU32 = AtomicU32::new(0);
static WLAN_HANDLE: AtomicIsize = AtomicIsize::new(0);
static VOLUME: Mutex<Option<Volume>> = Mutex::new(None);

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Volume {
    pub level: f32, // 0.0..1.0
    pub muted: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Wifi {
    /// Tidak ada adapter Wi-Fi (mis. PC desktop): ikon disembunyikan.
    Absent,
    Disconnected,
    Connected { quality: u32 },
}

pub fn volume() -> Option<Volume> {
    VOLUME.lock().ok().and_then(|v| *v)
}

pub fn post() {
    let t = NOTIFY.load(Ordering::Relaxed);
    if t != 0 {
        unsafe {
            let _ = PostThreadMessageW(t, WM_STATUS_CHANGED, WPARAM(0), LPARAM(0));
        }
    }
}

fn set_volume(v: Volume) {
    if let Ok(mut g) = VOLUME.lock() {
        if *g == Some(v) {
            return;
        }
        *g = Some(v);
    }
    post();
}

#[implement(IAudioEndpointVolumeCallback)]
struct VolumeCallback;

impl IAudioEndpointVolumeCallback_Impl for VolumeCallback_Impl {
    fn OnNotify(&self, pnotify: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> windows::core::Result<()> {
        if let Some(n) = unsafe { pnotify.as_ref() } {
            set_volume(Volume { level: n.fMasterVolume, muted: n.bMuted.as_bool() });
        }
        Ok(())
    }
}

/// Endpoint volume dibagi ke thread UI untuk mengatur volume (objek COM MTA aman lintas thread).
struct SharedEndpoint(IAudioEndpointVolume);
unsafe impl Send for SharedEndpoint {}
static ENDPOINT: Mutex<Option<SharedEndpoint>> = Mutex::new(None);

/// Atur volume utama 0.0..1.0 (mengubah mute menjadi off bila volume dinaikkan dari 0).
pub fn change_volume(level: f32) {
    if let Ok(g) = ENDPOINT.lock() {
        if let Some(e) = g.as_ref() {
            unsafe {
                let _ = e.0.SetMasterVolumeLevelScalar(level.clamp(0.0, 1.0), std::ptr::null());
                if level > 0.0 && e.0.GetMute().map(|m| m.as_bool()).unwrap_or(false) {
                    let _ = e.0.SetMute(false, std::ptr::null());
                }
            }
        }
    }
}

pub fn toggle_mute() {
    if let Ok(g) = ENDPOINT.lock() {
        if let Some(e) = g.as_ref() {
            unsafe {
                let m = e.0.GetMute().map(|m| m.as_bool()).unwrap_or(false);
                let _ = e.0.SetMute(!m, std::ptr::null());
            }
        }
    }
}

fn start_volume() {
    std::thread::spawn(|| unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let Ok(enumerator) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) else { return };
        let Ok(device) = enumerator.GetDefaultAudioEndpoint(eRender, eConsole) else { return };
        let Ok(endpoint) = device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) else { return };
        if let Ok(mut g) = ENDPOINT.lock() {
            *g = Some(SharedEndpoint(endpoint.clone()));
        }

        if let (Ok(level), Ok(muted)) = (endpoint.GetMasterVolumeLevelScalar(), endpoint.GetMute()) {
            set_volume(Volume { level, muted: muted.as_bool() });
        }
        let cb: IAudioEndpointVolumeCallback = VolumeCallback.into();
        if endpoint.RegisterControlChangeNotify(&cb).is_err() {
            return;
        }
        // Callback datang dari thread audio; thread ini menjaga endpoint & callback tetap hidup.
        loop {
            std::thread::park();
        }
    });
}

unsafe extern "system" fn wlan_callback(_data: *mut L2_NOTIFICATION_DATA, _ctx: *mut std::ffi::c_void) {
    post();
}

fn start_wifi() {
    unsafe {
        let mut negotiated = 0u32;
        let mut handle = HANDLE::default();
        if WlanOpenHandle(2, None, &mut negotiated, &mut handle) != 0 {
            return; // layanan WLAN tidak tersedia
        }
        WLAN_HANDLE.store(handle.0 as isize, Ordering::Relaxed);
        WlanRegisterNotification(
            handle,
            WLAN_NOTIFICATION_SOURCE_ACM | WLAN_NOTIFICATION_SOURCE_MSM,
            true,
            Some(wlan_callback),
            None,
            None,
            None,
        );
    }
}

/// Baca status Wi-Fi saat ini (cepat, lokal).
pub fn wifi() -> Wifi {
    let h = WLAN_HANDLE.load(Ordering::Relaxed);
    if h == 0 {
        return Wifi::Absent;
    }
    let handle = HANDLE(h as *mut _);
    unsafe {
        let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
        if WlanEnumInterfaces(handle, None, &mut list) != 0 || list.is_null() {
            return Wifi::Absent;
        }
        let n = (*list).dwNumberOfItems as usize;
        let items = std::slice::from_raw_parts((*list).InterfaceInfo.as_ptr(), n);
        let mut result = if n == 0 { Wifi::Absent } else { Wifi::Disconnected };
        for it in items {
            if it.isState != wlan_interface_state_connected {
                continue;
            }
            let mut size = 0u32;
            let mut data: *mut std::ffi::c_void = std::ptr::null_mut();
            let rc = WlanQueryInterface(handle, &it.InterfaceGuid, wlan_intf_opcode_current_connection, None, &mut size, &mut data, None);
            if rc == 0 && !data.is_null() {
                let attrs = &*(data as *const WLAN_CONNECTION_ATTRIBUTES);
                result = Wifi::Connected { quality: attrs.wlanAssociationAttributes.wlanSignalQuality };
                WlanFreeMemory(data);
                break;
            }
        }
        WlanFreeMemory(list as *const _);
        result
    }
}

pub fn start(thread_id: u32) {
    NOTIFY.store(thread_id, Ordering::Relaxed);
    start_volume();
    start_wifi();
}



// ---------- Jaringan Wi-Fi (daftar, sambung, putus) ----------

#[derive(Clone, Debug)]
pub struct Net {
    pub ssid: String,
    pub profile: String,
    pub quality: u32,
    pub secured: bool,
    pub connected: bool,
    pub has_profile: bool,
    /// 1 = terbuka, 7 = WPA2-PSK, 9 = WPA3-SAE, lainnya = tidak didukung dari sini.
    pub auth: i32,
}

fn first_iface() -> Option<(HANDLE, windows::core::GUID)> {
    let h = WLAN_HANDLE.load(Ordering::Relaxed);
    if h == 0 {
        return None;
    }
    let handle = HANDLE(h as *mut _);
    unsafe {
        let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
        if WlanEnumInterfaces(handle, None, &mut list) != 0 || list.is_null() {
            return None;
        }
        let r = if (*list).dwNumberOfItems > 0 { Some((handle, (*list).InterfaceInfo[0].InterfaceGuid)) } else { None };
        WlanFreeMemory(list as *const _);
        r
    }
}

/// Minta adapter memindai ulang (hasil muncul beberapa detik kemudian).
pub fn wifi_scan() {
    if let Some((h, g)) = first_iface() {
        unsafe {
            WlanScan(h, &g, None, None, None);
        }
    }
}

/// Jaringan terlihat: terhubung dulu, lalu sinyal terkuat; SSID ganda digabung.
pub fn networks() -> Vec<Net> {
    let Some((h, g)) = first_iface() else { return vec![] };
    let mut out: Vec<Net> = Vec::new();
    unsafe {
        let mut list: *mut WLAN_AVAILABLE_NETWORK_LIST = std::ptr::null_mut();
        if WlanGetAvailableNetworkList(h, &g, 0, None, &mut list) != 0 || list.is_null() {
            return out;
        }
        let items = std::slice::from_raw_parts((*list).Network.as_ptr(), (*list).dwNumberOfItems as usize);
        for n in items {
            let len = (n.dot11Ssid.uSSIDLength as usize).min(32);
            let ssid = String::from_utf8_lossy(&n.dot11Ssid.ucSSID[..len]).to_string();
            if ssid.is_empty() {
                continue;
            }
            let plen = n.strProfileName.iter().position(|&c| c == 0).unwrap_or(256);
            let net = Net {
                profile: String::from_utf16_lossy(&n.strProfileName[..plen]),
                quality: n.wlanSignalQuality,
                secured: n.bSecurityEnabled.as_bool(),
                connected: n.dwFlags & WLAN_AVAILABLE_NETWORK_CONNECTED != 0,
                has_profile: n.dwFlags & WLAN_AVAILABLE_NETWORK_HAS_PROFILE != 0,
                auth: n.dot11DefaultAuthAlgorithm.0,
                ssid,
            };
            match out.iter_mut().find(|x| x.ssid == net.ssid) {
                Some(x) => {
                    if net.connected || (!x.connected && net.quality > x.quality) {
                        *x = net;
                    }
                }
                None => out.push(net),
            }
        }
        WlanFreeMemory(list as *const _);
    }
    out.sort_by(|a, b| b.connected.cmp(&a.connected).then(b.quality.cmp(&a.quality)));
    out
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

fn connect_profile(profile: &str) -> bool {
    let Some((h, g)) = first_iface() else { return false };
    let p: Vec<u16> = profile.encode_utf16().chain(std::iter::once(0)).collect();
    let params = WLAN_CONNECTION_PARAMETERS {
        wlanConnectionMode: wlan_connection_mode_profile,
        strProfile: windows::core::PCWSTR(p.as_ptr()),
        pDot11Ssid: std::ptr::null_mut(),
        pDesiredBssidList: std::ptr::null_mut(),
        dot11BssType: dot11_BSS_type_infrastructure,
        dwFlags: 0,
    };
    unsafe { WlanConnect(h, &g, &params, None) == 0 }
}

/// Sambungkan `net`: pakai profil tersimpan bila ada; jika tidak, buat profil
/// (jaringan terbuka, WPA2-PSK, atau WPA3-SAE dengan `password`). Mengembalikan false bila gagal.
pub fn connect(net: &Net, password: Option<&str>) -> bool {
    if net.has_profile && !net.profile.is_empty() {
        return connect_profile(&net.profile);
    }
    let Some((h, g)) = first_iface() else { return false };
    let (auth, enc, key) = match (net.auth, password) {
        (1, _) if !net.secured => ("open", "none", String::new()),
        (7, Some(p)) => ("WPA2PSK", "AES", p.to_string()),
        (9, Some(p)) => ("WPA3SAE", "AES", p.to_string()),
        _ => return false,
    };
    let key_xml = if key.is_empty() {
        String::new()
    } else {
        format!("<sharedKey><keyType>passphrase</keyType><protected>false</protected><keyMaterial>{}</keyMaterial></sharedKey>", xml_escape(&key))
    };
    let name = xml_escape(&net.ssid);
    let xml = format!(
        "<?xml version=\"1.0\"?><WLANProfile xmlns=\"http://www.microsoft.com/networking/WLAN/profile/v1\"><name>{name}</name><SSIDConfig><SSID><name>{name}</name></SSID></SSIDConfig><connectionType>ESS</connectionType><connectionMode>auto</connectionMode><MSM><security><authEncryption><authentication>{auth}</authentication><encryption>{enc}</encryption><useOneX>false</useOneX></authEncryption>{key_xml}</security></MSM></WLANProfile>"
    );
    let x: Vec<u16> = xml.encode_utf16().chain(std::iter::once(0)).collect();
    let mut reason = 0u32;
    unsafe {
        let rc = WlanSetProfile(h, &g, 0, windows::core::PCWSTR(x.as_ptr()), windows::core::PCWSTR::null(), true, None, &mut reason);
        if rc != 0 {
            return false;
        }
    }
    connect_profile(&net.ssid)
}

pub fn wifi_disconnect() {
    if let Some((h, g)) = first_iface() {
        unsafe {
            WlanDisconnect(h, &g, None);
        }
    }
}
/// Hapus profil Wi-Fi bernama `name` (dipakai untuk membuang profil buatan kita yang gagal tersambung).
pub fn delete_profile(name: &str) {
    if let Some((h, g)) = first_iface() {
        let n: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            WlanDeleteProfile(h, &g, windows::core::PCWSTR(n.as_ptr()), None);
        }
    }
}
thread_local! {
    /// Meter puncak suara perangkat output default (dibuat lazim di thread pemanggil, diperbarui tiap ~10 dtk
    /// agar ikut berganti saat perangkat output berganti).
    static METER: std::cell::RefCell<Option<(std::time::Instant, IAudioMeterInformation)>> = const { std::cell::RefCell::new(None) };
}

/// Puncak suara yang sedang diputar komputer, 0.0..1.0 (0 bila tidak ada/gagal).
pub fn audio_peak() -> f32 {
    METER.with(|m| {
        let mut m = m.borrow_mut();
        if m.as_ref().map(|(t, _)| t.elapsed().as_secs() >= 10).unwrap_or(true) {
            *m = unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .and_then(|e| e.GetDefaultAudioEndpoint(eRender, eConsole))
                    .and_then(|d| d.Activate::<IAudioMeterInformation>(CLSCTX_ALL, None))
                    .ok()
                    .map(|meter| (std::time::Instant::now(), meter))
            };
        }
        m.as_ref().and_then(|(_, meter)| unsafe { meter.GetPeakValue() }.ok()).unwrap_or(0.0).clamp(0.0, 1.0)
    })
}
