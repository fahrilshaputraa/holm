# Membuat ikon aplikasi HolmBar (res/holmbar.ico + docs/images/logo.png) dari gambar vektor.
# Jalankan dari mana saja:  powershell -ExecutionPolicy Bypass -File tools\make-icon.ps1
Add-Type -AssemblyName System.Drawing
$root = Split-Path $PSScriptRoot -Parent

function New-RoundRect([float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = [Math]::Min($r * 2, [Math]::Min($w, $h))
    $p.AddArc($x, $y, $d, $d, 180, 90)
    $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $p.CloseFigure()
    return $p
}

# Gambar ikon pada ukuran $n px: layar gelap membulat, menu bar + notch di atas, Dock di bawah.
function New-Icon([int]$n) {
    $bmp = New-Object System.Drawing.Bitmap $n, $n, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = 'AntiAlias'
    $g.PixelOffsetMode = 'HighQuality'
    $u = $n / 256.0
    $body = New-RoundRect (12 * $u) (12 * $u) (232 * $u) (232 * $u) (56 * $u)
    $rect = New-Object System.Drawing.RectangleF 0, 0, $n, $n
    $grad = New-Object System.Drawing.Drawing2D.LinearGradientBrush $rect, ([System.Drawing.Color]::FromArgb(255, 58, 74, 160)), ([System.Drawing.Color]::FromArgb(255, 150, 70, 150)), 55
    $g.FillPath($grad, $body)
    $g.SetClip($body)
    # Menu bar tembus pandang di atas.
    $bar = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(70, 0, 0, 0))
    $g.FillRectangle($bar, 0, (12 * $u), $n, (40 * $u))
    # Notch hitam di tengah atas.
    $black = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 8, 8, 10))
    $notch = New-RoundRect (78 * $u) (-10 * $u) (100 * $u) (56 * $u) (20 * $u)
    $g.FillPath($black, $notch)
    $g.ResetClip()
    # Dock kaca dengan tiga aplikasi.
    $dock = New-RoundRect (44 * $u) (168 * $u) (168 * $u) (52 * $u) (20 * $u)
    $glass = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(92, 255, 255, 255))
    $g.FillPath($glass, $dock)
    $colors = @(@(10, 132, 255), @(48, 209, 88), @(255, 159, 10))
    for ($i = 0; $i -lt 3; $i++) {
        $c = $colors[$i]
        $b = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, $c[0], $c[1], $c[2]))
        $app = New-RoundRect ((58 + $i * 50) * $u) (178 * $u) (32 * $u) (32 * $u) (9 * $u)
        $g.FillPath($b, $app)
    }
    $g.Dispose()
    return $bmp
}

$sizes = 16, 24, 32, 48, 64, 128, 256
$blobs = @()
foreach ($n in $sizes) {
    $bmp = New-Icon $n
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $blobs += , $ms.ToArray()
    if ($n -eq 256) {
        New-Item -ItemType Directory -Force "$root\docs\images" | Out-Null
        $bmp.Save("$root\docs\images\logo.png", [System.Drawing.Imaging.ImageFormat]::Png)
    }
    $bmp.Dispose()
}

# Tulis ICO: header, direktori, lalu tiap gambar sebagai PNG (didukung Windows Vista ke atas).
$out = New-Object System.IO.MemoryStream
$w = New-Object System.IO.BinaryWriter $out
$w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $n = $sizes[$i]
    $dim = if ($n -ge 256) { 0 } else { $n }
    $w.Write([byte]$dim); $w.Write([byte]$dim); $w.Write([byte]0); $w.Write([byte]0)
    $w.Write([uint16]1); $w.Write([uint16]32)
    $w.Write([uint32]$blobs[$i].Length); $w.Write([uint32]$offset)
    $offset += $blobs[$i].Length
}
foreach ($b in $blobs) { $w.Write($b) }
$w.Flush()
[System.IO.File]::WriteAllBytes("$root\res\holmbar.ico", $out.ToArray())
"res\holmbar.ico ($($out.Length) byte), docs\images\logo.png"
