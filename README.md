# HolmBar

Satu aplikasi Rust untuk Windows yang menggabungkan menu bar atas, notch (dynamic island), dan Dock bergaya macOS.
Taskbar Windows disembunyikan lalu dipulihkan saat aplikasi keluar.

- Menu bar: logo Windows, nama aplikasi aktif, menu, Wi-Fi, volume, Bluetooth, baterai, jam (kalender saat diklik).
- Notch: kontrol media dengan sampul, lirik tersinkron (lrclib.net), dan banner notifikasi Windows.
- Dock: aplikasi tersemat (termasuk pin taskbar) dan yang berjalan, magnifikasi, tombol Windows.
- Jendela **Pengaturan**: klik kanan bar/Dock → *Settings...*, menu logo Windows → *HolmBar Settings...*,
  atau jalankan HolmBar sekali lagi. Perubahan langsung diterapkan dan ditulis ke config.toml tanpa menghapus komentar.
- Konfigurasi TOML di `%APPDATA%\HolmBar\config.toml`, dimuat ulang otomatis.

## Build

```
cargo build --release
```

Keluar lewat klik kanan bar, **Quit HolmBar**, agar taskbar kembali tampil.
Bila proses dimatikan paksa, jalankan HolmBar lalu keluar lagi, atau restart explorer.
