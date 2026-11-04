//! Menyematkan manifest Common Controls v6 sebagai resource RT_MANIFEST #2 (bukan #1, agar tidak
//! bentrok dengan manifest bawaan toolchain); settings.rs mengaktifkannya lewat activation context.
//! MSVC: link.exe menggabungkan manifest ke #1 sendiri. GNU: dikompilasi dengan windres; bila windres tidak ada, build tetap jalan
//! dan jendela Pengaturan memakai gaya kontrol klasik.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=res/holmbar.manifest");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("res").join("holmbar.manifest");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", manifest.display());
        return;
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let rc = out.join("holmbar.rc");
    let obj = out.join("holmbar-res.o");
    let path = manifest.display().to_string().replace('\\', "/");
    std::fs::write(&rc, format!("2 24 \"{path}\"\n")).unwrap();
    for tool in ["windres", "x86_64-w64-mingw32-windres"] {
        let ok = Command::new(tool).arg("-O").arg("coff").arg("-i").arg(&rc).arg("-o").arg(&obj).status().map(|s| s.success()).unwrap_or(false);
        if ok {
            println!("cargo:rustc-link-arg-bins={}", obj.display());
            return;
        }
    }
    println!("cargo:warning=windres tidak ditemukan; manifest Common Controls v6 tidak disematkan (Pengaturan memakai gaya klasik).");
}
