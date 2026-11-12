//! UI 文字列の言語切り替え（日本語が原文、英語は対訳表）。
//!
//! コード中の文字列は日本語のまま `tr("…")` / `trf("…{}…", &[&x])` で包む。英語では `i18n_en.rs`
//! （`scripts/i18n_gen.py` が `i18n_en.json` から生成）の対訳を引く。対訳が無ければ原文を返す。
//! 言語は accio.ini の `lang=`（`ja` / `en`、無ければ Windows の UI 言語）で決まる。

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};

const JA: u8 = 0;
const EN: u8 = 1;
static LANG: AtomicU8 = AtomicU8::new(JA);

mod table {
    include!("i18n_en.rs");
}

/// `lang` は `Some("ja")` / `Some("en")`。`None` や未知の値なら Windows の UI 言語から決める。
pub fn init(lang: Option<&str>) {
    let v = match lang.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("ja") => JA,
        Some("en") => EN,
        _ => {
            // SAFETY: 引数なしの Win32 呼び出し。
            let id = unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() };
            if id & 0x3ff == 0x11 { JA } else { EN }
        }
    };
    LANG.store(v, Ordering::Relaxed);
}

pub fn is_en() -> bool {
    LANG.load(Ordering::Relaxed) == EN
}

/// 現在の言語の文字列。英語ではリークして `'static` にする（語彙は有限）。
pub fn tr(key: &'static str) -> &'static str {
    if !is_en() {
        return key;
    }
    static CACHE: OnceLock<Mutex<HashMap<&'static str, &'static str>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut c = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(v) = c.get(key) {
        return v;
    }
    let v: &'static str = match table::EN.binary_search_by(|(k, _)| k.cmp(&key)) {
        Ok(i) => table::EN[i].1,
        Err(_) => key,
    };
    c.insert(key, v);
    v
}

/// `{}` / `{:.N}` を順に埋める（`format!` の最小の部分集合。対訳側も同じ並びで書く）。
pub fn trf(key: &'static str, args: &[&dyn Display]) -> String {
    fill(tr(key), args)
}

fn fill(pat: &str, args: &[&dyn Display]) -> String {
    let mut out = String::with_capacity(pat.len() + 16);
    let mut it = args.iter();
    let mut rest = pat;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        if let Some(stripped) = after.strip_prefix('{') {
            out.push('{');
            rest = stripped;
            continue;
        }
        let Some(j) = after.find('}') else {
            out.push_str(&rest[i..]);
            return out;
        };
        let spec = &after[..j];
        match it.next() {
            Some(a) => {
                if let Some(p) = spec.strip_prefix(":.").and_then(|s| s.parse::<usize>().ok()) {
                    out.push_str(&format!("{a:.p$}"));
                } else {
                    out.push_str(&a.to_string());
                }
            }
            None => {
                out.push('{');
                out.push_str(spec);
                out.push('}');
            }
        }
        rest = &after[j + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fills_placeholders() {
        assert_eq!(fill("{} 件 ({:.1} ms)", &[&3, &12.345]), "3 件 (12.3 ms)");
        assert_eq!(fill("a{}b", &[]), "a{}b");
        assert_eq!(fill("{{x}} {}", &[&"y"]), "{x} y");
    }
}
