//! Menyematkan resource Windows ke exe: ikon aplikasi (#1), info versi, dan manifest Common Controls v6
//! sebagai RT_MANIFEST #2 (bukan #1, agar tidak bentrok dengan manifest bawaan toolchain); settings.rs
//! mengaktifkannya lewat activation context.
//! GNU: dikompilasi dengan windres. MSVC: manifest digabung link.exe; ikon/versi lewat rc.exe bila ada.
//! Bila alat resource tidak ditemukan, build tetap jalan (exe tanpa ikon, Pengaturan bergaya klasik).

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=res/holmbar.manifest");
    println!("cargo:rerun-if-changed=res/holmbar.ico");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let fwd = |p: PathBuf| p.display().to_string().replace('\\', "/");
    let manifest = root.join("res").join("holmbar.manifest");
    let icon = fwd(root.join("res").join("holmbar.ico"));
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");

    let ver = std::env::var("CARGO_PKG_VERSION").unwrap();
    let mut parts: Vec<u16> = ver.split('.').filter_map(|p| p.parse().ok()).collect();
    parts.resize(4, 0);
    let commas = format!("{},{},{},{}", parts[0], parts[1], parts[2], parts[3]);
    let mut rc_text = format!(
        "1 ICON \"{icon}\"\n\
         1 VERSIONINFO\nFILEVERSION {commas}\nPRODUCTVERSION {commas}\nFILEOS 0x40004\nFILETYPE 0x1\n\
         BEGIN\n  BLOCK \"StringFileInfo\"\n  BEGIN\n    BLOCK \"040904B0\"\n    BEGIN\n\
               VALUE \"CompanyName\", \"HolmBar\"\n\
               VALUE \"FileDescription\", \"HolmBar\"\n\
               VALUE \"FileVersion\", \"{ver}\"\n\
               VALUE \"InternalName\", \"holm-bar\"\n\
               VALUE \"OriginalFilename\", \"HolmBar.exe\"\n\
               VALUE \"ProductName\", \"HolmBar\"\n\
               VALUE \"ProductVersion\", \"{ver}\"\n\
             END\n  END\n\
           BLOCK \"VarFileInfo\"\n  BEGIN\n    VALUE \"Translation\", 0x409, 1200\n  END\nEND\n"
    );
    if msvc {
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", manifest.display());
    } else {
        rc_text.push_str(&format!("2 24 \"{}\"\n", fwd(manifest)));
    }

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let rc = out.join("holmbar.rc");
    std::fs::write(&rc, rc_text).unwrap();
    if msvc {
        let res = out.join("holmbar.res");
        let ok = Command::new("rc.exe").arg("/nologo").arg(format!("/fo{}", res.display())).arg(&rc).status().map(|s| s.success()).unwrap_or(false);
        if ok {
            println!("cargo:rustc-link-arg-bins={}", res.display());
        } else {
            println!("cargo:warning=rc.exe tidak ditemukan; exe dibuat tanpa ikon dan info versi.");
        }
        return;
    }
    let obj = out.join("holmbar-res.o");
    for tool in ["windres", "x86_64-w64-mingw32-windres"] {
        let ok = Command::new(tool).arg("-O").arg("coff").arg("-i").arg(&rc).arg("-o").arg(&obj).status().map(|s| s.success()).unwrap_or(false);
        if ok {
            println!("cargo:rustc-link-arg-bins={}", obj.display());
            return;
        }
    }
    println!("cargo:warning=windres tidak ditemukan; exe dibuat tanpa ikon, info versi, dan manifest Common Controls v6 (Pengaturan memakai gaya klasik).");
}
