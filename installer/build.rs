//! Resource installer: ikon HolmBar (#1), info versi, dan manifest Common Controls v6 sebagai RT_MANIFEST #2
//! (bukan #1, yang sudah diisi manifest bawaan toolchain berisi `asInvoker`); installer mengaktifkannya saat
//! jalan lewat activation context agar TaskDialog tersedia. Juga memastikan aplikasi sudah dibangun, karena
//! `holm-bar.exe` disematkan ke dalam installer.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("..");
    let app = root.join("target").join("release").join("holm-bar.exe");
    println!("cargo:rerun-if-changed={}", app.display());
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../res/holmbar.ico");
    if !app.exists() {
        panic!("target/release/holm-bar.exe belum ada. Jalankan `cargo build --release` dulu (atau tools/build-installer.ps1).");
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let fwd = |p: PathBuf| p.display().to_string().replace('\\', "/");
    let ver = std::env::var("CARGO_PKG_VERSION").unwrap();
    let mut parts: Vec<u16> = ver.split('.').filter_map(|p| p.parse().ok()).collect();
    parts.resize(4, 0);
    let commas = format!("{},{},{},{}", parts[0], parts[1], parts[2], parts[3]);
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    let manifest = root.join("res").join("holmbar.manifest");
    let mut rc_text = format!(
        "1 ICON \"{}\"\n\
         1 VERSIONINFO\nFILEVERSION {commas}\nPRODUCTVERSION {commas}\nFILEOS 0x40004\nFILETYPE 0x1\n\
         BEGIN\n  BLOCK \"StringFileInfo\"\n  BEGIN\n    BLOCK \"040904B0\"\n    BEGIN\n\
               VALUE \"CompanyName\", \"HolmBar\"\n\
               VALUE \"FileDescription\", \"HolmBar Setup\"\n\
               VALUE \"FileVersion\", \"{ver}\"\n\
               VALUE \"OriginalFilename\", \"HolmBar-Setup.exe\"\n\
               VALUE \"ProductName\", \"HolmBar\"\n\
               VALUE \"ProductVersion\", \"{ver}\"\n\
             END\n  END\n\
           BLOCK \"VarFileInfo\"\n  BEGIN\n    VALUE \"Translation\", 0x409, 1200\n  END\nEND\n",
        fwd(root.join("res").join("holmbar.ico"))
    );
    if msvc {
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", manifest.display());
    } else {
        rc_text.push_str(&format!("2 24 \"{}\"\n", fwd(manifest)));
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let rc = out.join("setup.rc");
    std::fs::write(&rc, rc_text).unwrap();
    if msvc {
        let res = out.join("setup.res");
        if Command::new("rc.exe").arg("/nologo").arg(format!("/fo{}", res.display())).arg(&rc).status().map(|s| s.success()).unwrap_or(false) {
            println!("cargo:rustc-link-arg-bins={}", res.display());
        }
        return;
    }
    let obj = out.join("setup-res.o");
    for tool in ["windres", "x86_64-w64-mingw32-windres"] {
        if Command::new(tool).arg("-O").arg("coff").arg("-i").arg(&rc).arg("-o").arg(&obj).status().map(|s| s.success()).unwrap_or(false) {
            println!("cargo:rustc-link-arg-bins={}", obj.display());
            return;
        }
    }
    println!("cargo:warning=windres tidak ditemukan; installer dibuat tanpa ikon dan memakai kotak pesan klasik.");
}
