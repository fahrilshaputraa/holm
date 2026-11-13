"""Membangun logo HolmBar: ikon aplikasi (SVG master + versi ukuran kecil), glyph satu warna, PNG, dan res/holmbar.ico.

Ide: layar mini berisi tiga bagian aplikasi (menu bar, notch, Dock). Notch hitam bergantung dari tepi atas dengan dua
mata putih (wajah idle notch), Dock kaca di bawah. Satu keluarga warna, kedalaman dari gradien dan kilau.
Jalankan ulang untuk menghasilkan semuanya:

    py -3 tools/make-logo.py

PNG dirender dengan render_png.py milik skill logo-design (butuh salah satu perender: Chrome, rsvg, ...).
"""
import math
import os
import struct
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "docs", "brand")
RENDER = os.path.join(os.path.expanduser("~"), ".claude", "skills", "logo-design", "scripts", "render_png.py")


def rr(x, y, w, h, r):
    return f"M{x+r} {y}h{w-2*r}a{r} {r} 0 0 1 {r} {r}v{h-2*r}a{r} {r} 0 0 1-{r} {r}h-{w-2*r}a{r} {r} 0 0 1-{r}-{r}v-{h-2*r}a{r} {r} 0 0 1 {r}-{r}z"


def squircle(cx, cy, a, n=5.0, steps=160):
    """Superellipse |x/a|^n + |y/a|^n = 1: lengkung sudut halus ala ikon Big Sur (bukan persegi + busur)."""
    pts = []
    for i in range(steps):
        t = 2 * math.pi * i / steps
        c, s = math.cos(t), math.sin(t)
        x = cx + a * math.copysign(abs(c) ** (2 / n), c)
        y = cy + a * math.copysign(abs(s) ** (2 / n), s)
        pts.append(f"{x:.2f} {y:.2f}")
    return "M" + "L".join(pts) + "Z"


TILE = squircle(128, 128, 116)

# Notch: menempel di tepi atas (y = 12), telinga cekung di kiri-kanan atas, sudut bawah membulat.
NOTCH = "M60 12H196Q180 12 180 28V54Q180 82 152 82H104Q76 82 76 54V28Q76 12 60 12Z"
NOTCH_S = "M52 12H204Q184 12 184 32V60Q184 90 154 90H102Q72 90 72 60V32Q72 12 52 12Z"  # versi kecil: lebih besar


def ell(cx, cy, rx, ry):
    return f"M{cx-rx} {cy}a{rx} {ry} 0 1 0 {2*rx} 0a{rx} {ry} 0 1 0-{2*rx} 0z"


EYES = ell(112, 52, 6.5, 10.5) + ell(144, 52, 6.5, 10.5)
EYES_S = ell(110, 58, 9, 14) + ell(146, 58, 9, 14)
DOCK = rr(48, 176, 160, 48, 24)
APPS = [rr(72, 187, 26, 26, 8), rr(112, 184, 32, 32, 10), rr(158, 187, 26, 26, 8)]
DOCK_S = rr(40, 168, 176, 60, 30)
APPS_S = [rr(64, 182, 32, 32, 9), rr(108, 178, 40, 40, 12), rr(160, 182, 32, 32, 9)]

DEFS = (
    '<defs>'
    '<linearGradient id="bg" x1="0.12" y1="0" x2="0.88" y2="1"><stop offset="0" stop-color="#6CB8FF"/><stop offset="1" stop-color="#3540E6"/></linearGradient>'
    '<linearGradient id="gloss" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#fff" stop-opacity=".34"/><stop offset=".5" stop-color="#fff" stop-opacity="0"/></linearGradient>'
    '<linearGradient id="nt" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#202028"/><stop offset="1" stop-color="#050507"/></linearGradient>'
    '<linearGradient id="gl" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#fff" stop-opacity=".62"/><stop offset="1" stop-color="#fff" stop-opacity=".3"/></linearGradient>'
    f'<clipPath id="tile"><path d="{TILE}"/></clipPath>'
    '</defs>'
)


def svg(body, title, defs=DEFS):
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256" role="img" aria-label="{title}">'
            f'<title>{title}</title>{defs}{body}</svg>\n')


def icon_body(small=False):
    b = f'<path fill="url(#bg)" d="{TILE}"/><path fill="url(#gloss)" d="{TILE}"/>'
    clip = '<g clip-path="url(#tile)">'
    # wallpaper: dua lengkung lembut berlapis (kedalaman tanpa menambah warna)
    clip += '<path fill="#fff" fill-opacity=".10" d="M0 168C54 120 112 128 168 150S228 150 256 118V256H0Z"/>'
    clip += '<path fill="#1A1F9C" fill-opacity=".16" d="M0 206C60 172 118 196 176 190S232 166 256 176V256H0Z"/>'
    if not small:
        # menu bar: pita tembus pandang di atas dengan dua item kecil di tiap sisi notch
        clip += '<rect x="0" y="12" width="256" height="26" fill="#fff" fill-opacity=".16"/>'
        clip += f'<path fill="#fff" fill-opacity=".85" d="{rr(36,21,18,8,4)}{rr(202,21,18,8,4)}"/>'
    clip += f'<path fill="url(#nt)" d="{NOTCH_S if small else NOTCH}"/>'
    clip += '</g>'
    b += clip
    eyes = EYES_S if small else EYES
    b += f'<path fill="#E8F6FF" d="{eyes}"/>'
    # Dock kaca: bayangan lembut, badan kaca bergradasi, tepi terang, tiga ikon putih (tengah membesar)
    dock, apps = (DOCK_S, APPS_S) if small else (DOCK, APPS)
    b += f'<path fill="#1B1F8F" fill-opacity=".28" transform="translate(0 5)" d="{dock}"/>'
    b += f'<path fill="url(#gl)" stroke="#fff" stroke-opacity=".55" stroke-width="1.5" d="{dock}"/>'
    b += f'<path fill="#fff" d="{"".join(apps)}"/>'
    return b


def glyph_svg(fill):
    """Glyph satu warna tanpa ubin: notch (mata berlubang) di atas, Dock (ikon berlubang) di bawah."""
    body = (f'<path fill="{fill}" fill-rule="evenodd" d="{NOTCH}{EYES}" transform="translate(0 -12)"/>'
            f'<path fill="{fill}" fill-rule="evenodd" d="{DOCK}{"".join(APPS)}"/>')
    return svg(body, "HolmBar glyph", defs="")


def write(name, text):
    os.makedirs(OUT, exist_ok=True)
    path = os.path.join(OUT, name)
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        f.write(text)
    return path


def render(svg_path, png_path, size):
    r = subprocess.run([sys.executable, RENDER, svg_path, "-o", png_path, "--size", str(size)], capture_output=True, text=True)
    if r.returncode != 0 or not os.path.exists(png_path):
        raise SystemExit(f"render gagal: {svg_path}\n{r.stdout}\n{r.stderr}")


def write_ico(path, entries):
    """ICO berisi PNG: entries = [(ukuran, file_png)]."""
    blobs = [(s, open(p, "rb").read()) for s, p in entries]
    head = struct.pack("<HHH", 0, 1, len(blobs))
    off = 6 + 16 * len(blobs)
    table, data = b"", b""
    for s, b in blobs:
        table += struct.pack("<BBBBHHII", 0 if s >= 256 else s, 0 if s >= 256 else s, 0, 0, 1, 32, len(b), off + len(data))
        data += b
    with open(path, "wb") as f:
        f.write(head + table + data)


def main():
    icon = write("holmbar-icon.svg", svg(icon_body(), "HolmBar app icon"))
    small = write("holmbar-icon-small.svg", svg(icon_body(small=True), "HolmBar app icon (small sizes)"))
    write("holmbar-glyph-black.svg", glyph_svg("#000000"))
    write("holmbar-glyph-white.svg", glyph_svg("#FFFFFF"))
    for stale in ("holmbar-symbol.svg", "holmbar-symbol-black.svg", "holmbar-symbol-white.svg", "holmbar-symbol-512.png"):
        p = os.path.join(OUT, stale)
        if os.path.exists(p):
            os.remove(p)

    tmp = tempfile.mkdtemp()
    pngs = {}
    for s in (16, 24, 32, 48, 64, 128, 256):
        p = os.path.join(tmp, f"icon-{s}.png")
        render(small if s <= 32 else icon, p, s)
        pngs[s] = p
    render(icon, os.path.join(OUT, "holmbar-icon-512.png"), 512)
    render(icon, os.path.join(ROOT, "docs", "images", "logo.png"), 256)
    write_ico(os.path.join(ROOT, "res", "holmbar.ico"), [(s, pngs[s]) for s in (16, 24, 32, 48, 64, 128, 256)])
    print("selesai:", OUT)


if __name__ == "__main__":
    main()
