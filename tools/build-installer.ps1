# Membangun HolmBar dan installernya, lalu menaruh hasilnya di dist\HolmBar-Setup-<versi>.exe
# Jalankan:  powershell -ExecutionPolicy Bypass -File tools\build-installer.ps1
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

# HolmBar yang dijalankan dari target\release mengunci exe-nya: minta ia keluar dulu (taskbar dipulihkan).
$dev = Join-Path $root 'target\release\holm-bar.exe'
$running = Get-Process holm-bar -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $dev }
if ($running) {
    Write-Host 'Menutup HolmBar (build pengembangan) yang sedang berjalan...'
    & $dev --quit
    $running | Wait-Process -Timeout 8 -ErrorAction SilentlyContinue
    Get-Process holm-bar -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $dev } | Stop-Process -Force
}

cargo build --release
if ($LASTEXITCODE -ne 0) { throw 'Build HolmBar gagal.' }
cargo build --release -p holm-setup
if ($LASTEXITCODE -ne 0) { throw 'Build installer gagal.' }

$version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
New-Item -ItemType Directory -Force dist | Out-Null
$out = "dist\HolmBar-Setup-$version.exe"
Copy-Item target\release\HolmBar-Setup.exe $out -Force
$size = [Math]::Round((Get-Item $out).Length / 1MB, 2)
$hash = (Get-FileHash $out -Algorithm SHA256).Hash
Write-Host "Selesai: $out ($size MB)"
Write-Host "SHA256: $hash"
