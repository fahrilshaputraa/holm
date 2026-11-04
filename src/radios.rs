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