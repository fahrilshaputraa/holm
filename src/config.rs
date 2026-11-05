use serde::Deserialize;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::SystemTime;

const DEFAULT_TOML: &str = include_str!("default_config.toml");

#[derive(Deserialize)]
pub struct WeatherCfg {
    /// Nama kota untuk cuaca (kosong = tidak ada data yang dikirim; animasi hujan jadi hiasan).
    pub city: String,
}

#[derive(Deserialize)]
pub struct Config {
    pub general: General,
    pub bar: Bar,
    pub notch: Notch,
    pub lyrics: LyricsCfg,
    pub dock: Dock,
    pub launcher: Launcher,
    pub notifications: Notifications,
    pub tray: Tray,
    pub control: Control,
    pub weather: WeatherCfg,
}

#[derive(Deserialize)]
pub struct Control {
    pub modules: Vec<String>,
}

#[derive(Deserialize)]
pub struct Tray {
    pub enabled: bool,
    pub inline: String,
    pub max_inline: i32,
    pub icon_size: i32,
    pub pinned: Vec<String>,
    pub hidden: Vec<String>,
}

#[derive(Deserialize)]
pub struct Notifications {
    pub enabled: bool,
    pub duration_ms: u32,
    pub show_body: bool,
    /// Monitor yang menampilkan kartu notch: "primary" (bawaan, privasi) atau "all".
    pub monitors: String,
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
    pub reclaim_space: bool,
    pub autohide: String,
    pub autohide_delay_ms: u32,
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
    pub style: String,
    pub background: String,
    pub accent: String,
    pub tint_from_artwork: bool,
    /// Animasi notch saat idle: "off" | "sky" | "plant" | "pulse" | "stars" | "wave".
    pub idle_animation: String,
    pub tabs: Vec<String>,
    pub default_tab: String,
    pub compact_left: Vec<String>,
    pub compact_right: Vec<String>,
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

/// Ganti/tambah satu kunci di dokumen TOML; komentar di belakang nilai lama dipertahankan.
pub fn set_key(doc: &mut toml_edit::DocumentMut, sec: &str, key: &str, v: toml_edit::Value) -> Result<(), String> {
    let table = doc.entry(sec).or_insert(toml_edit::table());
    let Some(table) = table.as_table_like_mut() else {
        return Err(format!("[{sec}] di config.toml bukan tabel."));
    };
    match table.get_mut(key).and_then(|i| i.as_value_mut()) {
        Some(old) => {
            let decor = old.decor().clone();
            *old = v;
            *old.decor_mut() = decor;
        }
        None => {
            table.insert(key, toml_edit::Item::Value(v));
        }
    }
    Ok(())
}

/// Tambah/hapus entri di `dock.pinned` pada config.toml (komentar & kunci lain tetap utuh).
/// `remove` memakai pencocok karena entri bisa memakai %VAR% atau huruf besar-kecil berbeda.
pub fn update_pinned(add: Option<&str>, remove: impl Fn(&str) -> bool) -> Result<(), String> {
    let text = std::fs::read_to_string(path()).unwrap_or_default();
    let mut doc = text.parse::<toml_edit::DocumentMut>().map_err(|e| e.to_string())?;
    let mut list: Vec<String> = doc.get("dock").and_then(|d| d.get("pinned")).and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_else(|| {
        // Belum ditulis pengguna: mulai dari nilai bawaan agar pin bawaan tidak hilang.
        load().map(|c| c.dock.pinned).unwrap_or_default()
    });
    list.retain(|e| !remove(e));
    if let Some(a) = add {
        if !list.iter().any(|e| e.eq_ignore_ascii_case(a)) {
            list.push(a.to_string());
        }
    }
    let arr: toml_edit::Array = list.iter().map(String::as_str).collect();
    set_key(&mut doc, "dock", "pinned", toml_edit::Value::Array(arr))?;
    let out = doc.to_string();
    validate(&out)?;
    let _ = std::fs::create_dir_all(dir());
    std::fs::write(path(), out).map_err(|e| e.to_string())
}

/// Migrasi teks config lama. None = tidak ada yang perlu diubah.
/// v2: ukuran notch bawaan lama (32 / 440 / 232) diganti ukuran baru yang lebih ringkas.
fn migrate_text(text: &str) -> Option<String> {
    let mut doc = text.parse::<toml_edit::DocumentMut>().ok()?;
    let ver = doc.get("general").and_then(|g| g.get("config_version")).and_then(|v| v.as_integer()).unwrap_or(1);
    if ver >= 7 {
        return None;
    }
    if ver < 2 {
        migrate_v2(&mut doc)?;
    }
    // v3: item "tray" ditambahkan di depan daftar kanan bar bila belum ada.
    if ver < 3 {
        if let Some(arr) = doc.get_mut("bar").and_then(|b| b.get_mut("right")).and_then(|r| r.as_array_mut()) {
            if !arr.iter().any(|v| v.as_str() == Some("tray")) {
                arr.insert(0, "tray");
                arr.fmt();
            }
        }
    }
    // v4: logo bawaan lama (glyph Apple U+F302) diganti logo Windows vektor.
    if doc.get("bar").and_then(|b| b.get("logo")).and_then(|v| v.as_str()) == Some("\u{f302}") {
        set_key(&mut doc, "bar", "logo", "windows".into()).ok()?;
    }
    // v5: font bar bawaan lama (varian Display untuk ukuran besar) diganti varian Text untuk teks kecil.
    if ver < 5 {
        let bar_str = |doc: &toml_edit::DocumentMut, k: &str| doc.get("bar").and_then(|b| b.get(k)).and_then(|v| v.as_str().map(str::to_string));
        if bar_str(&doc, "font").as_deref() == Some("Segoe UI Variable Display") {
            set_key(&mut doc, "bar", "font", "Segoe UI Variable Text".into()).ok()?;
        }
        if doc.get("bar").and_then(|b| b.get("font_size")).and_then(|v| v.as_float()) == Some(12.5) {
            set_key(&mut doc, "bar", "font_size", 13.0f64.into()).ok()?;
        }
    }
    // v6: label desktop bawaan lama "Finder" diganti nama brand HolmBar (nilai buatan pengguna tidak disentuh).
    if doc.get("bar").and_then(|b| b.get("desktop_label")).and_then(|v| v.as_str()) == Some("Finder") {
        set_key(&mut doc, "bar", "desktop_label", "HolmBar".into()).ok()?;
    }
    // v7: Dock tampil di semua monitor; notch terbuka lebih lebar (media + kalender + pintasan berdampingan).
    if ver < 7 {
        if doc.get("dock").and_then(|t| t.get("monitors")).and_then(|v| v.as_str()) == Some("primary") {
            set_key(&mut doc, "dock", "monitors", "all".into()).ok()?;
        }
        for (k, was, new) in [("expanded_width", 480i64, 640i64), ("expanded_height", 132, 150)] {
            if doc.get("notch").and_then(|t| t.get(k)).and_then(|v| v.as_integer()) == Some(was) {
                set_key(&mut doc, "notch", k, new.into()).ok()?;
            }
        }
    }
    set_key(&mut doc, "general", "config_version", 7i64.into()).ok()?;
    let out = doc.to_string();
    validate(&out).ok()?;
    Some(out)
}

/// v2: ukuran notch bawaan lama (32 / 440 / 232) diganti ukuran baru yang lebih ringkas.
fn migrate_v2(doc: &mut toml_edit::DocumentMut) -> Option<()> {
    let old: Vec<(&str, i64)> = [("compact_height", 32, 28), ("expanded_width", 440, 480), ("expanded_height", 232, 132)]
        .into_iter()
        .filter(|(k, was, _)| doc.get("notch").and_then(|t| t.get(k)).and_then(|v| v.as_integer()) == Some(*was))
        .map(|(k, _, new)| (k, new))
        .collect();
    for (k, new) in old {
        set_key(doc, "notch", k, new.into()).ok()?;
    }
    Some(())
}

fn migrate() {
    if let Some(out) = std::fs::read_to_string(path()).ok().as_deref().and_then(migrate_text) {
        let _ = std::fs::write(path(), out);
    }
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
    migrate();
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

    #[test]
    fn migrates_old_notch_sizes_only_when_default() {
        let old = "[general]
language = \"id\"

[notch]
compact_height = 32   # tinggi
expanded_width = 500
expanded_height = 232
";
        let out = migrate_text(old).expect("perlu migrasi");
        assert!(out.contains("compact_height = 28   # tinggi"), "{out}");
        assert!(out.contains("expanded_width = 500"), "nilai buatan pengguna tidak disentuh: {out}");
        assert!(out.contains("expanded_height = 150"), "bawaan lama naik ke bawaan terbaru: {out}");
        assert!(out.contains("config_version = 7"));
        assert!(migrate_text(&out).is_none(), "migrasi hanya sekali");
    }

    #[test]
    fn migrates_dock_to_all_monitors() {
        let old = "[general]
config_version = 6

[dock]
monitors = \"primary\"   # m

[notch]
expanded_width = 520
";
        let out = migrate_text(old).expect("perlu migrasi");
        assert!(out.contains("monitors = \"all\"   # m"), "{out}");
        assert!(out.contains("expanded_width = 520"), "nilai buatan pengguna tidak disentuh: {out}");
    }

    #[test]
    fn update_pinned_adds_and_removes() {
        let dir = std::env::temp_dir().join(format!("holm-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("config.toml");
        std::fs::write(&file, "[dock]\npinned = [\"C:\\\\a.exe\"]   # pin\n").unwrap();
        // update_pinned memakai path() dari APPDATA; uji lewat penyunting dokumen yang sama.
        let mut doc: toml_edit::DocumentMut = std::fs::read_to_string(&file).unwrap().parse().unwrap();
        let mut list: Vec<String> = doc["dock"]["pinned"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect();
        list.retain(|e| !e.eq_ignore_ascii_case("C:\\a.exe"));
        list.push("aumid:foo!app".into());
        let arr: toml_edit::Array = list.iter().map(String::as_str).collect();
        set_key(&mut doc, "dock", "pinned", toml_edit::Value::Array(arr)).unwrap();
        let out = doc.to_string();
        assert!(out.contains("aumid:foo!app") && !out.contains("a.exe") && out.contains("# pin"), "{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn migrates_finder_label_to_brand() {
        let old = "[general]
config_version = 5

[bar]
desktop_label = \"Finder\"  # label
";
        let out = migrate_text(old).expect("perlu migrasi");
        assert!(out.contains("desktop_label = \"HolmBar\"  # label"), "{out}");
        let mine = "[general]
config_version = 5

[bar]
desktop_label = \"Desktop\"
";
        assert!(migrate_text(mine).unwrap().contains("desktop_label = \"Desktop\""));
    }

    #[test]
    fn migrates_display_font_to_text() {
        let old = "[general]\nconfig_version = 4\n\n[bar]\nfont = \"Segoe UI Variable Display\"\nfont_size = 12.5\n";
        let out = migrate_text(old).expect("perlu migrasi");
        assert!(out.contains("font = \"Segoe UI Variable Text\""), "{out}");
        assert!(out.contains("font_size = 13.0"), "{out}");
        let mine = "[general]\nconfig_version = 4\n\n[bar]\nfont = \"Inter\"\n";
        assert!(migrate_text(mine).unwrap().contains("font = \"Inter\""));
    }

    #[test]
    fn migrates_apple_logo_to_windows() {
        let old = "[general]\nconfig_version = 3\n\n[bar]\nlogo = \"\u{f302}\"   # logo\n";
        let out = migrate_text(old).expect("perlu migrasi");
        assert!(out.contains("logo = \"windows\"   # logo"), "{out}");
        let custom = "[general]\nconfig_version = 3\n\n[bar]\nlogo = \"X\"\n";
        assert!(migrate_text(custom).unwrap().contains("logo = \"X\""), "logo buatan pengguna tidak disentuh");
    }

    #[test]
    fn migrates_tray_into_bar_right() {
        let old = "[general]\nconfig_version = 2\n\n[bar]\nright = [\"wifi\", \"clock\"]  # kanan\n";
        let out = migrate_text(old).expect("perlu migrasi");
        assert!(out.contains("right = [\"tray\", \"wifi\", \"clock\"]  # kanan"), "{out}");
        assert!(migrate_text(&out).is_none());
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
