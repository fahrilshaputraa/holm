# HolmBar

Satu aplikasi Rust untuk Windows yang menggabungkan menu bar atas, notch (dynamic island), dan Dock bergaya macOS.
Taskbar Windows disembunyikan lalu dipulihkan saat aplikasi keluar.

- Menu bar: logo Windows, nama aplikasi aktif, menu, Wi-Fi, volume, Bluetooth, baterai, jam (kalender saat diklik).
- Control Center (ikon dua sakelar di bar): Wi-Fi, Bluetooth, Mode Pesawat, Jangan Ganggu, Proyeksi, slider
  Layar (kecerahan) dan Suara, serta Sedang Diputar. Modul dan item bar dipilih di Pengaturan → Control Center.
- Ikon tray aplikasi (Discord, OneDrive, dll.) pindah ke menu bar: sebagian langsung tampil (ikuti pilihan
  taskbar Windows, atau atur `[tray]`), sisanya di popup panah. Klik kiri/kanan diteruskan ke aplikasinya.
  HolmBar menjadi host `Shell_TrayWnd` tanpa menyuntik explorer; semua pesan tetap diteruskan ke Explorer.
- Notch (bentuk notch atau pill): saat ringkas menampilkan item pilihan di kiri/kanan (sampul, visualizer,
  judul, timer, baterai, jam, tanggal, CPU); saat terbuka bertab Media (lirik tersinkron lrclib.net),
  Kalender, Timer/Stopwatch, dan Sistem; plus banner notifikasi Windows.
- Dock: aplikasi tersemat (termasuk pin taskbar) dan yang berjalan, magnifikasi gelombang realtime, tombol Windows.
  Sembunyi otomatis (`dock.autohide`: `smart` = sembunyi saat ada jendela menutupi, `always`, `off`); arahkan
  kursor ke tepi bawah layar untuk memunculkannya. Klik kanan ikon: *Keep in Dock* / *Remove from Dock*
  (disimpan di `dock.pinned`; aplikasi Store lewat `aumid:ID`). Jendela di desktop virtual lain ikut tampil seperti taskbar.
- Semua menu klik kanan, menu logo, dan konfirmasi daya digambar sendiri bergaya macOS (bukan menu Win32).
- Jendela **Pengaturan**: klik kanan bar/Dock → *Settings...*, menu logo Windows → *HolmBar Settings...*,
  atau jalankan HolmBar sekali lagi. Perubahan langsung diterapkan dan ditulis ke config.toml tanpa menghapus komentar.
- Konfigurasi TOML di `%APPDATA%\HolmBar\config.toml`, dimuat ulang otomatis.

## Build

```
cargo build --release
```

Keluar lewat klik kanan bar, **Quit HolmBar**, agar taskbar kembali tampil.
Bila proses dimatikan paksa, jalankan HolmBar lalu keluar lagi, atau restart explorer.
