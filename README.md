# HolmBar

Satu aplikasi Rust untuk Windows yang menggabungkan menu bar atas, notch (dynamic island), dan Dock bergaya macOS.
Taskbar Windows disembunyikan lalu dipulihkan saat aplikasi keluar.

- Menu bar: logo Windows, nama aplikasi aktif, menu, Wi-Fi, volume, Bluetooth, baterai, jam (kalender saat diklik).
- Ikon tray aplikasi (Discord, OneDrive, dll.) pindah ke menu bar: sebagian langsung tampil (ikuti pilihan
  taskbar Windows, atau atur `[tray]`), sisanya di popup panah. Klik kiri/kanan diteruskan ke aplikasinya.
  HolmBar menjadi host `Shell_TrayWnd` tanpa menyuntik explorer; semua pesan tetap diteruskan ke Explorer.
- Notch (bentuk notch atau pill): saat ringkas menampilkan item pilihan di kiri/kanan (sampul, visualizer,
  judul, timer, baterai, jam, tanggal, CPU); saat terbuka bertab Media (lirik tersinkron lrclib.net),
  Kalender, Timer/Stopwatch, dan Sistem; plus banner notifikasi Windows.
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
