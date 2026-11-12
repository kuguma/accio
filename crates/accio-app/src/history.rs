//! 実行履歴（開いたファイル）と検索履歴。Everything と同じ CSV で保存する。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn now_filetime() -> i64 {
    let mut ft = windows_sys::Win32::Foundation::FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    unsafe { windows_sys::Win32::System::SystemInformation::GetSystemTimeAsFileTime(&mut ft) };
    ((ft.dwHighDateTime as i64) << 32) | ft.dwLowDateTime as i64
}

fn csv_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// `"path",count,last_run` 形式の行を読む（先頭が引用符のときのみ対応）。
fn parse_quoted_line(line: &str) -> Option<(String, Vec<&str>)> {
    let rest = line.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.char_indices().peekable();
    let mut end = None;
    while let Some((i, c)) = chars.next() {
        if c == '"' {
            if chars.peek().map(|(_, c2)| *c2 == '"').unwrap_or(false) {
                out.push('"');
                chars.next();
            } else {
                end = Some(i + 1);
                break;
            }
        } else {
            out.push(c);
        }
    }
    let tail = &rest[end?..];
    Some((out, tail.trim_start_matches(',').split(',').collect()))
}

pub struct RunHistory {
    path: PathBuf,
    /// 小文字パス → (回数, 最終実行 FILETIME)
    map: HashMap<String, (u32, i64)>,
    dirty: bool,
}

impl RunHistory {
    pub fn load(path: &Path) -> Self {
        let mut map = HashMap::new();
        if let Ok(text) = std::fs::read_to_string(path) {
            for line in text.lines().skip(1) {
                if let Some((p, f)) = parse_quoted_line(line) {
                    let count = f.first().and_then(|v| v.trim().parse().ok()).unwrap_or(1);
                    let last = f.get(1).and_then(|v| v.trim().parse().ok()).unwrap_or(0);
                    map.insert(accio_index::volume::lowercase(&p), (count, last));
                }
            }
        }
        Self { path: path.to_path_buf(), map, dirty: false }
    }

    pub fn record(&mut self, full_path: &str) {
        let e = self.map.entry(accio_index::volume::lowercase(full_path)).or_insert((0, 0));
        e.0 += 1;
        e.1 = now_filetime();
        self.dirty = true;
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// 検索層（runcount:/dr:）へ渡すスナップショット。
    pub fn snapshot(&self) -> accio_query::RunHistorySnapshot {
        std::sync::Arc::new(self.map.iter().map(|(p, (c, t))| (p.clone(), *c, *t)).collect())
    }

    /// (回数, 最終実行 FILETIME)
    pub fn info(&self, full_path: &str) -> Option<(u32, i64)> {
        self.map.get(&accio_index::volume::lowercase(full_path)).copied()
    }

    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }
        let mut out = String::from("Filename,Run Count,Date Run\r\n");
        for (p, (c, t)) in &self.map {
            out.push_str(&format!("{},{c},{t}\r\n", csv_quote(p)));
        }
        if std::fs::write(&self.path, out).is_ok() {
            self.dirty = false;
        }
    }
}
