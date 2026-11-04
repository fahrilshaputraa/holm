//! Pembungkus tipis GDI+ untuk jendela layered per-pixel alpha.
//! Menggambar langsung ke memori DIB (format PARGB) tanpa salinan.

use std::ptr::null_mut;
use std::sync::atomic::{AtomicUsize, Ordering};
use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
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

pub struct Font {
    family: *mut GpFontFamily,
    font: *mut GpFont,
}

impl Font {
    pub fn new(name: &str, px: f32, bold: bool) -> Font {
        unsafe {
            let mut family: *mut GpFontFamily = null_mut();
            for candidate in [name, "Segoe UI Symbol", "Segoe UI"] {
                let w = wide(candidate);
                family = null_mut();
                let st = GdipCreateFontFamilyFromName(PCWSTR(w.as_ptr()), null_mut(), &mut family);
                if st.0 == 0 && !family.is_null() {
                    break;
                }
            }
            let mut font: *mut GpFont = null_mut();
            let style = if bold { FontStyleBold.0 } else { FontStyleRegular.0 };
            GdipCreateFont(family, px, style, UnitPixel, &mut font);
            Font { family, font }
        }
    }
}

impl Drop for Font {
    fn drop(&mut self) {
        unsafe {
            GdipDeleteFont(self.font);
            GdipDeleteFontFamily(self.family);
        }
    }
}

pub struct Canvas {
    hdc_screen: HDC,
    hdc_mem: HDC,
    hbmp: HBITMAP,
    old: HGDIOBJ,
    bitmap: *mut GpBitmap,
    gfx: *mut GpGraphics,
    pub w: i32,
    pub h: i32,
}

impl Canvas {
    pub fn new(w: i32, h: i32) -> Option<Canvas> {
        if w <= 0 || h <= 0 {
            return None;
        }
        unsafe {
            let hdc_screen = GetDC(None);
            let hdc_mem = CreateCompatibleDC(Some(hdc_screen));
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
                    ReleaseDC(None, hdc_screen);
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

            Some(Canvas { hdc_screen, hdc_mem, hbmp, old, bitmap, gfx, w, h })
        }
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

    /// Batasi gambar berikutnya ke kotak ini (sampai `reset_clip`).
    pub fn clip(&self, x: f32, y: f32, w: f32, h: f32) {
        unsafe {
            GdipSetClipRect(self.gfx, x, y, w, h, CombineModeReplace);
        }
    }

    pub fn reset_clip(&self) {
        unsafe {
            GdipResetClip(self.gfx);
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

    fn format(align: Align) -> *mut GpStringFormat {
        let mut f: *mut GpStringFormat = null_mut();
        unsafe {
            GdipCreateStringFormat(0, 0, &mut f);
            // NoWrap | NoClip
            GdipSetStringFormatFlags(f, 0x1000 | 0x4000);
            GdipSetStringFormatTrimming(f, StringTrimmingEllipsisCharacter);
            GdipSetStringFormatLineAlign(f, StringAlignmentCenter);
            let a = match align {
                Align::Near => StringAlignmentNear,
                Align::Center => StringAlignmentCenter,
                Align::Far => StringAlignmentFar,
            };
            GdipSetStringFormatAlign(f, a);
        }
        f
    }

    /// Teks pada kotak (x, y, w, h), rata vertikal tengah.
    pub fn text(&self, s: &str, font: &Font, rect: (f32, f32, f32, f32), color: u32, align: Align) {
        if s.is_empty() {
            return;
        }
        let ws = wide(s);
        let r = RectF { X: rect.0, Y: rect.1, Width: rect.2, Height: rect.3 };
        unsafe {
            let f = Self::format(align);
            let b = self.brush(color);
            GdipDrawString(self.gfx, PCWSTR(ws.as_ptr()), (ws.len() - 1) as i32, font.font, &r, f, b as *mut _);
            GdipDeleteBrush(b as *mut _);
            GdipDeleteStringFormat(f);
        }
    }

    pub fn measure(&self, s: &str, font: &Font) -> f32 {
        if s.is_empty() {
            return 0.0;
        }
        let ws = wide(s);
        let layout = RectF { X: 0.0, Y: 0.0, Width: 10000.0, Height: 200.0 };
        let mut bounds = RectF::default();
        unsafe {
            let f = Self::format(Align::Near);
            GdipMeasureString(
                self.gfx,
                PCWSTR(ws.as_ptr()),
                (ws.len() - 1) as i32,
                font.font,
                &layout,
                f,
                &mut bounds,
                null_mut(),
                null_mut(),
            );
            GdipDeleteStringFormat(f);
        }
        bounds.Width
    }

    /// Tampilkan ke jendela layered. `pos` = posisi layar; None mempertahankan posisi saat ini.
    pub fn present(&self, hwnd: HWND, pos: Option<(i32, i32)>) {
        unsafe {
            let mut dst = pos.map(|(x, y)| POINT { x, y });
            let mut src = POINT { x: 0, y: 0 };
            let mut size = SIZE { cx: self.w, cy: self.h };
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

impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe {
            GdipDeleteGraphics(self.gfx);
            GdipDisposeImage(self.bitmap as *mut GpImage);
            SelectObject(self.hdc_mem, self.old);
            let _ = DeleteObject(self.hbmp.into());
            let _ = DeleteDC(self.hdc_mem);
            ReleaseDC(None, self.hdc_screen);
        }
    }
}

