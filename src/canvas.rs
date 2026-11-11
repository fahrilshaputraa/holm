//! Pembungkus tipis GDI+ (bentuk/gambar) + DirectWrite (teks) untuk jendela layered per-pixel alpha.
//! Menggambar langsung ke memori DIB (format PARGB) tanpa salinan; kanvas dan font di-cache.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicUsize, Ordering};
use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::GdiPlus::*;
use windows::Win32::UI::WindowsAndMessaging::*;

static GDI_TOKEN: AtomicUsize = AtomicUsize::new(0);

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn startup() {
    let input = GdiplusStartupInput {
        GdiplusVersion: 1,
        DebugEventCallback: 0,
        SuppressBackgroundThread: windows::core::BOOL(0),
        SuppressExternalCodecs: windows::core::BOOL(0),
    };
    let mut token = 0usize;
    unsafe { GdiplusStartup(&mut token, &input, null_mut()) };
    GDI_TOKEN.store(token, Ordering::Relaxed);
}

pub fn shutdown() {
    let t = GDI_TOKEN.swap(0, Ordering::Relaxed);
    if t != 0 {
        unsafe { GdiplusShutdown(t) };
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Align {
    Near,
    Center,
    Far,
}

fn config_black(a: f32) -> u32 {
    ((a.clamp(0.0, 1.0) * 255.0) as u32) << 24
}

/// Bitmap hasil decode (PNG/JPEG/...) untuk digambar di kanvas.
pub struct Image {
    bmp: *mut GpBitmap,
    _stream: Option<windows::Win32::System::Com::IStream>, // harus hidup selama bitmap dipakai
}

impl Image {
    pub fn from_bytes(bytes: &[u8]) -> Option<Image> {
        unsafe {
            let stream = windows::Win32::UI::Shell::SHCreateMemStream(Some(bytes))?;
            let mut bmp: *mut GpBitmap = null_mut();
            let st = GdipCreateBitmapFromStream(&stream, &mut bmp);
            if st.0 != 0 || bmp.is_null() {
                return None;
            }
            Some(Image { bmp, _stream: Some(stream) })
        }
    }

    /// Salin ikon Windows ke bitmap GDI+ (pemanggil tetap memiliki `icon`).
    pub fn from_hicon(icon: HICON) -> Option<Image> {
        unsafe {
            let mut bmp: *mut GpBitmap = null_mut();
            let st = GdipCreateBitmapFromHICON(icon, &mut bmp);
            if st.0 != 0 || bmp.is_null() {
                return None;
            }
            Some(Image { bmp, _stream: None })
        }
    }
}

impl Image {
    /// Salinan persegi (dipangkas di tengah) berukuran `px`, diperkecil sekali dengan kualitas tinggi.
    /// Dipakai agar gambar per-frame cukup memakai interpolasi cepat dari bitmap kecil.
    pub fn resized(&self, px: i32) -> Option<Image> {
        unsafe {
            let (mut iw, mut ih) = (0u32, 0u32);
            GdipGetImageWidth(self.bmp as *mut GpImage, &mut iw);
            GdipGetImageHeight(self.bmp as *mut GpImage, &mut ih);
            if iw == 0 || ih == 0 || px <= 0 {
                return None;
            }
            let mut dst: *mut GpBitmap = null_mut();
            if GdipCreateBitmapFromScan0(px, px, 0, 0x000E_200B, None, &mut dst).0 != 0 || dst.is_null() {
                return None;
            }
            let mut g: *mut GpGraphics = null_mut();
            GdipGetImageGraphicsContext(dst as *mut GpImage, &mut g);
            GdipSetInterpolationMode(g, InterpolationModeHighQualityBicubic);
            GdipSetPixelOffsetMode(g, PixelOffsetModeHalf);
            let side = iw.min(ih) as i32;
            let (sx, sy) = ((iw as i32 - side) / 2, (ih as i32 - side) / 2);
            GdipDrawImageRectRectI(g, self.bmp as *mut GpImage, 0, 0, px, px, sx, sy, side, side, UnitPixel, std::ptr::null(), 0, null_mut());
            GdipDeleteGraphics(g);
            Some(Image { bmp: dst, _stream: None })
        }
    }

    /// Ikon yang hampir seluruhnya gelap (dibuat untuk taskbar terang) diubah menjadi siluet putih
    /// agar terbaca di bar gelap; ikon berwarna dibiarkan. Mengembalikan true bila diubah.
    pub fn lighten_if_dark(&self) -> bool {
        unsafe {
            let (mut iw, mut ih) = (0u32, 0u32);
            GdipGetImageWidth(self.bmp as *mut GpImage, &mut iw);
            GdipGetImageHeight(self.bmp as *mut GpImage, &mut ih);
            if iw == 0 || ih == 0 || iw > 256 || ih > 256 {
                return false;
            }
            let (mut lum, mut sat, mut n) = (0u64, 0u64, 0u64);
            for y in 0..ih as i32 {
                for x in 0..iw as i32 {
                    let mut c = 0u32;
                    if GdipBitmapGetPixel(self.bmp, x, y, &mut c).0 != 0 || (c >> 24) < 96 {
                        continue;
                    }
                    let (r, g, b) = ((c >> 16) & 0xFF, (c >> 8) & 0xFF, c & 0xFF);
                    lum += ((r * 299 + g * 587 + b * 114) / 1000) as u64;
                    sat += (r.max(g).max(b) - r.min(g).min(b)) as u64;
                    n += 1;
                }
            }
            if n == 0 || lum / n > 70 || sat / n > 40 {
                return false;
            }
            for y in 0..ih as i32 {
                for x in 0..iw as i32 {
                    let mut c = 0u32;
                    if GdipBitmapGetPixel(self.bmp, x, y, &mut c).0 == 0 {
                        GdipBitmapSetPixel(self.bmp, x, y, (c & 0xFF00_0000) | 0x00F2_F2F2);
                    }
                }
            }
            true
        }
    }

    /// Warna rata-rata (sampel 8x8) yang dicerahkan, untuk mewarnai visualizer/progres.
    /// None bila gambar terlalu abu-abu/gelap untuk menghasilkan warna yang jelas.
    pub fn tint(&self) -> Option<u32> {
        unsafe {
            let (mut iw, mut ih) = (0u32, 0u32);
            GdipGetImageWidth(self.bmp as *mut GpImage, &mut iw);
            GdipGetImageHeight(self.bmp as *mut GpImage, &mut ih);
            if iw == 0 || ih == 0 {
                return None;
            }
            let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
            for i in 0..8 {
                for j in 0..8 {
                    let mut c = 0u32;
                    let (x, y) = ((iw * (2 * i + 1) / 16) as i32, (ih * (2 * j + 1) / 16) as i32);
                    if GdipBitmapGetPixel(self.bmp, x, y, &mut c).0 != 0 {
                        continue;
                    }
                    let (cr, cg, cb) = ((c >> 16) & 0xFF, (c >> 8) & 0xFF, c & 0xFF);
                    // Utamakan piksel berwarna: bobot = saturasi kasar.
                    let w = cr.max(cg).max(cb) - cr.min(cg).min(cb) + 8;
                    r += cr * w;
                    g += cg * w;
                    b += cb * w;
                    n += w;
                }
            }
            if n == 0 {
                return None;
            }
            let (r, g, b) = (r / n, g / n, b / n);
            let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
            if mx < 24 || mx - mn < 28 {
                return None;
            }
            let k = 235.0 / mx as f32;
            let f = |v: u32| ((v as f32 * k).min(255.0)) as u32;
            Some(0xFF00_0000 | (f(r) << 16) | (f(g) << 8) | f(b))
        }
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        unsafe {
            GdipDisposeImage(self.bmp as *mut GpImage);
        }
    }
}

// ---------- Teks: DirectWrite (lebih tajam dari GDI+, ber-hinting, tanpa jarak huruf aneh) ----------

thread_local! {
    static DWRITE: RefCell<Option<IDWriteFactory>> = const { RefCell::new(None) };
    static D2D: RefCell<Option<ID2D1Factory>> = const { RefCell::new(None) };
    /// Format teks per (font, ukuran, tebal): membuat format/menelusuri font tiap frame sangat mahal.
    static FONTS: RefCell<HashMap<(String, i32, bool), Font>> = RefCell::new(HashMap::new());
    /// Kanvas bekas dipakai ulang (membuat DIB + konteks GDI+ tiap frame mahal dan membuat UI tersendat).
    static POOL: RefCell<Vec<Inner>> = const { RefCell::new(Vec::new()) };
}

fn dwrite() -> Option<IDWriteFactory> {
    DWRITE.with(|d| {
        let mut d = d.borrow_mut();
        if d.is_none() {
            *d = unsafe { DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED) }.ok();
        }
        d.clone()
    })
}

fn d2d() -> Option<ID2D1Factory> {
    D2D.with(|d| {
        let mut d = d.borrow_mut();
        if d.is_none() {
            *d = unsafe { D2D1CreateFactory::<ID2D1Factory>(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) }.ok();
        }
        d.clone()
    })
}

#[derive(Clone)]
pub struct Font {
    fmt: Option<IDWriteTextFormat>,
    trim: Option<IDWriteInlineObject>,
    px: f32,
}

impl Font {
    /// Font (di-cache). `bold` memakai Semibold (600) seperti teks tebal macOS.
    pub fn new(name: &str, px: f32, bold: bool) -> Font {
        let key = (name.to_string(), (px * 16.0).round() as i32, bold);
        if let Some(f) = FONTS.with(|c| c.borrow().get(&key).cloned()) {
            return f;
        }
        let f = Self::create(name, px, bold);
        FONTS.with(|c| {
            let mut c = c.borrow_mut();
            if c.len() > 400 {
                c.clear();
            }
            c.insert(key, f.clone());
        });
        f
    }

    fn create(name: &str, px: f32, bold: bool) -> Font {
        let Some(fac) = dwrite() else { return Font { fmt: None, trim: None, px } };
        unsafe {
            let fam = wide(name);
            let loc = wide("en-us");
            let weight = if bold { DWRITE_FONT_WEIGHT_SEMI_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL };
            let fmt = fac.CreateTextFormat(PCWSTR(fam.as_ptr()), None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, px.max(1.0), PCWSTR(loc.as_ptr())).ok();
            let mut trim = None;
            if let Some(f) = &fmt {
                let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
                let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
                trim = fac.CreateEllipsisTrimmingSign(f).ok();
                let t = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
                let _ = f.SetTrimming(&t, trim.as_ref());
            }
            Font { fmt, trim, px }
        }
    }

    /// Tata letak satu teks pada kotak lebar `w`, tinggi `h`.
    fn layout(&self, s: &str, w: f32, h: f32, align: Align, wrap: bool) -> Option<IDWriteTextLayout> {
        let fmt = self.fmt.as_ref()?;
        let ws: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            let l = dwrite()?.CreateTextLayout(&ws, fmt, w.max(1.0), h.max(1.0)).ok()?;
            let _ = l.SetTextAlignment(match align {
                Align::Near => DWRITE_TEXT_ALIGNMENT_LEADING,
                Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
                Align::Far => DWRITE_TEXT_ALIGNMENT_TRAILING,
            });
            if wrap {
                let _ = l.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);
                let _ = l.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR);
                // Baris terakhir yang muat diberi elipsis.
                let t = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_WORD, delimiter: 0, delimiterCount: 0 };
                let _ = l.SetTrimming(&t, self.trim.as_ref());
            }
            Some(l)
        }
    }
}

struct Inner {
    hdc_mem: HDC,
    hbmp: HBITMAP,
    old: HGDIOBJ,
    bitmap: *mut GpBitmap,
    gfx: *mut GpGraphics,
    w: i32,
    h: i32,
    rt: RefCell<Option<(ID2D1DCRenderTarget, ID2D1SolidColorBrush)>>,
}

impl Inner {
    fn free(self) {
        unsafe {
            GdipDeleteGraphics(self.gfx);
            GdipDisposeImage(self.bitmap as *mut GpImage);
            SelectObject(self.hdc_mem, self.old);
            let _ = DeleteObject(self.hbmp.into());
            let _ = DeleteDC(self.hdc_mem);
        }
    }
}

pub struct Canvas {
    hdc_screen: HDC,
    hdc_mem: HDC,
    gfx: *mut GpGraphics,
    inner: Option<Inner>,
    clip: Cell<Option<(f32, f32, f32, f32)>>,
    pub w: i32,
    pub h: i32,
}

impl Canvas {
    pub fn new(w: i32, h: i32) -> Option<Canvas> {
        if w <= 0 || h <= 0 {
            return None;
        }
        let inner = POOL.with(|p| {
            let mut p = p.borrow_mut();
            p.iter().position(|i| i.w == w && i.h == h).map(|i| p.swap_remove(i))
        });
        let inner = match inner {
            Some(i) => {
                unsafe {
                    GdipResetClip(i.gfx);
                    GdipGraphicsClear(i.gfx, 0);
                }
                i
            }
            None => Self::alloc(w, h)?,
        };
        let hdc_screen = unsafe { GetDC(None) };
        Some(Canvas { hdc_screen, hdc_mem: inner.hdc_mem, gfx: inner.gfx, inner: Some(inner), clip: Cell::new(None), w, h })
    }

    fn alloc(w: i32, h: i32) -> Option<Inner> {
        unsafe {
            let hdc_screen = GetDC(None);
            let hdc_mem = CreateCompatibleDC(Some(hdc_screen));
            ReleaseDC(None, hdc_screen);
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut std::ffi::c_void = null_mut();
            let hbmp = match CreateDIBSection(Some(hdc_mem), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
                std::result::Result::Ok(b) => b,
                Err(_) => {
                    let _ = DeleteDC(hdc_mem);
                    return None;
                }
            };
            let old = SelectObject(hdc_mem, hbmp.into());

            // 0x000E200B = PixelFormat32bppPARGB
            let mut bitmap: *mut GpBitmap = null_mut();
            GdipCreateBitmapFromScan0(w, h, w * 4, 0x000E_200B, Some(bits as *const u8), &mut bitmap);
            let mut gfx: *mut GpGraphics = null_mut();
            GdipGetImageGraphicsContext(bitmap as *mut GpImage, &mut gfx);
            GdipSetSmoothingMode(gfx, SmoothingModeAntiAlias);
            GdipSetTextRenderingHint(gfx, TextRenderingHintAntiAliasGridFit);

            Some(Inner { hdc_mem, hbmp, old, bitmap, gfx, w, h, rt: RefCell::new(None) })
        }
    }

    /// Render target Direct2D yang menggambar ke DIB yang sama (dibuat sekali per kanvas).
    fn rt(&self) -> Option<(ID2D1DCRenderTarget, ID2D1SolidColorBrush)> {
        let inner = self.inner.as_ref()?;
        if inner.rt.borrow().is_none() {
            unsafe {
                let props = D2D1_RENDER_TARGET_PROPERTIES {
                    r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                    pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                    dpiX: 96.0,
                    dpiY: 96.0,
                    usage: D2D1_RENDER_TARGET_USAGE_NONE,
                    minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
                };
                let rt = d2d()?.CreateDCRenderTarget(&props).ok()?;
                rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
                if let Some(p) = dwrite().and_then(|f| f.CreateCustomRenderingParams(1.8, 0.6, 0.0, DWRITE_PIXEL_GEOMETRY_FLAT, DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC).ok()) {
                    rt.SetTextRenderingParams(&p);
                }
                let brush = rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 }, None).ok()?;
                *inner.rt.borrow_mut() = Some((rt, brush));
            }
        }
        inner.rt.borrow().clone()
    }

    fn brush(&self, color: u32) -> *mut GpSolidFill {
        let mut b: *mut GpSolidFill = null_mut();
        unsafe { GdipCreateSolidFill(color, &mut b) };
        b
    }

    pub fn fill_rect(&self, x: f32, y: f32, w: f32, h: f32, color: u32) {
        unsafe {
            let b = self.brush(color);
            GdipFillRectangle(self.gfx, b as *mut _, x, y, w, h);
            GdipDeleteBrush(b as *mut _);
        }
    }

    pub fn fill_ellipse(&self, x: f32, y: f32, w: f32, h: f32, color: u32) {
        unsafe {
            let b = self.brush(color);
            GdipFillEllipse(self.gfx, b as *mut _, x, y, w, h);
            GdipDeleteBrush(b as *mut _);
        }
    }

    pub fn fill_polygon(&self, pts: &[(f32, f32)], color: u32) {
        let p: Vec<PointF> = pts.iter().map(|&(x, y)| PointF { X: x, Y: y }).collect();
        unsafe {
            let b = self.brush(color);
            GdipFillPolygon(self.gfx, b as *mut _, p.as_ptr(), p.len() as i32, FillModeAlternate);
            GdipDeleteBrush(b as *mut _);
        }
    }

    fn rr_path(x: f32, y: f32, w: f32, h: f32, r: (f32, f32, f32, f32)) -> *mut GpPath {
        let (tl, tr, br, bl) = r;
        unsafe {
            let mut p: *mut GpPath = null_mut();
            GdipCreatePath(FillModeAlternate, &mut p);
            GdipAddPathLine(p, x + tl, y, x + w - tr, y);
            if tr > 0.0 {
                GdipAddPathArc(p, x + w - 2.0 * tr, y, 2.0 * tr, 2.0 * tr, 270.0, 90.0);
            }
            GdipAddPathLine(p, x + w, y + tr, x + w, y + h - br);
            if br > 0.0 {
                GdipAddPathArc(p, x + w - 2.0 * br, y + h - 2.0 * br, 2.0 * br, 2.0 * br, 0.0, 90.0);
            }
            GdipAddPathLine(p, x + w - br, y + h, x + bl, y + h);
            if bl > 0.0 {
                GdipAddPathArc(p, x, y + h - 2.0 * bl, 2.0 * bl, 2.0 * bl, 90.0, 90.0);
            }
            GdipAddPathLine(p, x, y + h - bl, x, y + tl);
            if tl > 0.0 {
                GdipAddPathArc(p, x, y, 2.0 * tl, 2.0 * tl, 180.0, 90.0);
            }
            GdipClosePathFigure(p);
            p
        }
    }

    fn fill_path(&self, p: *mut GpPath, color: u32) {
        unsafe {
            let b = self.brush(color);
            GdipFillPath(self.gfx, b as *mut _, p);
            GdipDeleteBrush(b as *mut _);
            GdipDeletePath(p);
        }
    }

    /// Busur lingkaran berpusat (cx, cy) dengan ujung membulat. Sudut dalam derajat, 0 = arah jam 3.
    pub fn stroke_arc(&self, cx: f32, cy: f32, r: f32, start: f32, sweep: f32, width: f32, color: u32) {
        if sweep.abs() < 0.1 {
            return;
        }
        unsafe {
            let mut pen: *mut GpPen = null_mut();
            GdipCreatePen1(color, width, UnitPixel, &mut pen);
            GdipSetPenStartCap(pen, LineCapRound);
            GdipSetPenEndCap(pen, LineCapRound);
            GdipDrawArc(self.gfx, pen, cx - r, cy - r, 2.0 * r, 2.0 * r, start, sweep);
            GdipDeletePen(pen);
        }
    }

    /// Hapus seluruh kanvas menjadi transparan (kanvas dipakai ulang antar frame).
    pub fn clear(&self) {
        unsafe {
            GdipGraphicsClear(self.gfx, 0);
        }
    }

    /// Gambar bitmap kecil yang sudah dipangkas persegi tanpa clip: jalur cepat untuk animasi.
    pub fn draw_image_fast(&self, img: &Image, x: f32, y: f32, w: f32, h: f32) {
        unsafe {
            GdipSetInterpolationMode(self.gfx, InterpolationModeHighQualityBilinear);
            GdipSetPixelOffsetMode(self.gfx, PixelOffsetModeHalf);
            GdipDrawImageRect(self.gfx, img.bmp as *mut GpImage, x, y, w, h);
        }
    }

    /// Batasi gambar berikutnya ke kotak ini (sampai `reset_clip`).
    pub fn clip(&self, x: f32, y: f32, w: f32, h: f32) {
        self.clip.set(Some((x, y, w, h)));
        unsafe {
            GdipSetClipRect(self.gfx, x, y, w, h, CombineModeReplace);
        }
    }

    pub fn reset_clip(&self) {
        self.clip.set(None);
        unsafe {
            GdipResetClip(self.gfx);
        }
    }

    /// Persegi membulat berisi gradien linear `c0` -> `c1` (mendatar atau tegak).
    #[allow(dead_code)]
    pub fn fill_gradient(&self, x: f32, y: f32, w: f32, h: f32, r: (f32, f32, f32, f32), c0: u32, c1: u32, horizontal: bool) {
        unsafe {
            let rect = RectF { X: x - 0.5, Y: y - 0.5, Width: w + 1.0, Height: h + 1.0 };
            let mut b: *mut GpLineGradient = null_mut();
            let mode = if horizontal { LinearGradientModeHorizontal } else { LinearGradientModeVertical };
            GdipCreateLineBrushFromRect(&rect, c0, c1, mode, WrapModeTileFlipXY, &mut b);
            let p = Self::rr_path(x, y, w, h, r);
            GdipFillPath(self.gfx, b as *mut _, p);
            GdipDeletePath(p);
            GdipDeleteBrush(b as *mut _);
        }
    }

    /// Garis bersambung berujung dan bersudut bulat melalui titik-titik `pts`.
    pub fn polyline(&self, pts: &[(f32, f32)], width: f32, color: u32) {
        if pts.len() < 2 {
            return;
        }
        let p: Vec<PointF> = pts.iter().map(|&(x, y)| PointF { X: x, Y: y }).collect();
        unsafe {
            let mut pen: *mut GpPen = null_mut();
            GdipCreatePen1(color, width, UnitPixel, &mut pen);
            GdipSetPenStartCap(pen, LineCapRound);
            GdipSetPenEndCap(pen, LineCapRound);
            GdipSetPenLineJoin(pen, LineJoinRound);
            GdipDrawLines(self.gfx, pen, p.as_ptr(), p.len() as i32);
            GdipDeletePen(pen);
        }
    }

    /// Garis tebal berujung bulat.
    pub fn line(&self, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, color: u32) {
        unsafe {
            let mut pen: *mut GpPen = null_mut();
            GdipCreatePen1(color, width, UnitPixel, &mut pen);
            GdipSetPenStartCap(pen, LineCapRound);
            GdipSetPenEndCap(pen, LineCapRound);
            GdipDrawLine(self.gfx, pen, x0, y0, x1, y1);
            GdipDeletePen(pen);
        }
    }

    /// Persegi panjang dengan radius sudut berbeda: (kiri-atas, kanan-atas, kanan-bawah, kiri-bawah).
    pub fn fill_round_rect(&self, x: f32, y: f32, w: f32, h: f32, r: (f32, f32, f32, f32), color: u32) {
        self.fill_path(Self::rr_path(x, y, w, h, r), color);
    }

    /// Bentuk notch MacBook: menempel di tepi atas, "telinga" cekung `ear` di kiri/kanan atas
    /// yang menyatu dengan menu bar, dan sudut bawah cembung `br`. Lebar jendela = badan + 2*ear.
    pub fn fill_notch(&self, w: f32, h: f32, ear: f32, br: f32, color: u32) {
        unsafe {
            let mut p: *mut GpPath = null_mut();
            GdipCreatePath(FillModeAlternate, &mut p);
            let (e, r) = (ear.min(h / 2.0), br.min(h / 2.0).max(0.0));
            GdipAddPathLine(p, 0.0, 0.0, w, 0.0);
            // telinga kanan: pusat (w, e), dari atas ke kiri
            GdipAddPathArc(p, w - e, 0.0 - 0.0, 2.0 * e, 2.0 * e, 270.0, -90.0);
            GdipAddPathLine(p, w - e, e, w - e, h - r);
            GdipAddPathArc(p, w - e - 2.0 * r, h - 2.0 * r, 2.0 * r, 2.0 * r, 0.0, 90.0);
            GdipAddPathLine(p, w - e - r, h, e + r, h);
            GdipAddPathArc(p, e, h - 2.0 * r, 2.0 * r, 2.0 * r, 90.0, 90.0);
            GdipAddPathLine(p, e, h - r, e, e);
            // telinga kiri: pusat (0, e), dari kanan ke atas
            GdipAddPathArc(p, -e, 0.0, 2.0 * e, 2.0 * e, 0.0, -90.0);
            GdipClosePathFigure(p);
            self.fill_path(p, color);
        }
    }

    /// Gambar `img` pada kotak (x, y, w, h) dengan sudut membulat `radius`.
    pub fn draw_image(&self, img: &Image, x: f32, y: f32, w: f32, h: f32, radius: f32, alpha: f32) {
        unsafe {
            let (mut iw, mut ih) = (0u32, 0u32);
            GdipGetImageWidth(img.bmp as *mut GpImage, &mut iw);
            GdipGetImageHeight(img.bmp as *mut GpImage, &mut ih);
            if iw == 0 || ih == 0 {
                return;
            }
            // Pangkas persegi di tengah gambar (sampul biasanya 1:1, tapi tidak selalu).
            let side = iw.min(ih) as i32;
            let (sx, sy) = ((iw as i32 - side) / 2, (ih as i32 - side) / 2);
            let clip = Self::rr_path(x, y, w, h, (radius, radius, radius, radius));
            GdipSetClipPath(self.gfx, clip, CombineModeReplace);
            GdipDeletePath(clip);
            GdipSetInterpolationMode(self.gfx, InterpolationModeHighQualityBicubic);
            GdipDrawImageRectRectI(
                self.gfx,
                img.bmp as *mut GpImage,
                x.round() as i32,
                y.round() as i32,
                w.round() as i32,
                h.round() as i32,
                sx,
                sy,
                side,
                side,
                UnitPixel,
                std::ptr::null(),
                0,
                null_mut(),
            );
            GdipResetClip(self.gfx);
            if alpha < 1.0 {
                // Peredupan (fade) dengan menimpa warna latar notch di atas gambar.
                self.fill_round_rect(x, y, w, h, (radius, radius, radius, radius), config_black(1.0 - alpha));
            }
        }
    }

    /// Teks pada kotak (x, y, w, h), rata vertikal tengah, satu baris dengan elipsis.
    pub fn text(&self, s: &str, font: &Font, rect: (f32, f32, f32, f32), color: u32, align: Align) {
        if s.is_empty() || color >> 24 == 0 {
            return;
        }
        if let Some(l) = font.layout(s, rect.2, rect.3, align, false) {
            self.draw_layout(&l, font, rect, color);
        }
    }

    #[allow(dead_code)]
    /// Teks beberapa baris (dibungkus per kata) mulai dari atas kotak; baris terakhir diberi elipsis.
    pub fn text_wrap(&self, s: &str, font: &Font, rect: (f32, f32, f32, f32), color: u32, align: Align) {
        if s.is_empty() || color >> 24 == 0 {
            return;
        }
        if let Some(l) = font.layout(s, rect.2, rect.3, align, true) {
            self.draw_layout(&l, font, rect, color);
        }
    }

    fn draw_layout(&self, l: &IDWriteTextLayout, font: &Font, rect: (f32, f32, f32, f32), color: u32) {
        let Some((rt, brush)) = self.rt() else { return };
        // Kotak yang di-bind sedikit lebih besar dari kotak teks (huruf boleh sedikit menjorok),
        // dipotong ke kanvas dan ke clip aktif.
        let m = (font.px * 0.45).max(2.0);
        let (mut l0, mut t0, mut r0, mut b0) = (rect.0 - m, rect.1 - m, rect.0 + rect.2 + m, rect.1 + rect.3 + m);
        if let Some((cx, cy, cw, ch)) = self.clip.get() {
            l0 = l0.max(cx);
            t0 = t0.max(cy);
            r0 = r0.min(cx + cw);
            b0 = b0.min(cy + ch);
        }
        let (bl, bt) = (l0.max(0.0).floor() as i32, t0.max(0.0).floor() as i32);
        let (br, bb) = (r0.min(self.w as f32).ceil() as i32, b0.min(self.h as f32).ceil() as i32);
        if br <= bl || bb <= bt {
            return;
        }
        let a = (color >> 24) as f32 / 255.0;
        let ch = |sh: u32| ((color >> sh) & 0xFF) as f32 / 255.0;
        unsafe {
            let _ = GdiFlush();
            if rt.BindDC(self.hdc_mem, &RECT { left: bl, top: bt, right: br, bottom: bb }).is_err() {
                return;
            }
            rt.BeginDraw();
            brush.SetColor(&D2D1_COLOR_F { r: ch(16), g: ch(8), b: ch(0), a });
            let origin = windows_numerics::Vector2 { X: rect.0 - bl as f32, Y: rect.1 - bt as f32 };
            rt.DrawTextLayout(origin, l, &brush, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
            let _ = rt.EndDraw(None, None);
        }
    }

    pub fn measure(&self, s: &str, font: &Font) -> f32 {
        measure(s, font)
    }

    /// Tampilkan ke jendela layered. `pos` = posisi layar; None mempertahankan posisi saat ini.
    pub fn present(&self, hwnd: HWND, pos: Option<(i32, i32)>) {
        self.present_size(hwnd, pos, self.w, self.h);
    }

    /// Seperti `present`, tetapi hanya sudut kiri-atas `w`×`h` dari kanvas (kanvas besar dipakai ulang
    /// untuk jendela yang ukurannya berubah tiap frame, mis. notch saat beranimasi).
    pub fn present_size(&self, hwnd: HWND, pos: Option<(i32, i32)>, w: i32, h: i32) {
        unsafe {
            let _ = GdiFlush();
            if let Some(path) = CAPTURE.with(|c| c.borrow_mut().take()) {
                self.save_png(&path, w.clamp(1, self.w), h.clamp(1, self.h));
            }
            let mut dst = pos.map(|(x, y)| POINT { x, y });
            let mut src = POINT { x: 0, y: 0 };
            let mut size = SIZE { cx: w.clamp(1, self.w), cy: h.clamp(1, self.h) };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let _ = UpdateLayeredWindow(
                hwnd,
                Some(self.hdc_screen),
                dst.as_mut().map(|p| p as *mut POINT as *const POINT),
                Some(&mut size),
                Some(self.hdc_mem),
                Some(&mut src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
        }
    }
}

thread_local! {
    static CAPTURE: RefCell<Option<std::path::PathBuf>> = const { RefCell::new(None) };
}

/// `present` berikutnya di thread ini juga menyimpan isi kanvas ke file PNG (dipakai `--shots`).
pub fn capture_next(path: std::path::PathBuf) {
    CAPTURE.with(|c| *c.borrow_mut() = Some(path));
}

pub fn capture_cancel() {
    CAPTURE.with(|c| *c.borrow_mut() = None);
}

impl Canvas {
    /// Salin sudut kiri-atas `w`×`h` kanvas ke jendela biasa (bukan layered), mis. di WM_PAINT.
    pub fn blit(&self, hdc: HDC, w: i32, h: i32) {
        unsafe {
            let _ = GdiFlush();
            let _ = BitBlt(hdc, 0, 0, w.min(self.w), h.min(self.h), Some(self.hdc_mem), 0, 0, SRCCOPY);
        }
    }

    /// Simpan sudut kiri-atas `w`×`h` kanvas sebagai PNG beralpha.
    pub fn save_png(&self, path: &std::path::Path, w: i32, h: i32) {
        let Some(inner) = self.inner.as_ref() else { return };
        // CLSID encoder PNG GDI+.
        let png = windows::core::GUID::from_u128(0x557CF406_1A04_11D3_9A73_0000F81EF32E);
        let file = wide(&path.to_string_lossy());
        unsafe {
            let mut part: *mut GpBitmap = null_mut();
            // 0x0026200A = PixelFormat32bppARGB (alpha lurus, sesuai PNG).
            if GdipCloneBitmapAreaI(0, 0, w, h, 0x0026_200A, inner.bitmap, &mut part).0 == 0 && !part.is_null() {
                GdipSaveImageToFile(part as *mut GpImage, PCWSTR(file.as_ptr()), &png, std::ptr::null());
                GdipDisposeImage(part as *mut GpImage);
            }
        }
    }
}

/// Lebar teks satu baris (piksel) tanpa memerlukan kanvas.
pub fn measure(s: &str, font: &Font) -> f32 {
    if s.is_empty() {
        return 0.0;
    }
    let Some(l) = font.layout(s, 100_000.0, 1000.0, Align::Near, false) else { return 0.0 };
    let mut m = DWRITE_TEXT_METRICS::default();
    unsafe {
        if l.GetMetrics(&mut m).is_err() {
            return 0.0;
        }
    }
    m.widthIncludingTrailingWhitespace
}

impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe {
            ReleaseDC(None, self.hdc_screen);
        }
        if let Some(inner) = self.inner.take() {
            // Simpan untuk dipakai ulang; kanvas besar (> 8 MB) atau kolam penuh dibebaskan.
            let big = inner.w as i64 * inner.h as i64 * 4 > 8 << 20;
            let spill = POOL.with(|p| {
                let mut p = p.borrow_mut();
                if big {
                    return Some(inner);
                }
                p.push(inner);
                if p.len() > 10 {
                    Some(p.remove(0))
                } else {
                    None
                }
            });
            if let Some(i) = spill {
                i.free();
            }
        }
    }
}
