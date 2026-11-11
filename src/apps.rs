//! Indeks aplikasi terpasang dari `shell:AppsFolder` (daftar yang sama dengan Start Menu):
//! nama tampilan -> AppUserModelID. Dipakai untuk mencari IKON ASLI pengirim notifikasi: banner hanya
//! memberi nama aplikasi ("WhatsApp", "Microsoft Edge"), sedangkan ikon resolusi tinggi diambil shell
//! lewat AppUserModelID-nya (berlaku untuk aplikasi Store maupun aplikasi desktop biasa).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// (nama huruf kecil, nama ternormalisasi, AppUserModelID)
static INDEX: Mutex<Vec<(String, String, String)>> = Mutex::new(Vec::new());
static BUILT: Mutex<Option<Instant>> = Mutex::new(None);
static BUILDING: AtomicBool = AtomicBool::new(false);

/// Hanya huruf/angka, huruf kecil ("Microsoft Edge" -> "microsoftedge").
fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

/// Bangun indeks di thread latar (dipanggil saat start dan saat pencarian gagal pada indeks lama).
pub fn refresh() {
    if BUILDING.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        let list = enumerate();
        if !list.is_empty() {
            if let Ok(mut g) = INDEX.lock() {
                // Entri yang dipelajari dari notifikasi (remember) dipertahankan.
                let learned: Vec<_> = g.iter().filter(|e| !list.iter().any(|n| n.0 == e.0)).cloned().collect();
                *g = list;
                g.extend(learned);
            }
        }
        if let Ok(mut b) = BUILT.lock() {
            *b = Some(Instant::now());
        }
        BUILDING.store(false, Ordering::SeqCst);
    });
}

fn enumerate() -> Vec<(String, String, String)> {
    use windows::Win32::System::Com::*;
    use windows::Win32::UI::Shell::*;
    let mut out = Vec::new();
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let Ok(folder) = SHGetKnownFolderItem::<IShellItem>(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None) else { return out };
        let Ok(items) = folder.BindToHandler::<_, IEnumShellItems>(None::<&IBindCtx>, &BHID_EnumItems) else { return out };
        let text = |it: &IShellItem, kind: SIGDN| match it.GetDisplayName(kind) {
            Ok(p) => {
                let s = p.to_string().unwrap_or_default();
                CoTaskMemFree(Some(p.0 as *const _));
                s
            }
            Err(_) => String::new(),
        };
        loop {
            let mut one = [None];
            let mut got = 0u32;
            if items.Next(&mut one, Some(&mut got)).is_err() || got == 0 {
                break;
            }
            let Some(it) = one[0].take() else { break };
            let name = text(&it, SIGDN_NORMALDISPLAY);
            let id = text(&it, SIGDN_PARENTRELATIVEPARSING);
            if !name.is_empty() && !id.is_empty() {
                out.push((name.to_lowercase(), norm(&name), id));
            }
            if out.len() > 5000 {
                break;
            }
        }
    }
    out
}

/// Catat pasangan nama -> AppUserModelID yang diketahui pasti (dari UserNotificationListener).
pub fn remember(name: &str, aumid: &str) {
    let key = name.trim().to_lowercase();
    if key.is_empty() || aumid.is_empty() {
        return;
    }
    if let Ok(mut g) = INDEX.lock() {
        match g.iter_mut().find(|e| e.0 == key) {
            Some(e) => e.2 = aumid.to_string(),
            None => g.push((key.clone(), norm(&key), aumid.to_string())),
        }
    }
}

/// AppUserModelID aplikasi bernama `name` (nama pengirim pada banner notifikasi).
pub fn aumid_for(name: &str) -> Option<String> {
    let key = name.trim().to_lowercase();
    if key.len() < 2 {
        return None;
    }
    let hit = INDEX.lock().ok().and_then(|g| lookup(&g, &key));
    if hit.is_none() {
        // Mungkin aplikasi baru dipasang: bangun ulang indeks (paling sering sekali per menit).
        let stale = BUILT.lock().ok().map(|b| b.map(|t| t.elapsed() > Duration::from_secs(60)).unwrap_or(true)).unwrap_or(false);
        if stale {
            refresh();
        }
    }
    hit
}

fn lookup(list: &[(String, String, String)], key: &str) -> Option<String> {
    if let Some(e) = list.iter().find(|e| e.0 == key) {
        return Some(e.2.clone());
    }
    let n = norm(key);
    if n.len() < 3 {
        return None;
    }
    if let Some(e) = list.iter().find(|e| e.1 == n) {
        return Some(e.2.clone());
    }
    // "WhatsApp Beta" <-> "WhatsApp", "Teams" <-> "Microsoft Teams": awalan/akhiran, pilih nama terpendek.
    list.iter()
        .filter(|e| e.1.len() >= 3 && (e.1.starts_with(&n) || n.starts_with(&e.1) || e.1.ends_with(&n)))
        .min_by_key(|e| e.1.len().abs_diff(n.len()))
        .map(|e| e.2.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str, id: &str) -> (String, String, String) {
        (name.to_lowercase(), norm(name), id.to_string())
    }

    #[test]
    fn lookup_prefers_exact_then_normalized_then_affix() {
        let list = vec![e("WhatsApp", "wa!App"), e("Microsoft Edge", "MSEdge"), e("Microsoft Teams", "teams!x"), e("Microsoft Edge Beta", "MSEdgeBeta")];
        assert_eq!(lookup(&list, "whatsapp").as_deref(), Some("wa!App"));
        assert_eq!(lookup(&list, "microsoft-edge").as_deref(), Some("MSEdge"));
        assert_eq!(lookup(&list, "teams").as_deref(), Some("teams!x"));
        assert_eq!(lookup(&list, "whatsapp beta").as_deref(), Some("wa!App"));
        assert_eq!(lookup(&list, "zz"), None);
        assert_eq!(lookup(&list, "notepad"), None);
    }
}
