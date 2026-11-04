//! Ikon vektor bergaya filled (solid), digambar langsung dengan `Canvas`.
//! Font ikon Windows (Segoe Fluent/MDL2) kebanyakan outline, jadi semua ikon UI HolmBar
//! digambar di sini dalam ruang desain 24×24 yang diskalakan ke kotak tujuan.
//! `bg` = warna latar di bawah ikon, dipakai untuk potongan (lubang gir, jarum stopwatch, dll.).

use crate::canvas::Canvas;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Icon {
    Music,
    Calendar,
    Stopwatch,
    Chart,
    Gear,
    Play,
    Pause,
    Prev,
    Next,
    Reset,
    ChevronLeft,
    ChevronRight,
    ChevronDown,
    ChevronUp,
    Monitor,
    Apps,
    Bell,
    Document,
    Check,
    Warning,
    Close,
    /// Baterai dengan tingkat isi 0–100.
    Battery(u8),
    Cpu,
}

struct P<'a> {
    c: &'a Canvas,
    x: f32,
    y: f32,
    u: f32,
}

impl P<'_> {
    fn pt(&self, x: f32, y: f32) -> (f32, f32) {
        (self.x + x * self.u, self.y + y * self.u)
    }
    fn rect(&self, x: f32, y: f32, w: f32, h: f32, r: f32, col: u32) {
        let (px, py) = self.pt(x, y);
        let r = r * self.u;
        self.c.fill_round_rect(px, py, w * self.u, h * self.u, (r, r, r, r), col);
    }
    fn rect4(&self, x: f32, y: f32, w: f32, h: f32, r: (f32, f32, f32, f32), col: u32) {
        let (px, py) = self.pt(x, y);
        let u = self.u;
        self.c.fill_round_rect(px, py, w * u, h * u, (r.0 * u, r.1 * u, r.2 * u, r.3 * u), col);
    }
    fn circle(&self, cx: f32, cy: f32, r: f32, col: u32) {
        let (px, py) = self.pt(cx - r, cy - r);
        self.c.fill_ellipse(px, py, 2.0 * r * self.u, 2.0 * r * self.u, col);
    }
    fn poly(&self, pts: &[(f32, f32)], col: u32) {
        let v: Vec<(f32, f32)> = pts.iter().map(|&(x, y)| self.pt(x, y)).collect();
        self.c.fill_polygon(&v, col);
    }
    /// Batang tebal dari (x0,y0) ke (x1,y1) sebagai poligon.
    fn bar(&self, x0: f32, y0: f32, x1: f32, y1: f32, w: f32, col: u32) {
        let (dx, dy) = (x1 - x0, y1 - y0);
        let len = (dx * dx + dy * dy).sqrt().max(0.001);
        let (nx, ny) = (-dy / len * w / 2.0, dx / len * w / 2.0);
        self.poly(&[(x0 + nx, y0 + ny), (x1 + nx, y1 + ny), (x1 - nx, y1 - ny), (x0 - nx, y0 - ny)], col);
        self.circle(x0, y0, w / 2.0, col);
        self.circle(x1, y1, w / 2.0, col);
    }
}

/// Gambar `icon` di tengah kotak `r` (x, y, w, h); ukuran ikon = sisi terpendek kotak.
pub fn draw(c: &Canvas, icon: Icon, r: (f32, f32, f32, f32), col: u32, bg: u32) {
    let side = r.2.min(r.3);
    let p = P { c, x: r.0 + (r.2 - side) / 2.0, y: r.1 + (r.3 - side) / 2.0, u: side / 24.0 };
    match icon {
        Icon::Play => p.poly(&[(7.0, 4.0), (7.0, 20.0), (20.0, 12.0)], col),
        Icon::Pause => {
            p.rect(5.5, 4.0, 4.8, 16.0, 1.4, col);
            p.rect(13.7, 4.0, 4.8, 16.0, 1.4, col);
        }
        Icon::Prev => {
            p.rect(3.5, 5.0, 2.8, 14.0, 1.2, col);
            p.poly(&[(20.5, 5.0), (20.5, 19.0), (7.5, 12.0)], col);
        }
        Icon::Next => {
            p.rect(17.7, 5.0, 2.8, 14.0, 1.2, col);
            p.poly(&[(3.5, 5.0), (3.5, 19.0), (16.5, 12.0)], col);
        }
        Icon::Music => {
            p.circle(6.5, 17.5, 3.6, col);
            p.circle(17.0, 15.5, 3.6, col);
            p.rect(8.3, 5.5, 1.9, 12.5, 0.5, col);
            p.rect(18.8, 3.5, 1.9, 12.5, 0.5, col);
            p.poly(&[(8.3, 5.5), (20.7, 3.0), (20.7, 6.6), (8.3, 9.1)], col);
        }
        Icon::Calendar => {
            p.rect(3.0, 4.5, 18.0, 16.5, 3.5, col);
            p.rect(3.0, 9.0, 18.0, 1.6, 0.0, bg);
            p.rect(7.0, 2.5, 2.4, 4.6, 1.2, col);
            p.rect(14.6, 2.5, 2.4, 4.6, 1.2, col);
            for (i, j) in [(0, 0), (1, 0), (2, 0), (0, 1), (1, 1)] {
                p.rect(6.4 + i as f32 * 4.3, 12.4 + j as f32 * 3.9, 2.6, 2.4, 0.6, bg);
            }
        }
        Icon::Stopwatch => {
            p.circle(12.0, 13.5, 8.3, col);
            p.rect(9.8, 1.6, 4.4, 2.4, 1.0, col);
            p.rect(11.1, 3.0, 1.8, 3.0, 0.0, col);
            p.bar(18.0, 6.3, 19.6, 4.7, 1.8, col);
            p.bar(12.0, 13.5, 12.0, 8.5, 1.8, bg);
            p.circle(12.0, 13.5, 1.6, bg);
        }
        Icon::Chart => {
            p.rect(3.5, 12.5, 4.6, 8.5, 1.4, col);
            p.rect(9.7, 7.5, 4.6, 13.5, 1.4, col);
            p.rect(15.9, 3.0, 4.6, 18.0, 1.4, col);
        }
        Icon::Cpu => {
            p.rect(5.5, 5.5, 13.0, 13.0, 2.5, col);
            p.rect(9.0, 9.0, 6.0, 6.0, 1.2, bg);
            for i in 0..3 {
                let o = 8.0 + i as f32 * 3.5;
                p.rect(o, 2.0, 1.6, 3.0, 0.5, col);
                p.rect(o, 19.0, 1.6, 3.0, 0.5, col);
                p.rect(2.0, o, 3.0, 1.6, 0.5, col);
                p.rect(19.0, o, 3.0, 1.6, 0.5, col);
            }
        }
        Icon::Gear => {
            for k in 0..8 {
                let a = k as f32 * std::f32::consts::PI / 4.0;
                let (ca, sa) = (a.cos(), a.sin());
                let (r0, r1, hw) = (6.0, 10.3, 2.0);
                let q = |r: f32, t: f32| (12.0 + ca * r - sa * t, 12.0 + sa * r + ca * t);
                p.poly(&[q(r0, -hw), q(r1, -hw * 0.8), q(r1, hw * 0.8), q(r0, hw)], col);
            }
            p.circle(12.0, 12.0, 7.6, col);
            p.circle(12.0, 12.0, 3.0, bg);
        }
        Icon::Reset => {
            let (cx, cy, rr) = (12.0, 12.6, 7.0);
            let (px, py) = p.pt(cx, cy);
            c.stroke_arc(px, py, rr * p.u, -80.0, 300.0, 2.8 * p.u, col);
            p.poly(&[(10.0, 1.6), (10.0, 9.4), (16.2, 5.6)], col);
        }
        Icon::ChevronLeft => p.poly(&[(15.5, 3.5), (18.0, 6.0), (12.0, 12.0), (18.0, 18.0), (15.5, 20.5), (7.0, 12.0)], col),
        Icon::ChevronRight => p.poly(&[(8.5, 3.5), (6.0, 6.0), (12.0, 12.0), (6.0, 18.0), (8.5, 20.5), (17.0, 12.0)], col),
        Icon::ChevronUp => p.poly(&[(3.5, 15.5), (6.0, 18.0), (12.0, 12.0), (18.0, 18.0), (20.5, 15.5), (12.0, 7.0)], col),
        Icon::ChevronDown => p.poly(&[(3.5, 8.5), (6.0, 6.0), (12.0, 12.0), (18.0, 6.0), (20.5, 8.5), (12.0, 17.0)], col),
        Icon::Monitor => {
            p.rect(2.0, 3.5, 20.0, 13.5, 2.5, col);
            p.poly(&[(9.0, 16.5), (15.0, 16.5), (16.0, 20.0), (8.0, 20.0)], col);
            p.rect(6.5, 19.5, 11.0, 2.0, 1.0, col);
        }
        Icon::Apps => {
            for (x, y) in [(3.0, 3.0), (13.0, 3.0), (3.0, 13.0), (13.0, 13.0)] {
                p.rect(x, y, 8.0, 8.0, 2.4, col);
            }
        }
        Icon::Bell => {
            p.rect4(5.5, 3.5, 13.0, 14.0, (6.5, 6.5, 1.0, 1.0), col);
            p.rect(3.0, 15.5, 18.0, 3.0, 1.5, col);
            p.circle(12.0, 20.0, 2.4, col);
            p.rect(10.8, 1.5, 2.4, 3.0, 1.2, col);
        }
        Icon::Document => {
            p.poly(&[(5.0, 2.0), (14.0, 2.0), (19.5, 7.5), (19.5, 22.0), (5.0, 22.0)], col);
            p.poly(&[(14.0, 2.0), (14.0, 7.5), (19.5, 7.5)], bg);
            p.poly(&[(14.8, 2.8), (14.8, 6.7), (18.7, 6.7)], col);
            for (i, w) in [8.0f32, 8.0, 5.0].iter().enumerate() {
                p.rect(8.0, 11.0 + i as f32 * 3.5, *w, 1.7, 0.8, bg);
            }
        }
        Icon::Check => {
            p.circle(12.0, 12.0, 10.0, col);
            p.bar(7.4, 12.4, 10.6, 15.6, 2.4, bg);
            p.bar(10.6, 15.6, 16.8, 8.8, 2.4, bg);
        }
        Icon::Warning => {
            p.circle(12.0, 12.0, 10.0, col);
            p.rect(10.8, 6.0, 2.4, 8.0, 1.2, bg);
            p.circle(12.0, 17.2, 1.5, bg);
        }
        Icon::Close => {
            p.bar(6.0, 6.0, 18.0, 18.0, 2.8, col);
            p.bar(18.0, 6.0, 6.0, 18.0, 2.8, col);
        }
        Icon::Battery(pct) => {
            p.rect(1.5, 6.5, 19.0, 11.0, 3.0, crate::config::with_alpha(col, 0.35 * ((col >> 24) as f32 / 255.0)));
            let w = 15.0 * (pct.min(100) as f32 / 100.0);
            if w > 0.5 {
                p.rect(3.5, 8.5, w.max(2.0), 7.0, 1.6, col);
            }
            p.rect(21.2, 9.8, 2.0, 4.4, 1.0, col);
        }
    }
}
