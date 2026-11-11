//! Pratinjau jendela saat kursor diam di atas ikon Dock (seperti taskbar Windows): thumbnail HIDUP lewat DWM,
//! judul jendela, tombol tutup, klik = pindah ke jendela itu, klik tengah = tutup jendela.
//! Latar digambar dengan Canvas lalu disalin (BitBlt) ke jendela biasa; thumbnail DWM tampil di atasnya.

use crate::canvas::{Align, Canvas, Font};
use crate::config;
use crate::icons::{self, Icon};
use crate::monitors::Mon;
use std::cell::RefCell;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const CLASS: PCWSTR = w!("HolmPreviewClass");
const TIMER_POLL: usize = 1;
type Rect = (f32, f32, f32, f32);

struct Card {
    src: isize,
    title: String,
    rect: Rect,
    close: Rect,
    thumb: isize,
}

struct Pv {
    hwnd: HWND,
    cards: Vec<Card>,
    /// (indeks kartu, kursor di tombol tutup?)
    hover: Option<(usize, bool)>,
    s: f32,
    w: i32,
    h: i32,
    /// Batas bawah monitor (layar): kursor di antara pratinjau dan tepi ini dianggap masih "di dalam".
    floor: i32,
}

thread_local! {
    static ST: RefCell<Option<Pv>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Pv) -> R) -> Option<R> {
    ST.with(|s| s.borrow_mut().as_mut().map(f))
}

pub fn register() {
    unsafe {
        let Ok(hmod) = GetModuleHandleW(None) else { return };
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hmod.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&wc);
    }
}

pub fn hide() {
    let Some(p) = ST.with(|s| s.borrow_mut().take()) else { return };
    unsafe {
        let _ = KillTimer(Some(p.hwnd), TIMER_POLL);
        for c in &p.cards {
            if c.thumb != 0 {
                let _ = DwmUnregisterThumbnail(c.thumb);
            }
        }
        let _ = DestroyWindow(p.hwnd);
    }
}

fn win_title(h: HWND) -> String {
    let mut b = [0u16; 256];
    let n = unsafe { GetWindowTextW(h, &mut b) };
    let t = String::from_utf16_lossy(&b[..n.max(0) as usize]);
    if t.trim().is_empty() {
        "(tanpa judul)".into()
    } else {
        t
    }
}

/// Tampilkan pratinjau `wins` di atas titik layar (`anchor_x`, `bottom`): bagian bawah pratinjau rata dengan `bottom`.
pub fn show(wins: &[isize], anchor_x: i32, bottom: i32, mon: Mon) {
    hide();
    if wins.is_empty() {
        return;
    }
    let s = mon.scale;
    let (card_w, card_h, gap, pad) = (240.0 * s, 178.0 * s, 8.0 * s, 8.0 * s);
    let n = wins.len().min(15);
    let cols = n.min(5);
    let rows = n.div_ceil(cols);
    let w = (cols as f32 * card_w + (cols as f32 - 1.0) * gap + 2.0 * pad).round() as i32;
    let h = (rows as f32 * card_h + (rows as f32 - 1.0) * gap + 2.0 * pad).round() as i32;
    let x = (anchor_x - w / 2).clamp(mon.x + 8, (mon.x + mon.w - w - 8).max(mon.x + 8));
    let y = (bottom - h).max(mon.y + (40.0 * s) as i32);
    let mut cards = Vec::new();
    for (i, &src) in wins.iter().take(n).enumerate() {
        let (c, r) = (i % cols, i / cols);
        let (cx, cy) = (pad + c as f32 * (card_w + gap), pad + r as f32 * (card_h + gap));
        let d = 22.0 * s;
        cards.push(Card {
            src,
            title: win_title(HWND(src as *mut _)),
            rect: (cx, cy, card_w, card_h),
            close: (cx + card_w - d - 6.0 * s, cy + 6.0 * s, d, d),
            thumb: 0,
        });
    }
    unsafe {
        let Ok(hmod) = GetModuleHandleW(None) else { return };
        let Ok(hwnd) = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE, CLASS, w!(""), WS_POPUP, x, y, w, h, None, None, Some(hmod.into()), None) else { return };
        let r = (14.0 * s) as i32;
        let rgn = CreateRoundRectRgn(0, 0, w + 1, h + 1, 2 * r, 2 * r);
        SetWindowRgn(hwnd, Some(rgn), true);
        for c in cards.iter_mut() {
            c.thumb = DwmRegisterThumbnail(hwnd, HWND(c.src as *mut _)).unwrap_or(0);
            place(c, s);
        }
        ST.with(|st| *st.borrow_mut() = Some(Pv { hwnd, cards, hover: None, s, w, h, floor: mon.y + mon.h }));
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        SetTimer(Some(hwnd), TIMER_POLL, 120, None);
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

/// Atur kotak tujuan thumbnail di dalam kartu (menjaga rasio gambar, di tengah area gambar).
fn place(c: &Card, s: f32) {
    if c.thumb == 0 {
        return;
    }
    let (ax, ay, aw, ah) = (c.rect.0 + 14.0 * s, c.rect.1 + 38.0 * s, c.rect.2 - 28.0 * s, c.rect.3 - 38.0 * s - 14.0 * s);
    let size = unsafe { DwmQueryThumbnailSourceSize(c.thumb) }.unwrap_or(SIZE { cx: 16, cy: 9 });
    let (sw, sh) = (size.cx.max(1) as f32, size.cy.max(1) as f32);
    let k = (aw / sw).min(ah / sh).min(1.0).max(0.01);
    let (tw, th) = (sw * k, sh * k);
    let (tx, ty) = (ax + (aw - tw) / 2.0, ay + (ah - th) / 2.0);
    let props = DWM_THUMBNAIL_PROPERTIES {
        dwFlags: 0x1 | 0x4 | 0x8, // RECTDESTINATION | OPACITY | VISIBLE
        rcDestination: RECT { left: tx as i32, top: ty as i32, right: (tx + tw) as i32, bottom: (ty + th) as i32 },
        rcSource: RECT::default(),
        opacity: 255,
        fVisible: windows::core::BOOL(1),
        fSourceClientAreaOnly: windows::core::BOOL(0),
    };
    unsafe {
        let _ = DwmUpdateThumbnailProperties(c.thumb, &props);
    }
}

fn inside(r: Rect, x: f32, y: f32) -> bool {
    x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3
}

fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    unsafe {
        let hdc = BeginPaint(hwnd, &mut ps);
        let _ = with(|p| {
            let Some(c) = Canvas::new(p.w, p.h) else { return };
            let (s, font_name) = (p.s, config::current().bar.font.clone());
            let r = 14.0 * s;
            c.fill_round_rect(0.0, 0.0, p.w as f32, p.h as f32, (r, r, r, r), 0xFF4A4A52);
            c.fill_round_rect(1.0, 1.0, p.w as f32 - 2.0, p.h as f32 - 2.0, (r - 1.0, r - 1.0, r - 1.0, r - 1.0), 0xFF1E1E23);
            let title_font = Font::new(&font_name, 12.5 * s, false);
            for (i, card) in p.cards.iter().enumerate() {
                let hov = p.hover.map(|(h, _)| h == i).unwrap_or(false);
                let k = 10.0 * s;
                c.fill_round_rect(card.rect.0, card.rect.1, card.rect.2, card.rect.3, (k, k, k, k), if hov { 0x30FFFFFF } else { 0x14FFFFFF });
                let tw = card.rect.2 - 2.0 * 14.0 * s - if hov { 26.0 * s } else { 0.0 };
                c.text(&card.title, &title_font, (card.rect.0 + 14.0 * s, card.rect.1 + 8.0 * s, tw, 22.0 * s), 0xFFF2F2F5, Align::Near);
                if hov {
                    let on_x = p.hover.map(|(_, x)| x).unwrap_or(false);
                    let cr = card.close;
                    c.fill_ellipse(cr.0, cr.1, cr.2, cr.3, if on_x { 0xFFE5484D } else { 0x33FFFFFF });
                    let ic = 10.0 * s;
                    icons::draw(&c, Icon::Close, (cr.0 + (cr.2 - ic) / 2.0, cr.1 + (cr.3 - ic) / 2.0, ic, ic), 0xFFFFFFFF, 0xFF1E1E23);
                }
            }
            c.blit(hdc, p.w, p.h);
        });
        let _ = EndPaint(hwnd, &ps);
    }
}

fn pick(x: f32, y: f32) -> Option<(usize, bool)> {
    with(|p| p.cards.iter().position(|c| inside(c.rect, x, y)).map(|i| (i, inside(p.cards[i].close, x, y)))).flatten()
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let xy = |l: LPARAM| ((l.0 & 0xFFFF) as i16 as f32, ((l.0 >> 16) & 0xFFFF) as i16 as f32);
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_MOUSEMOVE => {
            let (x, y) = xy(lparam);
            let h = pick(x, y);
            if with(|p| std::mem::replace(&mut p.hover, h) != h).unwrap_or(false) {
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP | WM_MBUTTONUP => {
            let (x, y) = xy(lparam);
            let Some((i, on_close)) = pick(x, y) else { return LRESULT(0) };
            let Some(src) = with(|p| p.cards[i].src) else { return LRESULT(0) };
            let target = HWND(src as *mut _);
            if on_close || msg == WM_MBUTTONUP {
                unsafe {
                    let _ = PostMessageW(Some(target), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            } else {
                unsafe {
                    if IsIconic(target).as_bool() {
                        let _ = ShowWindow(target, SW_RESTORE);
                    }
                    let _ = SetForegroundWindow(target);
                }
            }
            hide();
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_POLL => {
            // Tutup bila kursor keluar dari pratinjau (area di bawahnya sampai tepi layar = masih Dock).
            let mut pt = POINT::default();
            let mut r = RECT::default();
            let keep = unsafe {
                GetCursorPos(&mut pt).is_ok() && GetWindowRect(hwnd, &mut r).is_ok() && with(|p| pt.x >= r.left - 14 && pt.x <= r.right + 14 && pt.y >= r.top - 4 && pt.y <= p.floor).unwrap_or(false)
            };
            let esc = unsafe { GetAsyncKeyState(VK_ESCAPE.0 as i32) } < 0;
            if !keep || esc {
                hide();
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
