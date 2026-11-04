//! Jendela Pengaturan bergaya System Settings macOS (gelap, sidebar berikon, kartu berkelompok).
//! Digambar sendiri dengan `Canvas` (GDI+ di jendela layered), sama seperti popup bar.
//!
//! Perubahan diterapkan otomatis: setiap kontrol mengubah salinan nilai, lalu setelah jeda singkat
//! hanya kunci yang berbeda dari file yang ditulis ke config.toml lewat `toml_edit` (komentar &
//! kunci lain tetap utuh) dan konfigurasi dimuat ulang. Opsi daftar (menu, item kanan, aplikasi
//! tersemat) tetap diatur lewat "Buka config.toml".

use crate::canvas::{Align, Canvas, Font};
use crate::config;
use std::cell::RefCell;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
use windows::Win32::UI::Controls::Dialogs::*;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const CLASS: PCWSTR = w!("HolmSettingsClass");
const TIMER_SAVE: usize = 1;
const SAVE_DELAY_MS: u32 = 450;

// ---------- Deskripsi pengaturan ----------

#[derive(Clone, Copy)]
enum Kind {
    Toggle,
    /// (nilai di config, teks yang ditampilkan)
    Choice(&'static [(&'static str, &'static str)]),
    /// unit: "px", "ms", "s" (ms ditampilkan dalam detik), "%" (0–1), "x" (kali), "" (angka)
    Slider { min: f64, max: f64, step: f64, int: bool, unit: &'static str },
    Color,
    Text,
    Hotkey,
}

struct Field {
    sec: &'static str,
    key: &'static str,
    label: &'static str,
    hint: &'static str,
    kind: Kind,
}

const fn f(sec: &'static str, key: &'static str, label: &'static str, hint: &'static str, kind: Kind) -> Field {
    Field { sec, key, label, hint, kind }
}

const fn int(min: f64, max: f64, step: f64, unit: &'static str) -> Kind {
    Kind::Slider { min, max, step, int: true, unit }
}

const fn num(min: f64, max: f64, step: f64, unit: &'static str) -> Kind {
    Kind::Slider { min, max, step, int: false, unit }
}

const MONITORS: Kind = Kind::Choice(&[("all", "Semua"), ("primary", "Utama")]);

struct Group {
    title: &'static str,
    fields: &'static [Field],
}

struct Section {
    title: &'static str,
    icon: &'static str,
    tint: u32,
    groups: &'static [Group],
    footer: &'static str,
}

const SECTIONS: &[Section] = &[
    Section {
        title: "Umum",
        icon: "\u{E713}",
        tint: 0xFF8E8E93,
        groups: &[
            Group {
                title: "Tampilan",
                fields: &[
                    f("general", "language", "Bahasa jam & kalender", "", Kind::Choice(&[("id", "Indonesia"), ("en", "English")])),
                    f("general", "accent", "Warna aksen", "Sorotan popup, slider, dan pilihan", Kind::Color),
                ],
            },
            Group {
                title: "Pencarian aplikasi",
                fields: &[f("launcher", "hotkey", "Pintasan keyboard", "Buka pencarian aplikasi dari mana saja", Kind::Hotkey)],
            },
            Group {
                title: "Media",
                fields: &[f("lyrics", "enabled", "Lirik tersinkron", "Judul, artis, dan durasi lagu dikirim ke lrclib.net", Kind::Toggle)],
            },
        ],
        footer: "",
    },
    Section {
        title: "Menu bar",
        icon: "\u{E7F4}",
        tint: 0xFF0A84FF,
        groups: &[
            Group {
                title: "",
                fields: &[
                    f("bar", "enabled", "Tampilkan menu bar", "", Kind::Toggle),
                    f("bar", "monitors", "Monitor", "", MONITORS),
                ],
            },
            Group {
                title: "Tampilan",
                fields: &[
                    f("bar", "height", "Tinggi", "", int(20.0, 64.0, 1.0, "px")),
                    f("bar", "opacity", "Opasitas latar", "", num(0.2, 1.0, 0.05, "%")),
                    f("bar", "background", "Warna latar", "", Kind::Color),
                    f("bar", "text", "Warna teks", "", Kind::Color),
                ],
            },
            Group {
                title: "Teks",
                fields: &[
                    f("bar", "font", "Font", "", Kind::Text),
                    f("bar", "font_size", "Ukuran font", "", num(9.0, 20.0, 0.5, "px")),
                    f("bar", "clock_format", "Format jam", "%a %A %d %b %B %m %Y %y %H %M", Kind::Text),
                    f("bar", "desktop_label", "Nama saat desktop aktif", "Ditampilkan saat explorer.exe di depan", Kind::Text),
                ],
            },
        ],
        footer: "Urutan menu dan item di kanan bar diatur di config.toml.",
    },
    Section {
        title: "Notch",
        icon: "\u{E8D6}",
        tint: 0xFF30D158,
        groups: &[
            Group {
                title: "",
                fields: &[
                    f("notch", "enabled", "Tampilkan notch", "Kontrol media, lirik, dan banner notifikasi", Kind::Toggle),
                    f("notch", "monitors", "Monitor", "", MONITORS),
                    f("notch", "expand_on_hover", "Buka saat kursor di atasnya", "", Kind::Toggle),
                ],
            },
            Group {
                title: "Ukuran",
                fields: &[
                    f("notch", "compact_width", "Lebar ringkas", "", int(120.0, 400.0, 2.0, "px")),
                    f("notch", "expanded_width", "Lebar terbuka", "", int(300.0, 800.0, 4.0, "px")),
                    f("notch", "expanded_height", "Tinggi terbuka", "", int(150.0, 400.0, 2.0, "px")),
                ],
            },
            Group {
                title: "Gaya & animasi",
                fields: &[
                    f("notch", "accent", "Warna aksen", "Progres lagu dan sorotan", Kind::Color),
                    f("notch", "close_delay_ms", "Jeda sebelum menutup", "", int(0.0, 2000.0, 50.0, "ms")),
                    f("notch", "animation_ms", "Durasi animasi", "", int(0.0, 600.0, 10.0, "ms")),
                ],
            },
        ],
        footer: "",
    },
    Section {
        title: "Dock",
        icon: "\u{ECAA}",
        tint: 0xFFBF5AF2,
        groups: &[
            Group {
                title: "",
                fields: &[
                    f("dock", "enabled", "Tampilkan Dock", "", Kind::Toggle),
                    f("dock", "replace_taskbar", "Gantikan taskbar Windows", "Taskbar dipulihkan saat HolmBar ditutup", Kind::Toggle),
                    f("dock", "monitors", "Monitor", "", MONITORS),
                    f("dock", "reserve_space", "Sisihkan ruang layar", "Jendela maksimal tidak menutupi Dock", Kind::Toggle),
                ],
            },
            Group {
                title: "Tampilan",
                fields: &[
                    f("dock", "icon_size", "Ukuran ikon", "", int(32.0, 96.0, 1.0, "px")),
                    f("dock", "magnification", "Pembesaran", "1.0× = mati", num(1.0, 2.5, 0.1, "x")),
                    f("dock", "opacity", "Opasitas latar", "", num(0.1, 1.0, 0.05, "%")),
                    f("dock", "background", "Warna latar", "", Kind::Color),
                ],
            },
            Group {
                title: "Aplikasi",
                fields: &[
                    f("dock", "start_button", "Tombol Windows di Dock", "Klik = cari aplikasi, klik kanan = menu daya", Kind::Toggle),
                    f("dock", "import_taskbar_pins", "Ikut pin taskbar Windows", "", Kind::Toggle),
                ],
            },
        ],
        footer: "Daftar aplikasi tersemat diatur di config.toml (dock.pinned).",
    },
    Section {
        title: "Notifikasi",
        icon: "\u{EA8F}",
        tint: 0xFFFF453A,
        groups: &[Group {
            title: "",
            fields: &[
                f("notifications", "enabled", "Banner notifikasi di notch", "Membaca banner Windows yang sedang tampil", Kind::Toggle),
                f("notifications", "show_body", "Tampilkan isi pesan", "Mati = hanya aplikasi dan judul", Kind::Toggle),
                f("notifications", "duration_ms", "Lama tampil", "", int(2000.0, 15000.0, 500.0, "s")),
            ],
        }],
        footer: "Riwayat notifikasi dan banner yang disenyapkan Do Not Disturb tidak terbaca.",
    },
];

/// Semua field berurutan: (indeks bagian, field). Indeks di sini = indeks nilai di `St::values`.
fn all_fields() -> Vec<(usize, &'static Field)> {
    SECTIONS.iter().enumerate().flat_map(|(si, s)| s.groups.iter().flat_map(move |g| g.fields.iter().map(move |f| (si, f)))).collect()
}

// ---------- Tata letak & warna ----------

type Rect = (f32, f32, f32, f32);

const W: f32 = 780.0;
const H: f32 = 560.0;
const M: f32 = 18.0; // margin bayangan
const SB: f32 = 214.0; // lebar sidebar
const ROW: f32 = 46.0;
const RADIUS: f32 = 12.0;

const BG: u32 = 0xFF1E1E21;
const SIDEBAR: u32 = 0xFF28282C;
const BORDER: u32 = 0xFF3C3C42;
const CARD: u32 = 0xFF2A2A2E;
const CARD_BORDER: u32 = 0xFF36363B;
const SEP: u32 = 0xFF38383E;
const TEXT: u32 = 0xFFF5F5F7;
const DIM: u32 = 0xFF9A9AA1;
const TRACK: u32 = 0xFF4A4A51;
const FIELD: u32 = 0xFF1A1A1D;
const HOVER: u32 = 0x16FFFFFF;
const OK: u32 = 0xFF30D158;
const ERR: u32 = 0xFFFF6961;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Close,
    Section(usize),
    OpenFile,
    Field(usize),
    Seg(usize, usize),
    Clear(usize),
}

struct St {
    hwnd: HWND,
    s: f32,
    section: usize,
    scroll: f32,
    max_scroll: f32,
    values: Vec<Option<toml::Value>>,
    rects: Vec<(Rect, Hit)>,
    hover: Option<Hit>,
    tracking: bool,
    drag: Option<(usize, f32, f32)>, // field, x awal track, lebar track
    editing: Option<(usize, String)>,
    recording: Option<usize>,
    status: Option<(String, bool)>, // (pesan, error?)
}

thread_local! {
    static STATE: RefCell<Option<St>> = const { RefCell::new(None) };
    static CUSTOM_COLORS: RefCell<[COLORREF; 16]> = RefCell::new([COLORREF(0x00FF_FFFF); 16]);
}

fn with<R>(f: impl FnOnce(&mut St) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut b| b.as_mut().map(f)))
}

// ---------- Siklus hidup ----------

/// Aktifkan manifest Common Controls v6 (resource #2, lihat build.rs) agar dialog warna
/// dan kotak pesan memakai gaya Windows modern. Gagal = gaya klasik, tidak fatal.
fn activate_visual_styles() {
    use windows::Win32::System::ApplicationInstallationAndServicing::{ActivateActCtx, CreateActCtxW, ACTCTXW};
    const ACTCTX_FLAG_RESOURCE_NAME_VALID: u32 = 0x8;
    const ACTCTX_FLAG_HMODULE_VALID: u32 = 0x80;
    unsafe {
        let Ok(hmod) = GetModuleHandleW(None) else { return };
        let ctx = ACTCTXW {
            cbSize: std::mem::size_of::<ACTCTXW>() as u32,
            dwFlags: ACTCTX_FLAG_HMODULE_VALID | ACTCTX_FLAG_RESOURCE_NAME_VALID,
            hModule: hmod,
            lpResourceName: PCWSTR(2 as *const u16),
            ..Default::default()
        };
        if let Ok(h) = CreateActCtxW(&ctx) {
            let mut cookie = 0usize;
            let _ = ActivateActCtx(Some(h), &mut cookie); // aktif sepanjang umur proses
        }
    }
}

pub fn register() {
    activate_visual_styles();
    unsafe {
        let icc = INITCOMMONCONTROLSEX { dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32, dwICC: ICC_STANDARD_CLASSES };
        let _ = InitCommonControlsEx(&icc);
        let Ok(hmod) = GetModuleHandleW(None) else { return };
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hmod.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hIcon: LoadIconW(None, IDI_APPLICATION).unwrap_or_default(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&wc);
    }
}

/// Buka jendela Pengaturan; bila sudah terbuka, bawa ke depan.
pub fn open() {
    if let Some(h) = with(|st| st.hwnd) {
        unsafe {
            if IsIconic(h).as_bool() {
                let _ = ShowWindow(h, SW_RESTORE);
            }
            let _ = SetForegroundWindow(h);
        }
        return;
    }
    let merged = config::merged_value().ok();
    let values = all_fields().iter().map(|(_, f)| merged.as_ref().and_then(|m| m.get(f.sec)?.get(f.key).cloned())).collect();
    unsafe {
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTOPRIMARY);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        let s = dx.max(96) as f32 / 96.0;
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let (w, h) = (((W + 2.0 * M) * s).round() as i32, ((H + 2.0 * M) * s).round() as i32);
        let a = mi.rcWork;
        let x = a.left + ((a.right - a.left) - w) / 2;
        let y = a.top + ((a.bottom - a.top) - h) / 2;
        let Ok(hmod) = GetModuleHandleW(None) else { return };
        let Ok(hwnd) = CreateWindowExW(WS_EX_LAYERED | WS_EX_APPWINDOW, CLASS, w!("Pengaturan HolmBar"), WS_POPUP | WS_SYSMENU | WS_MINIMIZEBOX, x, y, w, h, None, None, Some(hmod.into()), None) else {
            return;
        };
        STATE.with(|st| {
            *st.borrow_mut() = Some(St {
                hwnd,
                s,
                section: 0,
                scroll: 0.0,
                max_scroll: 0.0,
                values,
                rects: Vec::new(),
                hover: None,
                tracking: false,
                drag: None,
                editing: None,
                recording: None,
                status: None,
            })
        });
        redraw();
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
}

fn close() {
    let Some(h) = with(|st| st.hwnd) else { return };
    commit_edit();
    save();
    unsafe {
        let _ = DestroyWindow(h);
    }
}

// ---------- Nilai ----------

fn as_f64(v: Option<&toml::Value>) -> Option<f64> {
    match v? {
        toml::Value::Integer(i) => Some(*i as f64),
        toml::Value::Float(x) => Some(*x),
        _ => None,
    }
}

fn fmt_num(x: f64, step: f64) -> String {
    let dec = if step >= 1.0 { 0 } else if step >= 0.1 { 1 } else { 2 };
    format!("{x:.dec$}")
}

fn slider_text(v: f64, step: f64, unit: &str) -> String {
    match unit {
        "%" => format!("{}%", (v * 100.0).round()),
        "s" => format!("{:.1} dtk", v / 1000.0),
        "ms" => format!("{} ms", v.round()),
        "px" => format!("{} px", fmt_num(v, step)),
        "x" => format!("{:.1}×", v),
        _ => fmt_num(v, step),
    }
}

fn hotkey_text(s: &str) -> String {
    s.split('+')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| match p {
            "ctrl" | "control" => "Ctrl".to_string(),
            "alt" => "Alt".into(),
            "shift" => "Shift".into(),
            "win" | "super" => "Win".into(),
            "space" => "Space".into(),
            other => other.to_uppercase(),
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

fn set_value(i: usize, v: toml::Value) {
    let hwnd = with(|st| {
        st.values[i] = Some(v);
        st.status = None;
        st.hwnd
    });
    if let Some(h) = hwnd {
        unsafe {
            SetTimer(Some(h), TIMER_SAVE, SAVE_DELAY_MS, None);
        }
    }
    redraw();
}

fn commit_edit() {
    if let Some((i, text)) = with(|st| st.editing.take()).flatten() {
        set_value(i, toml::Value::String(text.trim().to_string()));
    }
}

fn same(old: Option<&toml::Value>, new: &toml::Value) -> bool {
    match (old, new) {
        (Some(a), toml::Value::Float(b)) => as_f64(Some(a)).map(|a| (a - b).abs() < 1e-9).unwrap_or(false),
        (Some(a), b) => a == b,
        (None, _) => false,
    }
}

fn to_edit(v: &toml::Value) -> toml_edit::Value {
    match v {
        toml::Value::Boolean(b) => (*b).into(),
        toml::Value::Integer(i) => (*i).into(),
        toml::Value::Float(x) => (*x).into(),
        toml::Value::String(s) => s.as_str().into(),
        other => other.to_string().as_str().into(),
    }
}

/// Ganti/tambah satu kunci; komentar di belakang nilai lama dipertahankan.
fn set_key(doc: &mut toml_edit::DocumentMut, sec: &str, key: &str, v: toml_edit::Value) -> Result<(), String> {
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

/// Tulis kunci yang berubah ke config.toml lalu muat ulang.
fn write_changes(values: &[Option<toml::Value>]) -> Result<bool, String> {
    let current = config::merged_value().map_err(|e| format!("config.toml berisi kesalahan: {e}"))?;
    let fields = all_fields();
    let changes: Vec<_> = fields
        .iter()
        .zip(values)
        .filter_map(|((_, f), v)| v.as_ref().filter(|v| !same(current.get(f.sec).and_then(|t| t.get(f.key)), v)).map(|v| (*f, v)))
        .collect();
    if changes.is_empty() {
        return Ok(false);
    }
    let path = config::path();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut doc = text.parse::<toml_edit::DocumentMut>().map_err(|e| format!("Gagal membaca config.toml: {e}"))?;
    for (f, v) in changes {
        set_key(&mut doc, f.sec, f.key, to_edit(v))?;
    }
    let out = doc.to_string();
    config::validate(&out).map_err(|e| format!("Tidak valid: {e}"))?;
    let _ = std::fs::create_dir_all(config::dir());
    std::fs::write(&path, out).map_err(|e| format!("Gagal menyimpan: {e}"))?;
    Ok(true)
}

fn save() {
    let Some((hwnd, values)) = with(|st| (st.hwnd, st.values.clone())) else { return };
    unsafe {
        let _ = KillTimer(Some(hwnd), TIMER_SAVE);
    }
    let result = write_changes(&values);
    if matches!(result, Ok(true)) {
        crate::reload_config();
    }
    with(|st| {
        st.status = match result {
            Ok(true) => Some(("Perubahan diterapkan".into(), false)),
            Ok(false) => st.status.take(),
            Err(e) => Some((e, true)),
        }
    });
    redraw();
}

// ---------- Gambar ----------

struct Fonts {
    body: Font,
    small: Font,
    group: Font,
    title: Font,
    app: Font,
    icon: Font,
    icon_s: Font,
}

fn fonts(s: f32) -> Fonts {
    let (ui, disp, ic) = ("Segoe UI Variable Text", "Segoe UI Variable Display", "Segoe Fluent Icons");
    Fonts {
        body: Font::new(ui, 13.0 * s, false),
        small: Font::new(ui, 11.5 * s, false),
        group: Font::new(ui, 12.0 * s, true),
        title: Font::new(disp, 20.0 * s, true),
        app: Font::new(disp, 15.0 * s, true),
        icon: Font::new(ic, 13.0 * s, false),
        icon_s: Font::new(ic, 8.0 * s, false),
    }
}

fn round(c: &Canvas, r: Rect, rad: f32, color: u32) {
    c.fill_round_rect(r.0, r.1, r.2, r.3, (rad, rad, rad, rad), color);
}

/// Kotak dengan garis tepi `border` setebal `bw`.
fn framed(c: &Canvas, r: Rect, rad: f32, bw: f32, border: u32, fill: u32) {
    round(c, r, rad, border);
    round(c, (r.0 + bw, r.1 + bw, r.2 - 2.0 * bw, r.3 - 2.0 * bw), (rad - bw).max(0.0), fill);
}

fn redraw() {
    with(render);
}

fn render(st: &mut St) {
    let s = st.s;
    let d = |v: f32| v * s;
    let (w, h) = (((W + 2.0 * M) * s).round() as i32, ((H + 2.0 * M) * s).round() as i32);
    let Some(c) = Canvas::new(w, h) else { return };
    let ft = fonts(s);
    let fields = all_fields();
    let accent = fields
        .iter()
        .position(|(_, f)| f.sec == "general" && f.key == "accent")
        .and_then(|i| st.values[i].as_ref()?.as_str().map(config::color))
        .map(|c| c | 0xFF00_0000)
        .unwrap_or(0xFF0A84FF);
    let hover = st.hover;
    let mut rects: Vec<(Rect, Hit)> = Vec::new();
    let (x0, y0, cw, ch) = (d(M), d(M), d(W), d(H));

    // Bayangan lembut + bingkai jendela.
    for k in 1..=14 {
        let k = d(k as f32);
        let r = d(RADIUS) + k;
        c.fill_round_rect(x0 - k, y0 - k + d(4.0), cw + 2.0 * k, ch + 2.0 * k, (r, r, r, r), 0x07000000);
    }
    framed(&c, (x0, y0, cw, ch), d(RADIUS), d(1.0), BORDER, BG);
    let inner = d(RADIUS - 1.0);
    c.fill_round_rect(x0 + d(1.0), y0 + d(1.0), d(SB) - d(1.0), ch - d(2.0), (inner, 0.0, 0.0, inner), SIDEBAR);
    c.fill_rect(x0 + d(SB), y0 + d(1.0), d(1.0), ch - d(2.0), BORDER);

    // Tombol jendela ala macOS: merah = tutup.
    let (lx, ly, ld) = (x0 + d(20.0), y0 + d(20.0), d(12.0));
    let close_r = (lx - d(4.0), ly - d(4.0), ld + d(8.0), ld + d(8.0));
    c.fill_ellipse(lx, ly, ld, ld, 0xFFFF5F57);
    if hover == Some(Hit::Close) {
        c.text("\u{E8BB}", &ft.icon_s, (lx, ly, ld, ld), 0xAA4D0000, Align::Center);
    }
    rects.push((close_r, Hit::Close));
    for i in 1..3 {
        c.fill_ellipse(lx + i as f32 * d(20.0), ly, ld, ld, 0xFF4A4A50);
    }

    // Judul aplikasi.
    c.text("HolmBar", &ft.app, (x0 + d(22.0), y0 + d(46.0), d(SB - 40.0), d(22.0)), TEXT, Align::Near);
    c.text("Pengaturan", &ft.small, (x0 + d(22.0), y0 + d(67.0), d(SB - 40.0), d(16.0)), DIM, Align::Near);

    // Daftar bagian.
    for (i, sec) in SECTIONS.iter().enumerate() {
        let r = (x0 + d(10.0), y0 + d(98.0) + i as f32 * d(36.0), d(SB - 20.0), d(32.0));
        if i == st.section {
            round(&c, r, d(7.0), accent);
        } else if hover == Some(Hit::Section(i)) {
            round(&c, r, d(7.0), HOVER);
        }
        let ic = (r.0 + d(8.0), r.1 + d(5.0), d(22.0), d(22.0));
        round(&c, ic, d(6.0), sec.tint);
        c.text(sec.icon, &ft.icon, ic, 0xFFFFFFFF, Align::Center);
        c.text(sec.title, &ft.body, (r.0 + d(40.0), r.1, r.2 - d(46.0), r.3), TEXT, Align::Near);
        rects.push((r, Hit::Section(i)));
    }

    // Status simpan + tautan file config di bawah sidebar.
    if let Some((msg, err)) = &st.status {
        let (color, icon) = if *err { (ERR, "\u{E783}") } else { (OK, "\u{E73E}") };
        let r = (x0 + d(20.0), y0 + ch - d(80.0), d(SB - 36.0), d(20.0));
        c.text(icon, &ft.icon, (r.0, r.1, d(16.0), r.3), color, Align::Near);
        c.text(msg, &ft.small, (r.0 + d(20.0), r.1, r.2 - d(20.0), r.3), color, Align::Near);
    }
    let of = (x0 + d(10.0), y0 + ch - d(50.0), d(SB - 20.0), d(32.0));
    if hover == Some(Hit::OpenFile) {
        round(&c, of, d(7.0), HOVER);
    }
    c.text("\u{E8A5}", &ft.icon, (of.0 + d(12.0), of.1, d(18.0), of.3), DIM, Align::Center);
    c.text("Buka config.toml", &ft.body, (of.0 + d(40.0), of.1, of.2 - d(46.0), of.3), DIM, Align::Near);
    rects.push((of, Hit::OpenFile));

    // Konten bagian aktif.
    let sec = &SECTIONS[st.section];
    let cx = x0 + d(SB);
    let cwid = cw - d(SB);
    c.text(sec.title, &ft.title, (cx + d(28.0), y0 + d(18.0), cwid - d(56.0), d(32.0)), TEXT, Align::Near);
    let top = y0 + d(62.0);
    let bottom = y0 + ch - d(2.0);
    c.clip(cx + d(1.0), top, cwid - d(2.0), bottom - top);
    let visible = |r: &Rect| r.1 + r.3 / 2.0 > top && r.1 + r.3 / 2.0 < bottom;

    let base = fields.iter().position(|(si, _)| *si == st.section).unwrap_or(0);
    let mut idx = base;
    let mut y = top + d(4.0) - st.scroll;
    let (card_x, card_w) = (cx + d(24.0), cwid - d(48.0));
    for g in sec.groups {
        if !g.title.is_empty() {
            c.text(g.title, &ft.group, (card_x + d(6.0), y, card_w, d(20.0)), DIM, Align::Near);
            y += d(26.0);
        }
        let card = (card_x, y, card_w, g.fields.len() as f32 * d(ROW));
        framed(&c, card, d(10.0), d(1.0), CARD_BORDER, CARD);
        for (row, field) in g.fields.iter().enumerate() {
            let ry = y + row as f32 * d(ROW);
            if row > 0 {
                c.fill_rect(card_x + d(16.0), ry, card_w - d(32.0), d(1.0), SEP);
            }
            let label_w = card_w * 0.5;
            if field.hint.is_empty() {
                c.text(field.label, &ft.body, (card_x + d(16.0), ry, label_w, d(ROW)), TEXT, Align::Near);
            } else {
                c.text(field.label, &ft.body, (card_x + d(16.0), ry + d(6.0), label_w, d(19.0)), TEXT, Align::Near);
                c.text(field.hint, &ft.small, (card_x + d(16.0), ry + d(24.0), label_w, d(16.0)), DIM, Align::Near);
            }
            let rx = card_x + card_w - d(16.0);
            let cy = ry + d(ROW) / 2.0;
            let val = st.values[idx].as_ref();
            let mut push = |r: Rect, hit: Hit| {
                if visible(&r) {
                    rects.push((r, hit));
                }
            };
            match field.kind {
                Kind::Toggle => {
                    let on = val.and_then(|v| v.as_bool()).unwrap_or(false);
                    let r = (rx - d(40.0), cy - d(12.0), d(40.0), d(24.0));
                    round(&c, r, d(12.0), if on { accent } else { TRACK });
                    let kx = if on { r.0 + d(18.0) } else { r.0 + d(2.0) };
                    c.fill_ellipse(kx, r.1 + d(2.0), d(20.0), d(20.0), 0xFFFFFFFF);
                    push((r.0 - d(6.0), ry, r.2 + d(12.0), d(ROW)), Hit::Field(idx));
                }
                Kind::Choice(opts) => {
                    let cur = val.and_then(|v| v.as_str()).unwrap_or("");
                    let seg_w = opts.iter().map(|(_, t)| c.measure(t, &ft.body)).fold(0.0f32, f32::max) + d(26.0);
                    let total = seg_w * opts.len() as f32 + d(4.0);
                    let r = (rx - total, cy - d(14.0), total, d(28.0));
                    round(&c, r, d(8.0), 0xFF3A3A40);
                    for (j, (v, t)) in opts.iter().enumerate() {
                        let sr = (r.0 + d(2.0) + j as f32 * seg_w, r.1 + d(2.0), seg_w, r.3 - d(4.0));
                        if *v == cur {
                            round(&c, sr, d(6.0), 0xFF66666D);
                        } else if hover == Some(Hit::Seg(idx, j)) {
                            round(&c, sr, d(6.0), HOVER);
                        }
                        c.text(t, &ft.body, sr, TEXT, Align::Center);
                        push(sr, Hit::Seg(idx, j));
                    }
                }
                Kind::Slider { min, max, step, unit, .. } => {
                    let v = as_f64(val).unwrap_or(min);
                    let frac = (((v - min) / (max - min)) as f32).clamp(0.0, 1.0);
                    let vw = d(66.0);
                    c.text(&slider_text(v, step, unit), &ft.body, (rx - vw, cy - d(10.0), vw, d(20.0)), TEXT, Align::Far);
                    let tw = d(180.0);
                    let tx = rx - vw - d(14.0) - tw;
                    round(&c, (tx, cy - d(2.0), tw, d(4.0)), d(2.0), TRACK);
                    round(&c, (tx, cy - d(2.0), tw * frac, d(4.0)), d(2.0), accent);
                    let kx = tx + tw * frac;
                    let kr = if hover == Some(Hit::Field(idx)) || st.drag.map(|(i, ..)| i) == Some(idx) { d(9.0) } else { d(8.0) };
                    c.fill_ellipse(kx - kr - d(0.5), cy - kr + d(1.0), 2.0 * kr + d(1.0), 2.0 * kr + d(1.0), 0x50000000);
                    c.fill_ellipse(kx - kr, cy - kr, 2.0 * kr, 2.0 * kr, 0xFFFFFFFF);
                    push((tx - d(10.0), ry, tw + d(20.0), d(ROW)), Hit::Field(idx));
                }
                Kind::Color => {
                    let hex = val.and_then(|v| v.as_str()).unwrap_or("");
                    let col = config::color(hex) | 0xFF00_0000;
                    let sw = (rx - d(24.0), cy - d(12.0), d(24.0), d(24.0));
                    c.fill_ellipse(sw.0, sw.1, sw.2, sw.3, 0x66FFFFFF);
                    c.fill_ellipse(sw.0 + d(1.5), sw.1 + d(1.5), sw.2 - d(3.0), sw.3 - d(3.0), col);
                    let hv = hover == Some(Hit::Field(idx));
                    c.text(&hex.to_uppercase(), &ft.body, (rx - d(130.0), cy - d(10.0), d(96.0), d(20.0)), if hv { TEXT } else { DIM }, Align::Far);
                    push((rx - d(134.0), cy - d(15.0), d(134.0), d(30.0)), Hit::Field(idx));
                }
                Kind::Text => {
                    let r = (rx - d(240.0), cy - d(14.0), d(240.0), d(28.0));
                    let editing = st.editing.as_ref().filter(|(i, _)| *i == idx).map(|(_, t)| t.as_str());
                    match editing {
                        Some(_) => framed(&c, r, d(7.0), d(2.0), accent, FIELD),
                        None => framed(&c, r, d(7.0), d(1.0), if hover == Some(Hit::Field(idx)) { 0xFF5A5A62 } else { 0xFF44444B }, FIELD),
                    }
                    let tr = (r.0 + d(10.0), r.1, r.2 - d(20.0), r.3);
                    let text = editing.unwrap_or_else(|| val.and_then(|v| v.as_str()).unwrap_or(""));
                    let tw = c.measure(text, &ft.body);
                    let al = if editing.is_some() && tw > tr.2 { Align::Far } else { Align::Near };
                    c.text(text, &ft.body, tr, TEXT, al);
                    if editing.is_some() {
                        let caret_x = if al == Align::Far { tr.0 + tr.2 } else { tr.0 + tw };
                        c.fill_rect(caret_x + d(1.0), cy - d(8.0), d(1.5), d(16.0), accent);
                    }
                    push(r, Hit::Field(idx));
                }
                Kind::Hotkey => {
                    let r = (rx - d(210.0), cy - d(14.0), d(210.0), d(28.0));
                    let rec = st.recording == Some(idx);
                    let cur = val.and_then(|v| v.as_str()).unwrap_or("");
                    if rec {
                        framed(&c, r, d(7.0), d(2.0), accent, FIELD);
                        c.text("Tekan kombinasi tombol…", &ft.body, r, DIM, Align::Center);
                    } else {
                        framed(&c, r, d(7.0), d(1.0), if hover == Some(Hit::Field(idx)) { 0xFF5A5A62 } else { 0xFF44444B }, FIELD);
                        if cur.is_empty() {
                            c.text("Klik untuk merekam", &ft.body, r, DIM, Align::Center);
                        } else {
                            c.text(&hotkey_text(cur), &ft.body, (r.0, r.1, r.2 - d(18.0), r.3), TEXT, Align::Center);
                        }
                    }
                    push(r, Hit::Field(idx));
                    if !rec && !cur.is_empty() {
                        let x = (r.0 + r.2 - d(24.0), cy - d(9.0), d(18.0), d(18.0));
                        c.fill_ellipse(x.0, x.1, x.2, x.3, if hover == Some(Hit::Clear(idx)) { 0xFF6A6A72 } else { 0xFF48484F });
                        c.text("\u{E8BB}", &ft.icon_s, x, TEXT, Align::Center);
                        push(x, Hit::Clear(idx));
                    }
                }
            }
            idx += 1;
        }
        y += card.3 + d(24.0);
    }
    if !sec.footer.is_empty() {
        c.text(sec.footer, &ft.small, (card_x + d(6.0), y - d(14.0), card_w, d(18.0)), DIM, Align::Near);
        y += d(14.0);
    }
    c.reset_clip();
    st.max_scroll = (y + st.scroll - top - (bottom - top) + d(8.0)).max(0.0);
    if st.scroll > st.max_scroll {
        st.scroll = st.max_scroll;
    }
    // Indikator gulir tipis bila konten lebih panjang dari jendela.
    if st.max_scroll > 0.0 {
        let view = bottom - top;
        let bar_h = (view * view / (view + st.max_scroll)).max(d(30.0));
        let by = top + (view - bar_h) * (st.scroll / st.max_scroll);
        round(&c, (cx + cwid - d(8.0), by, d(4.0), bar_h), d(2.0), 0x40FFFFFF);
    }

    st.rects = rects;
    c.present(st.hwnd, None);
}

// ---------- Input ----------

fn xy(lp: LPARAM) -> (f32, f32) {
    ((lp.0 & 0xFFFF) as i16 as f32, ((lp.0 >> 16) & 0xFFFF) as i16 as f32)
}

fn hit_at(x: f32, y: f32) -> Option<Hit> {
    with(|st| st.rects.iter().rev().find(|(r, _)| x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3).map(|(_, h)| *h)).flatten()
}

fn field(i: usize) -> &'static Field {
    all_fields()[i].1
}

fn slider_set(i: usize, x: f32) {
    let Kind::Slider { min, max, step, int, .. } = field(i).kind else { return };
    let Some(Some((tx, tw))) = with(|st| st.drag.map(|(_, tx, tw)| (tx, tw))) else { return };
    let frac = ((x - tx) / tw).clamp(0.0, 1.0) as f64;
    let v = (min + frac * (max - min)) / step;
    let v = (v.round() * step).clamp(min, max);
    let new = if int { toml::Value::Integer(v.round() as i64) } else { toml::Value::Float((v * 100.0).round() / 100.0) };
    if with(|st| st.values[i].as_ref() != Some(&new)).unwrap_or(false) {
        with(|st| st.values[i] = Some(new));
        redraw();
    }
}

fn pick_color(i: usize) {
    let Some((hwnd, cur)) = with(|st| (st.hwnd, st.values[i].as_ref().and_then(|v| v.as_str()).unwrap_or("").to_string())) else { return };
    let hex = cur.trim().trim_start_matches('#');
    let argb = config::color(&cur);
    let rgb = argb & 0x00FF_FFFF;
    let bgr = ((rgb & 0xFF) << 16) | (rgb & 0xFF00) | ((rgb >> 16) & 0xFF);
    let mut custom = CUSTOM_COLORS.with(|c| *c.borrow());
    let mut cc = CHOOSECOLORW {
        lStructSize: std::mem::size_of::<CHOOSECOLORW>() as u32,
        hwndOwner: hwnd,
        rgbResult: COLORREF(bgr),
        lpCustColors: custom.as_mut_ptr(),
        Flags: CC_RGBINIT | CC_FULLOPEN,
        ..Default::default()
    };
    if unsafe { ChooseColorW(&mut cc) }.as_bool() {
        CUSTOM_COLORS.with(|c| *c.borrow_mut() = custom);
        let r = cc.rgbResult.0;
        let rgb = ((r & 0xFF) << 16) | (r & 0xFF00) | ((r >> 16) & 0xFF);
        let s = if hex.len() == 8 { format!("#{:02X}{:06X}", argb >> 24, rgb) } else { format!("#{rgb:06X}") };
        set_value(i, toml::Value::String(s));
    }
}

fn click(hwnd: HWND, x: f32, y: f32) {
    let hit = hit_at(x, y);
    // Klik di luar kotak teks yang sedang diedit = selesai mengedit.
    let editing = with(|st| st.editing.as_ref().map(|(i, _)| *i)).flatten();
    if editing.is_some() && hit != editing.map(Hit::Field) {
        commit_edit();
    }
    if !matches!(hit, Some(Hit::Field(i)) if with(|st| st.recording == Some(i)).unwrap_or(false)) {
        with(|st| st.recording = None);
    }
    match hit {
        Some(Hit::Close) => close(),
        Some(Hit::Section(i)) => {
            with(|st| {
                st.section = i;
                st.scroll = 0.0;
            });
        }
        Some(Hit::OpenFile) => crate::open_config(),
        Some(Hit::Seg(i, j)) => {
            if let Kind::Choice(opts) = field(i).kind {
                set_value(i, toml::Value::String(opts[j].0.to_string()));
            }
        }
        Some(Hit::Clear(i)) => set_value(i, toml::Value::String(String::new())),
        Some(Hit::Field(i)) => match field(i).kind {
            Kind::Toggle => {
                let on = with(|st| st.values[i].as_ref().and_then(|v| v.as_bool()).unwrap_or(false)).unwrap_or(false);
                set_value(i, toml::Value::Boolean(!on));
            }
            Kind::Slider { .. } => {
                // Posisi track dihitung ulang dari rect hit (track = rect tanpa padding 10 DIP).
                let r = with(|st| st.rects.iter().find(|(_, h)| *h == Hit::Field(i)).map(|(r, _)| *r)).flatten();
                if let Some(r) = r {
                    let pad = with(|st| 10.0 * st.s).unwrap_or(10.0);
                    with(|st| st.drag = Some((i, r.0 + pad, r.2 - 2.0 * pad)));
                    unsafe {
                        SetCapture(hwnd);
                    }
                    slider_set(i, x);
                }
            }
            Kind::Color => pick_color(i),
            Kind::Text => {
                with(|st| {
                    if st.editing.is_none() {
                        let cur = st.values[i].as_ref().and_then(|v| v.as_str()).unwrap_or("").to_string();
                        st.editing = Some((i, cur));
                    }
                });
            }
            Kind::Hotkey => {
                with(|st| st.recording = Some(i));
            }
            Kind::Choice(_) => {}
        },
        None => {}
    }
    redraw();
}

fn key_down(vk: u16) -> bool {
    let down = |k: VIRTUAL_KEY| unsafe { GetKeyState(k.0 as i32) } < 0;
    let ctrl = down(VK_CONTROL);

    // Merekam pintasan.
    if let Some(i) = with(|st| st.recording).flatten() {
        let k = VIRTUAL_KEY(vk);
        if [VK_CONTROL, VK_SHIFT, VK_MENU, VK_LWIN, VK_RWIN, VK_LCONTROL, VK_RCONTROL, VK_LSHIFT, VK_RSHIFT, VK_LMENU, VK_RMENU].contains(&k) {
            return true; // tunggu tombol utama
        }
        let mut mods = Vec::new();
        if ctrl {
            mods.push("ctrl");
        }
        if down(VK_MENU) {
            mods.push("alt");
        }
        if down(VK_SHIFT) {
            mods.push("shift");
        }
        if down(VK_LWIN) || down(VK_RWIN) {
            mods.push("win");
        }
        let key = match vk {
            0x41..=0x5A | 0x30..=0x39 => Some(((vk as u8) as char).to_ascii_lowercase().to_string()),
            0x70..=0x7B => Some(format!("f{}", vk - 0x6F)),
            0x20 => Some("space".to_string()),
            _ => None,
        };
        match (vk, key) {
            (0x1B, _) if mods.is_empty() => {
                with(|st| st.recording = None);
            }
            (0x08 | 0x2E, _) if mods.is_empty() => {
                with(|st| st.recording = None);
                set_value(i, toml::Value::String(String::new()));
            }
            (_, Some(k)) if !mods.is_empty() => {
                with(|st| st.recording = None);
                set_value(i, toml::Value::String(format!("{}+{k}", mods.join("+"))));
            }
            _ => {
                with(|st| st.status = Some(("Pakai Ctrl/Alt/Shift/Win + huruf, angka, F1–F12, atau Spasi".into(), true)));
            }
        }
        redraw();
        return true;
    }

    // Mengedit teks.
    if with(|st| st.editing.is_some()).unwrap_or(false) {
        match vk {
            0x0D => commit_edit(),
            0x1B => {
                with(|st| st.editing = None);
            }
            0x08 => {
                with(|st| {
                    if let Some((_, t)) = st.editing.as_mut() {
                        if ctrl {
                            t.clear();
                        } else {
                            t.pop();
                        }
                    }
                });
            }
            0x56 if ctrl => {
                let clip = clipboard_text();
                with(|st| {
                    if let Some((_, t)) = st.editing.as_mut() {
                        t.push_str(&clip.replace(['\r', '\n'], " "));
                    }
                });
            }
            _ => return false,
        }
        redraw();
        return true;
    }

    match vk {
        0x1B => close(),
        0x26 | 0x28 => {
            // Panah atas/bawah berpindah bagian.
            with(|st| {
                let n = SECTIONS.len();
                st.section = if vk == 0x26 { (st.section + n - 1) % n } else { (st.section + 1) % n };
                st.scroll = 0.0;
            });
            redraw();
        }
        _ => return false,
    }
    true
}

fn clipboard_text() -> String {
    const CF_UNICODETEXT: u32 = 13;
    let mut out = String::new();
    unsafe {
        let owner = with(|st| st.hwnd);
        if OpenClipboard(owner).is_err() {
            return out;
        }
        if let Ok(h) = GetClipboardData(CF_UNICODETEXT) {
            let g = HGLOBAL(h.0);
            let p = GlobalLock(g) as *const u16;
            if !p.is_null() {
                let mut n = 0;
                while *p.add(n) != 0 && n < 4096 {
                    n += 1;
                }
                out = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
                let _ = GlobalUnlock(g);
            }
        }
        let _ = CloseClipboard();
    }
    out
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_NCHITTEST => {
            let mut pt = POINT { x: (lparam.0 & 0xFFFF) as i16 as i32, y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32 };
            unsafe {
                let _ = ScreenToClient(hwnd, &mut pt);
            }
            let (x, y) = (pt.x as f32, pt.y as f32);
            let s = with(|st| st.s).unwrap_or(1.0);
            // Bayangan tembus klik; strip atas jendela bisa diseret.
            if x < M * s || y < M * s || x > (M + W) * s || y > (M + H) * s {
                return LRESULT(HTTRANSPARENT as isize);
            }
            if y < (M + 56.0) * s && hit_at(x, y).is_none() {
                return LRESULT(HTCAPTION as isize);
            }
            LRESULT(HTCLIENT as isize)
        }
        WM_SETCURSOR => {
            if (lparam.0 & 0xFFFF) as u32 == HTCLIENT {
                let mut pt = POINT::default();
                unsafe {
                    let _ = GetCursorPos(&mut pt);
                    let _ = ScreenToClient(hwnd, &mut pt);
                }
                let cursor = match hit_at(pt.x as f32, pt.y as f32) {
                    Some(Hit::Field(i)) if matches!(field(i).kind, Kind::Text) => IDC_IBEAM,
                    Some(_) => IDC_HAND,
                    None => IDC_ARROW,
                };
                unsafe {
                    SetCursor(LoadCursorW(None, cursor).ok());
                }
                return LRESULT(1);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_MOUSEMOVE => {
            let (x, y) = xy(lparam);
            if let Some(Some((i, ..))) = with(|st| st.drag) {
                slider_set(i, x);
                return LRESULT(0);
            }
            let start_track = with(|st| !std::mem::replace(&mut st.tracking, true)).unwrap_or(false);
            if start_track {
                let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                unsafe {
                    let _ = TrackMouseEvent(&mut tme);
                }
            }
            let h = hit_at(x, y);
            if with(|st| std::mem::replace(&mut st.hover, h) != h).unwrap_or(false) {
                redraw();
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            if with(|st| {
                st.tracking = false;
                st.hover.take().is_some()
            })
            .unwrap_or(false)
            {
                redraw();
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = xy(lparam);
            click(hwnd, x, y);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if let Some(Some((i, ..))) = with(|st| st.drag.take()) {
                unsafe {
                    let _ = ReleaseCapture();
                }
                if let Some(v) = with(|st| st.values[i].clone()).flatten() {
                    set_value(i, v); // jadwalkan simpan setelah selesai menyeret
                }
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) & 0xFFFF) as i16 as f32 / 120.0;
            if with(|st| {
                let old = st.scroll;
                st.scroll = (st.scroll - delta * 56.0 * st.s).clamp(0.0, st.max_scroll);
                st.scroll != old
            })
            .unwrap_or(false)
            {
                redraw();
            }
            LRESULT(0)
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            if key_down(wparam.0 as u16) {
                return LRESULT(0);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_SYSCHAR if with(|st| st.recording.is_some()).unwrap_or(false) => LRESULT(0), // tanpa bunyi "ding" untuk Alt+...
        WM_CHAR => {
            let ch = char::from_u32(wparam.0 as u32).unwrap_or('\0');
            if !ch.is_control() {
                let changed = with(|st| st.editing.as_mut().map(|(_, t)| t.push(ch)).is_some()).unwrap_or(false);
                if changed {
                    redraw();
                }
            }
            LRESULT(0)
        }
        WM_ACTIVATE => {
            if wparam.0 & 0xFFFF == 0 {
                // Kehilangan fokus: simpan teks yang sedang diedit, batalkan perekaman.
                commit_edit();
                with(|st| st.recording = None);
                redraw();
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_SAVE => {
            if with(|st| st.drag.is_none()).unwrap_or(false) {
                save();
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let r = unsafe { *(lparam.0 as *const RECT) };
            with(|st| st.s = (wparam.0 & 0xFFFF) as f32 / 96.0);
            unsafe {
                let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            }
            redraw();
            LRESULT(0)
        }
        WM_CLOSE => {
            close();
            LRESULT(0)
        }
        WM_DESTROY => {
            STATE.with(|s| s.borrow_mut().take());
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_key_keeps_comments_and_adds_missing() {
        let mut doc: toml_edit::DocumentMut = "# atas\n[bar]\nheight = 32   # tinggi\nfont = \"A\"\n".parse().unwrap();
        set_key(&mut doc, "bar", "height", toml_edit::Value::from(40i64)).unwrap();
        set_key(&mut doc, "dock", "opacity", toml_edit::Value::from(0.5f64)).unwrap();
        let out = doc.to_string();
        assert!(out.contains("# atas"));
        assert!(out.contains("height = 40   # tinggi"), "{out}");
        assert!(out.contains("font = \"A\""));
        assert!(out.contains("[dock]") && out.contains("opacity = 0.5"), "{out}");
        config::validate(&out).expect("hasil tetap valid");
    }

    #[test]
    fn every_field_exists_in_defaults() {
        let v = config::merged_value().unwrap();
        for (_, f) in all_fields() {
            assert!(v.get(f.sec).and_then(|t| t.get(f.key)).is_some(), "{}.{} tidak ada di default_config.toml", f.sec, f.key);
        }
    }

    #[test]
    fn slider_labels() {
        assert_eq!(slider_text(0.9, 0.05, "%"), "90%");
        assert_eq!(slider_text(5000.0, 500.0, "s"), "5.0 dtk");
        assert_eq!(slider_text(1.7, 0.1, "x"), "1.7×");
        assert_eq!(hotkey_text("ctrl+shift+f12"), "Ctrl + Shift + F12");
    }
}
