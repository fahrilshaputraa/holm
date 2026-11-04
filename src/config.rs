use serde::Deserialize;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::SystemTime;

const DEFAULT_TOML: &str = include_str!("default_config.toml");

#[derive(Deserialize)]
pub struct Config {
    pub general: General,
    pub bar: Bar,
    pub notch: Notch,
    pub lyrics: LyricsCfg,
    pub dock: Dock,
    pub launcher: Launcher,
    pub notifications: Notifications,
}

#[derive(Deserialize)]
pub struct Notifications {
    pub enabled: bool,
    pub duration_ms: u32,
    pub show_body: bool,
    pub poll_ms: u32,
}

#[derive(Deserialize)]
pub struct Launcher {
    pub hotkey: String,
}

#[derive(Deserialize)]
pub struct Dock {
    pub enabled: bool,
    pub monitors: String,
    pub icon_size: i32,
    pub magnification: f32,
    pub background: String,
    pub opacity: f32,
    pub reserve_space: bool,
    pub replace_taskbar: bool,
    pub pinned: Vec<String>,
    pub start_button: bool,
    pub import_taskbar_pins: bool,
}

#[derive(Deserialize)]
pub struct LyricsCfg {
    pub enabled: bool,
}

#[derive(Deserialize)]
pub struct General {
    pub language: String,
    pub accent: String,
}

#[derive(Deserialize)]
pub struct Bar {
    pub enabled: bool,
    pub monitors: String,
    pub height: i32,
    pub background: String,
    pub opacity: f32,
    pub text: String,
    pub dim_text: String,
    pub font: String,
    pub icon_font: String,
    pub font_size: f32,
    pub logo: String,
    pub desktop_label: String,
    pub menus: Vec<String>,
    pub right: Vec<String>,
    pub clock_format: String,
}

#[derive(Deserialize)]
pub struct Notch {
    pub enabled: bool,
    pub monitors: String,
    pub background: String,
    pub accent: String,
    pub compact_width: i32,
    pub compact_height: i32,
    pub expanded_width: i32,
    pub expanded_height: i32,
    pub expand_on_hover: bool,
    pub close_delay_ms: u32,
    pub animation_ms: u32,
}

thread_local! {
    static CURRENT: RefCell<Option<Rc<Config>>> = const { RefCell::new(None) };
}

pub fn current() -> Rc<Config> {
    CURRENT.with(|c| c.borrow().clone().expect("config not initialised"))
}

pub fn dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("HolmBar")
}

pub fn path() -> PathBuf {
    dir().join("config.toml")
}

pub fn mtime() -> Option<SystemTime> {
    std::fs::metadata(path()).and_then(|m| m.modified()).ok()
}

/// Buat file konfigurasi bawaan jika belum ada.
pub fn ensure_file() {
    let p = path();
    if !p.exists() {
        let _ = std::fs::create_dir_all(dir());
        // Pindahan dari nama lama (%APPDATA%\HormBar) bila ada.
        let old = dir().with_file_name("HormBar").join("config.toml");
        if std::fs::copy(&old, &p).is_err() {
            let _ = std::fs::write(&p, DEFAULT_TOML);
        }
    }
}

fn merge(base: &mut toml::Value, over: toml::Value) {
    match (base, over) {
        (toml::Value::Table(b), toml::Value::Table(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(bv) => merge(bv, v),
                    None => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (b, o) => *b = o,
    }
}

/// Nilai bawaan digabung dengan teks config pengguna (bila ada).
fn merged_from(user_text: Option<&str>) -> Result<toml::Value, String> {
    let mut merged: toml::Value = toml::from_str(DEFAULT_TOML).map_err(|e| e.to_string())?;
    if let Some(text) = user_text {
        let user: toml::Value = toml::from_str(text).map_err(|e| e.to_string())?;
        merge(&mut merged, user);
    }
    Ok(merged)
}

/// Nilai efektif (bawaan + file pengguna) sebagai pohon TOML, untuk dialog pengaturan.
pub fn merged_value() -> Result<toml::Value, String> {
    merged_from(std::fs::read_to_string(path()).ok().as_deref())
}

/// Pastikan teks config pengguna valid sebelum ditulis ke disk.
pub fn validate(user_text: &str) -> Result<(), String> {
    merged_from(Some(user_text))?.try_into::<Config>().map(|_| ()).map_err(|e| e.to_string())
}

/// Nilai bawaan + file pengguna (digabung per-kunci). Error -> pesan untuk ditampilkan.
pub fn load() -> Result<Config, String> {
    merged_value()?.try_into::<Config>().map_err(|e| e.to_string())
}

/// Muat ulang; jika gagal, konfigurasi lama dipertahankan dan error dikembalikan.
pub fn reload() -> Result<(), String> {
    let cfg = load()?;
    CURRENT.with(|c| *c.borrow_mut() = Some(Rc::new(cfg)));
    Ok(())
}

pub fn init() {
    ensure_file();
    let cfg = load().or_else(|_| {
        let v: toml::Value = toml::from_str(DEFAULT_TOML).unwrap();
        v.try_into::<Config>().map_err(|e| e.to_string())
    });
    CURRENT.with(|c| *c.borrow_mut() = Some(Rc::new(cfg.expect("default config is valid"))));
}

/// "#RRGGBB" atau "#AARRGGBB" -> 0xAARRGGBB.
pub fn color(s: &str) -> u32 {
    let h = s.trim().trim_start_matches('#');
    match (h.len(), u32::from_str_radix(h, 16)) {
        (6, Ok(v)) => 0xFF00_0000 | v,
        (8, Ok(v)) => v,
        _ => 0xFFFF_00FF,
    }
}

pub fn with_alpha(c: u32, a: f32) -> u32 {
    let a = (a.clamp(0.0, 1.0) * 255.0).round() as u32;
    (c & 0x00FF_FFFF) | (a << 24)
}

/// "ctrl+alt+space" -> (modifier Win32, virtual key). None bila tidak valid / kosong.
pub fn parse_hotkey(s: &str) -> Option<(u32, u32)> {
    let (mut mods, mut key) = (0u32, None);
    for part in s.to_lowercase().split('+').map(str::trim).filter(|p| !p.is_empty()) {
        match part {
            "alt" => mods |= 0x1,
            "ctrl" | "control" => mods |= 0x2,
            "shift" => mods |= 0x4,
            "win" | "super" => mods |= 0x8,
            "space" => key = Some(0x20),
            p if p.len() == 1 && p.as_bytes()[0].is_ascii_alphanumeric() => key = Some(p.to_ascii_uppercase().as_bytes()[0] as u32),
            p if p.starts_with('f') && p[1..].parse::<u32>().map(|n| (1..=12).contains(&n)).unwrap_or(false) => key = Some(0x70 + p[1..].parse::<u32>().unwrap() - 1),
            _ => return None,
        }
    }
    if mods == 0 {
        return None; // wajib ada modifier agar tidak menangkap tombol biasa
    }
    key.map(|k| (mods, k))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let v: toml::Value = toml::from_str(DEFAULT_TOML).expect("default toml parses");
        v.try_into::<Config>().expect("default deserializes into Config");
    }

    /// Menampilkan galat parse config pengguna yang sebenarnya (jika ada).
    #[test]
    fn user_config_loads() {
        match load() {
            Ok(c) => println!("user config OK; dock.enabled = {}", c.dock.enabled),
            Err(e) => panic!("config pengguna gagal dimuat: {e}"),
        }
    }
}
