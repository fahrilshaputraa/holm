# Menyusun gambar README (docs/images/*.png) dari tangkapan komponen HolmBar.
#
# 1. Jalankan HolmBar, lalu:  holm-bar.exe --shots
#    (menyimpan PNG tiap komponen ke %APPDATA%\HolmBar\shots; taruh demo-art.png di folder itu lebih dulu
#     agar media contoh dipakai, bukan lagu yang sedang Anda putar)
# 2. powershell -ExecutionPolicy Bypass -File tools\make-screenshots.ps1
Add-Type -AssemblyName System.Drawing
$root = Split-Path $PSScriptRoot -Parent
$shots = "$env:APPDATA\HolmBar\shots"
$out = "$root\docs\images"
New-Item -ItemType Directory -Force $out | Out-Null

function Get-Shot([string]$name) { [System.Drawing.Image]::FromFile("$shots\$name.png") }

function New-RoundRect([float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = $r * 2
    $p.AddArc($x, $y, $d, $d, 180, 90)
    $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $p.CloseFigure()
    return $p
}

# Kanvas berlatar "wallpaper" gradien dengan sudut membulat.
function New-Canvas([int]$w, [int]$h, [int]$radius = 28) {
    $bmp = New-Object System.Drawing.Bitmap $w, $h, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = 'AntiAlias'
    $g.InterpolationMode = 'HighQualityBicubic'
    $g.PixelOffsetMode = 'HighQuality'
    $g.SetClip((New-RoundRect 0 0 $w $h $radius))
    $rect = New-Object System.Drawing.Rectangle 0, 0, $w, $h
    $br = New-Object System.Drawing.Drawing2D.LinearGradientBrush $rect, ([System.Drawing.Color]::Black), ([System.Drawing.Color]::Black), 25
    $blend = New-Object System.Drawing.Drawing2D.ColorBlend 4
    $blend.Colors = @([System.Drawing.Color]::FromArgb(255, 30, 27, 75), [System.Drawing.Color]::FromArgb(255, 76, 49, 160), [System.Drawing.Color]::FromArgb(255, 168, 72, 150), [System.Drawing.Color]::FromArgb(255, 236, 132, 110))
    $blend.Positions = @(0.0, 0.38, 0.72, 1.0)
    $br.InterpolationColors = $blend
    $g.FillRectangle($br, $rect)
    # Cahaya lembut agar latar tidak datar.
    foreach ($blob in @(@(0.18, 0.2, 0.55, 90, 120, 255), @(0.85, 0.9, 0.5, 255, 190, 120))) {
        $bw = [int]($w * $blob[2]); $bx = [int]($w * $blob[0] - $bw / 2); $by = [int]($h * $blob[1] - $bw / 2)
        $path = New-Object System.Drawing.Drawing2D.GraphicsPath
        $path.AddEllipse($bx, $by, $bw, $bw)
        $pg = New-Object System.Drawing.Drawing2D.PathGradientBrush $path
        $pg.CenterColor = [System.Drawing.Color]::FromArgb(70, $blob[3], $blob[4], $blob[5])
        $pg.SurroundColors = @([System.Drawing.Color]::FromArgb(0, $blob[3], $blob[4], $blob[5]))
        $g.FillPath($pg, $path)
    }
    return @{ Bmp = $bmp; G = $g; W = $w; H = $h }
}

function Save-Canvas($c, [string]$name) {
    $c.G.Dispose()
    $c.Bmp.Save("$out\$name.png", [System.Drawing.Imaging.ImageFormat]::Png)
    $c.Bmp.Dispose()
    Write-Host "docs\images\$name.png"
}

# Gambar $img dengan lebar $w (tinggi proporsional), tepi atas di $y, dipusatkan pada $cx. Mengembalikan tingginya.
function Add-Image($c, $img, [float]$cx, [float]$y, [float]$w) {
    $h = $img.Height * $w / $img.Width
    $c.G.DrawImage($img, [float]($cx - $w / 2), $y, $w, [float]$h)
    return $h
}

function Add-Label($c, [string]$text, [float]$cx, [float]$y, [float]$size = 20) {
    $font = New-Object System.Drawing.Font 'Segoe UI Semibold', $size, ([System.Drawing.FontStyle]::Regular), ([System.Drawing.GraphicsUnit]::Pixel)
    $fmt = New-Object System.Drawing.StringFormat
    $fmt.Alignment = 'Center'
    $c.G.TextRenderingHint = 'AntiAliasGridFit'
    $c.G.DrawString($text, $font, (New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(230, 255, 255, 255))), $cx, $y, $fmt)
}

# ---------- hero: bar + notch terbuka + Dock di atas wallpaper ----------
$bar = Get-Shot 'bar-0'; $notch = Get-Shot 'notch-media'; $dock = Get-Shot 'dock-0'
$c = New-Canvas $bar.Width 460 22
$c.G.DrawImage($bar, 0, 0, $bar.Width, $bar.Height)
[void](Add-Image $c $notch ($c.W / 2) 0 $notch.Width)
$c.G.DrawImage($dock, [int](($c.W - $dock.Width) / 2), ($c.H - $dock.Height - 8), $dock.Width, $dock.Height)
Save-Canvas $c 'hero'
$bar.Dispose(); $notch.Dispose(); $dock.Dispose()

# ---------- kisi 2 kolom untuk keadaan notch ----------
function New-Grid([string[]]$names, [string[]]$labels, [string]$file, [int]$cellW) {
    $imgs = $names | ForEach-Object { Get-Shot $_ }
    $pad = 56; $gap = 48; $labelH = 44
    $rows = [Math]::Ceiling($imgs.Count / 2)
    $rowH = @()
    for ($r = 0; $r -lt $rows; $r++) {
        $m = 0
        for ($k = 0; $k -lt 2; $k++) { $i = $r * 2 + $k; if ($i -lt $imgs.Count) { $m = [Math]::Max($m, $imgs[$i].Height * [Math]::Min(1.0, $cellW / $imgs[$i].Width)) } }
        $rowH += $m
    }
    $w = 2 * $cellW + $gap + 2 * $pad
    $h = 2 * $pad + ($rowH | Measure-Object -Sum).Sum + $rows * $labelH + ($rows - 1) * $gap
    $c = New-Canvas $w ([int]$h)
    $y = $pad
    for ($r = 0; $r -lt $rows; $r++) {
        for ($k = 0; $k -lt 2; $k++) {
            $i = $r * 2 + $k
            if ($i -ge $imgs.Count) { continue }
            $cx = $pad + $cellW / 2 + $k * ($cellW + $gap)
            $iw = [Math]::Min($cellW, $imgs[$i].Width)
            Add-Label $c $labels[$i] $cx $y 22
            [void](Add-Image $c $imgs[$i] $cx ($y + $labelH) $iw)
        }
        $y += $rowH[$r] + $labelH + $gap
    }
    Save-Canvas $c $file
    $imgs | ForEach-Object { $_.Dispose() }
}

New-Grid @('notch2x-media', 'notch2x-calendar', 'notch2x-timer', 'notch2x-system') @('Home: media, mini calendar, timer', 'Calendar', 'Timer & stopwatch', 'System') 'notch-tabs' 900
New-Grid @('notch2x-message', 'notch2x-call', 'notch2x-bluetooth', 'notch2x-charging') @('Notifications with the real app icon', 'Incoming calls: accept or decline', 'Bluetooth device connected', 'Charger plugged in') 'notch-notifications' 900

# ---------- popup menu bar ----------
$pops = @('popup2x-control', 'popup2x-calendar', 'popup2x-volume') | ForEach-Object { Get-Shot $_ }
$scale = 0.75; $pad = 56; $gap = 44; $labelH = 44
$w = 2 * $pad + ($pops | ForEach-Object { $_.Width * $scale } | Measure-Object -Sum).Sum + 2 * $gap
$h = 2 * $pad + $labelH + ($pops | ForEach-Object { $_.Height * $scale } | Measure-Object -Maximum).Maximum
$c = New-Canvas ([int]$w) ([int]$h)
$x = $pad
$labels = @('Control Center', 'Calendar', 'Volume')
for ($i = 0; $i -lt $pops.Count; $i++) {
    $iw = $pops[$i].Width * $scale
    Add-Label $c $labels[$i] ($x + $iw / 2) $pad 22
    [void](Add-Image $c $pops[$i] ($x + $iw / 2) ($pad + $labelH) $iw)
    $x += $iw + $gap
}
Save-Canvas $c 'menu-bar-popups'
$pops | ForEach-Object { $_.Dispose() }

# ---------- Dock ----------
$dock = Get-Shot 'dock-0'
$c = New-Canvas ($dock.Width + 160) ($dock.Height + 90)
$c.G.DrawImage($dock, 80, 50, $dock.Width, $dock.Height)
Save-Canvas $c 'dock'
$dock.Dispose()
