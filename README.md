# HormBar

Satu aplikasi Rust untuk Windows yang menggabungkan menu bar atas, notch (dynamic island), dan Dock bergaya macOS.
Taskbar Windows disembunyikan lalu dipulihkan saat aplikasi keluar.

- Menu bar: logo Windows, nama aplikasi aktif, menu, Wi-Fi, volume, Bluetooth, baterai, jam (kalender saat diklik).
- Notch: kontrol media dengan sampul, lirik tersinkron (lrclib.net), dan banner notifikasi Windows.
- Dock: aplikasi tersemat (termasuk pin taskbar) dan yang berjalan, magnifikasi, tombol Windows.
- Konfigurasi TOML di `%APPDATA%\HormBar\config.toml`, dimuat ulang otomatis.

## Build

```
cargo build --release
```

Keluar lewat klik kanan bar, **Quit HormBar**, agar taskbar kembali tampil.
Bila proses dimatikan paksa, jalankan HormBar lalu keluar lagi, atau restart explorer.
