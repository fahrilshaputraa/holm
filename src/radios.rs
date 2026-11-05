//! Saklar radio (Wi-Fi/Bluetooth) dan daftar perangkat Bluetooth terpasang, via WinRT.
//! Semua panggilan async dijalankan di thread latar; hasil dibaca dari state bersama dan
//! thread UI diberi tahu lewat status::post().

use std::sync::Mutex;
use windows::Devices::Bluetooth::{BluetoothConnectionStatus, BluetoothDevice};
use windows::Devices::Enumeration::DeviceInformation;
use windows::Devices::Radios::{Radio, RadioKind, RadioState};
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

#[derive(Clone, Copy, PartialEq)]
pub enum Which {
    Wifi,
    Bluetooth,
}

#[derive(Clone)]
pub struct BtDevice {
    pub name: String,
    pub connected: bool,
}

static WIFI_ON: Mutex<Option<bool>> = Mutex::new(None);
static BT_ON: Mutex<Option<bool>> = Mutex::new(None);
static BT_DEVICES: Mutex<Vec<BtDevice>> = Mutex::new(Vec::new());

/// None = belum diketahui / tidak ada radio / akses ditolak.
pub fn is_on(w: Which) -> Option<bool> {
    match w {
        Which::Wifi => *WIFI_ON.lock().ok()?,
        Which::Bluetooth => *BT_ON.lock().ok()?,
    }
}

pub fn bt_devices() -> Vec<BtDevice> {
    BT_DEVICES.lock().map(|d| d.clone()).unwrap_or_default()
}

fn kind_of(w: Which) -> RadioKind {
    match w {
        Which::Wifi => RadioKind::WiFi,
        Which::Bluetooth => RadioKind::Bluetooth,
    }
}

fn init() {
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
}

fn read_states() {
    let Ok(op) = Radio::GetRadiosAsync() else { return };
    let Ok(list) = op.join() else { return };
    let (mut wifi, mut bt) = (None, None);
    for r in list {
        let on = r.State().map(|s| s == RadioState::On).ok();
        match r.Kind() {
            Ok(RadioKind::WiFi) => wifi = wifi.or(on),
            Ok(RadioKind::Bluetooth) => bt = bt.or(on),
            _ => {}
        }
    }
    if let Ok(mut g) = WIFI_ON.lock() {
        *g = wifi;
    }
    if let Ok(mut g) = BT_ON.lock() {
        *g = bt;
    }
}

/// Baca status radio di thread latar.
pub fn refresh() {
    std::thread::spawn(|| {
        init();
        read_states();
        crate::status::post();
    });
}

/// Nyalakan/matikan radio. Butuh izin radio Windows; bila ditolak, state tidak berubah.
pub fn set(w: Which, on: bool) {
    std::thread::spawn(move || {
        init();
        if let Ok(Ok(_)) = Radio::RequestAccessAsync().map(|o| o.join()) {
            if let Ok(op) = Radio::GetRadiosAsync() {
                if let Ok(list) = op.join() {
                    for r in list {
                        if r.Kind().ok() == Some(kind_of(w)) {
                            if let Ok(o) = r.SetStateAsync(if on { RadioState::On } else { RadioState::Off }) {
                                let _ = o.join();
                            }
                        }
                    }
                }
            }
        }
        read_states();
        crate::status::post();
    });
}

/// Daftar perangkat Bluetooth yang sudah dipasangkan, beserta status sambungan.
pub fn refresh_bt_devices() {
    std::thread::spawn(|| {
        init();
        let mut out = Vec::new();
        if let Ok(sel) = BluetoothDevice::GetDeviceSelectorFromPairingState(true) {
            if let Ok(op) = DeviceInformation::FindAllAsyncAqsFilter(&sel) {
                if let Ok(devs) = op.join() {
                    for d in devs {
                        let (Ok(id), Ok(name)) = (d.Id(), d.Name()) else { continue };
                        let connected = BluetoothDevice::FromIdAsync(&id)
                            .ok()
                            .and_then(|o| o.join().ok())
                            .and_then(|b| b.ConnectionStatus().ok())
                            == Some(BluetoothConnectionStatus::Connected);
                        out.push(BtDevice { name: name.to_string(), connected });
                    }
                }
            }
        }
        out.sort_by(|a, b| b.connected.cmp(&a.connected).then(a.name.cmp(&b.name)));
        if let Ok(mut g) = BT_DEVICES.lock() {
            *g = out;
        }
        crate::status::post();
    });
}
// ---------- Pemantau sambungan Bluetooth (kartu "Connected" di notch) ----------

pub const WM_BT_CONNECTED: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 40;

const PROP_CONTAINER: &str = "System.Devices.Aep.ContainerId";
/// DEVPKEY_Bluetooth_Battery: persen baterai perangkat Bluetooth (pada node PnP-nya).
const PROP_BATTERY: &str = "{104EA319-6EE2-4701-BD47-8DDBF425BBE5} 2";

fn props(list: &[&str]) -> windows_collections::IIterable<windows::core::HSTRING> {
    windows_collections::IIterable::from(list.iter().map(|s| windows::core::HSTRING::from(*s)).collect::<Vec<_>>())
}

/// (id, nama, container) perangkat Bluetooth yang sedang tersambung (klasik + LE, digabung per nama).
fn connected() -> Vec<(String, String, Option<windows::core::GUID>)> {
    use windows::Devices::Bluetooth::BluetoothLEDevice;
    use windows::Foundation::IReference;
    use windows::core::Interface;
    let mut out: Vec<(String, String, Option<windows::core::GUID>)> = Vec::new();
    let sels = [
        BluetoothDevice::GetDeviceSelectorFromConnectionStatus(BluetoothConnectionStatus::Connected),
        BluetoothLEDevice::GetDeviceSelectorFromConnectionStatus(BluetoothConnectionStatus::Connected),
    ];
    for sel in sels.into_iter().flatten() {
        let Ok(Ok(list)) = DeviceInformation::FindAllAsyncAqsFilterAndAdditionalProperties(&sel, &props(&[PROP_CONTAINER])).map(|o| o.join()) else { continue };
        for d in list {
            let (Ok(id), Ok(name)) = (d.Id(), d.Name()) else { continue };
            let name = name.to_string();
            if name.is_empty() || out.iter().any(|(_, n, _)| *n == name) {
                continue;
            }
            let cid = d
                .Properties()
                .ok()
                .and_then(|p| p.Lookup(&windows::core::HSTRING::from(PROP_CONTAINER)).ok())
                .and_then(|v| v.cast::<IReference<windows::core::GUID>>().ok())
                .and_then(|r| r.Value().ok());
            out.push((id.to_string(), name, cid));
        }
    }
    out
}

fn battery_of(cid: windows::core::GUID) -> Option<u8> {
    use windows::Devices::Enumeration::DeviceInformationKind;
    use windows::Foundation::IReference;
    use windows::core::Interface;
    let aqs = windows::core::HSTRING::from(format!("System.Devices.ContainerId:=\"{{{cid:?}}}\""));
    let list = DeviceInformation::FindAllAsyncWithKindAqsFilterAndAdditionalProperties(&aqs, &props(&[PROP_BATTERY]), DeviceInformationKind::Device).ok()?.join().ok()?;
    for d in list {
        if let Some(v) = d.Properties().ok().and_then(|p| p.Lookup(&windows::core::HSTRING::from(PROP_BATTERY)).ok()) {
            if let Some(b) = v.cast::<IReference<u8>>().ok().and_then(|r| r.Value().ok()) {
                return Some(b.min(100));
            }
        }
    }
    None
}

fn is_audio(id: &str) -> bool {
    use windows::Devices::Bluetooth::BluetoothMajorClass;
    BluetoothDevice::FromIdAsync(&windows::core::HSTRING::from(id))
        .ok()
        .and_then(|o| o.join().ok())
        .and_then(|b| b.ClassOfDevice().ok())
        .and_then(|c| c.MajorClass().ok())
        == Some(BluetoothMajorClass::AudioVideo)
}

/// Thread latar: setiap ~3 dtk bandingkan perangkat tersambung; yang baru tersambung dikirim ke thread UI
/// sebagai `Box<Toast>` lewat WM_BT_CONNECTED.
pub fn watch(main_tid: u32) {
    use crate::notifications::{Kind, Toast};
    std::thread::spawn(move || {
        init();
        let mut known: Option<Vec<String>> = None;
        loop {
            let now = connected();
            if let Some(prev) = &known {
                for (id, name, cid) in &now {
                    if prev.contains(name) {
                        continue;
                    }
                    let mut t = Toast::message("Bluetooth", name, "");
                    let audio = is_audio(id) || {
                        let l = name.to_lowercase();
                        ["bud", "pods", "headphone", "headset", "earphone", "speaker", "wh-", "wf-", "jbl", "audio"].iter().any(|k| l.contains(k))
                    };
                    t.kind = Kind::Bluetooth { battery: cid.and_then(battery_of), audio };
                    let b = Box::into_raw(Box::new(t));
                    unsafe {
                        if windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW(main_tid, WM_BT_CONNECTED, windows::Win32::Foundation::WPARAM(0), windows::Win32::Foundation::LPARAM(b as isize)).is_err() {
                            drop(Box::from_raw(b));
                        }
                    }
                }
            }
            known = Some(now.into_iter().map(|(_, n, _)| n).collect());
            std::thread::sleep(std::time::Duration::from_secs(3));
        }
    });
}
