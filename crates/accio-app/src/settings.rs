//! 設定の永続化（%LOCALAPPDATA%\accio\accio.ini、key=value）。

use std::collections::BTreeMap;
use std::path::Path;

#[derive(Default, Clone, Debug)]
pub struct Settings {
    map: BTreeMap<String, String>,
}

impl Settings {
    pub fn load(path: &Path) -> Self {
        let mut s = Self::default();
        if let Ok(text) = std::fs::read_to_string(path) {
            for line in text.lines() {
                if let Some((k, v)) = line.split_once('=') {
                    s.map.insert(k.trim().to_string(), v.trim().to_string());
                }
            }
        }
        s
    }
    pub fn save(&self, path: &Path) {
        let mut out = String::new();
        for (k, v) in &self.map {
            out.push_str(&format!("{k}={v}\n"));
        }
        let _ = std::fs::write(path, out);
    }
    pub fn get(&self, k: &str) -> Option<&str> {
        self.map.get(k).map(|s| s.as_str())
    }
    pub fn bool(&self, k: &str, default: bool) -> bool {
        self.get(k).map(|v| v == "1" || v == "true").unwrap_or(default)
    }
    pub fn set(&mut self, k: &str, v: impl ToString) {
        self.map.insert(k.to_string(), v.to_string());
    }

    /// `;` 区切りのリスト設定。
    pub fn list(&self, k: &str) -> Vec<String> {
        self.get(k).map(|v| v.split(';').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()).unwrap_or_default()
    }
}

/// 除外設定（隠し/システム/除外フォルダ/除外ファイル/対象フォルダ限定）を検索式の前置部に変換する。
/// UI の検索にも IPC/HTTP/ETP の検索にも同じものを `accio_query::set_search_prefix` で効かせる。
pub fn exclude_prefix(hidden: bool, system: bool, exclude_folders: &[String], exclude_files: &[String], include_only: &[String]) -> String {
    let mut p = String::new();
    for pat in exclude_files {
        if !pat.is_empty() {
            p.push_str(&format!(" !wfn:\"{pat}\""));
        }
    }
    if hidden {
        p.push_str(" !attrib:H");
    }
    if system {
        p.push_str(" !attrib:S");
    }
    for f in exclude_folders {
        if !f.is_empty() {
            p.push_str(&format!(" !ancestor:\"{f}\" !wfn:\"{f}\""));
        }
    }
    let inc: Vec<&String> = include_only.iter().filter(|f| !f.is_empty()).collect();
    if !inc.is_empty() {
        // 指定フォルダ（自身と配下）だけを対象にする
        let alts: Vec<String> = inc.iter().map(|f| format!("ancestor:\"{f}\"|wfn:\"{f}\"")).collect();
        p.push_str(&format!(" <{}>", alts.join("|")));
    }
    p.trim().to_string()
}

pub fn exclude_prefix_from(settings: &Settings) -> String {
    exclude_prefix(
        settings.bool("exclude_hidden", false),
        settings.bool("exclude_system", false),
        &settings.list("exclude_folders"),
        &settings.list("exclude_files"),
        &settings.list("include_only"),
    )
}
