//! ファイルリスト（Everything の EFU、または 1 行 1 パスのテキスト）から索引を組み立てる。
//!
//! EFU は `Filename,Size,Date Modified,Date Created,Attributes` のヘッダ付き CSV（日付は FILETIME）。
//! ヘッダが無ければ各行をパスとして読む。パスはルート（`C:` や `\\server\share`）ごとに
//! 別の VolumeIndex にし、途中のフォルダは暗黙に作る。

use crate::volume::{VolumeIndex, VolumeMeta, ATTR_DIRECTORY};
use std::collections::HashMap;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Row {
    pub path: String,
    pub size: u64,
    pub mtime: i64,
    pub ctime: i64,
    pub attrs: u32,
}

/// CSV の 1 行を分割する（引用符内の `,` と `""` を扱う）。
fn split_csv(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// テキスト全体 → 行。EFU ヘッダがあれば列名で対応付ける。
pub fn parse(text: &str) -> Vec<Row> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.lines();
    let Some(first) = lines.next() else { return Vec::new() };
    let header: Vec<String> = split_csv(first).iter().map(|h| h.trim().to_ascii_lowercase()).collect();
    let col = |name: &str| header.iter().position(|h| h == name);
    let (Some(ci_name), size_i, dm_i, dc_i, attr_i) = (col("filename"), col("size"), col("date modified"), col("date created"), col("attributes")) else {
        // ヘッダなし: 1 行 1 パス
        return std::iter::once(first)
            .chain(lines)
            .map(|l| l.trim().trim_matches('"').to_string())
            .filter(|l| !l.is_empty())
            .map(|p| Row { path: p, ..Default::default() })
            .collect();
    };
    let mut rows = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let f = split_csv(line);
        let get = |i: Option<usize>| i.and_then(|i| f.get(i)).map(|s| s.trim()).unwrap_or("");
        let path = f.get(ci_name).map(|s| s.trim().to_string()).unwrap_or_default();
        if path.is_empty() {
            continue;
        }
        rows.push(Row {
            path,
            size: get(size_i).parse().unwrap_or(0),
            mtime: get(dm_i).parse().unwrap_or(0),
            ctime: get(dc_i).parse().unwrap_or(0),
            attrs: get(attr_i).parse().unwrap_or(0),
        });
    }
    rows
}

/// パスをルートと残りに分ける。`C:\a\b` → (`C:`, `a\b`)、`\\s\share\x` → (`\\s\share`, `x`)。
fn split_root(p: &str) -> Option<(String, String)> {
    let p = p.strip_prefix(r"\\?\UNC\").map(|r| format!(r"\\{r}")).unwrap_or_else(|| p.strip_prefix(r"\\?\").unwrap_or(p).to_string());
    let p = p.replace('/', "\\");
    if let Some(rest) = p.strip_prefix(r"\\") {
        let mut it = rest.splitn(3, '\\');
        let server = it.next()?;
        let share = it.next()?;
        let tail = it.next().unwrap_or("");
        return Some((format!(r"\\{server}\{share}"), tail.to_string()));
    }
    let b = p.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        let root = p[..2].to_ascii_uppercase();
        let tail = p[2..].trim_start_matches('\\').to_string();
        return Some((root, tail));
    }
    None
}

/// 行の集合からルートごとの索引を組み立てる。
pub fn build(rows: &[Row]) -> Vec<VolumeIndex> {
    struct Vol {
        v: VolumeIndex,
        /// (親, 小文字名) → エントリ
        ids: HashMap<(u32, String), u32>,
        next_frn: u64,
    }
    let mut vols: Vec<Vol> = Vec::new();
    let mut by_root: HashMap<String, usize> = HashMap::new();
    for r in rows {
        let Some((root, tail)) = split_root(&r.path) else { continue };
        let vi = *by_root.entry(root.to_ascii_lowercase()).or_insert_with(|| {
            let meta = VolumeMeta { guid_path: format!("filelist:{root}"), display_root: root.clone(), ..Default::default() };
            vols.push(Vol { v: VolumeIndex::new(meta), ids: HashMap::new(), next_frn: 16 });
            vols.len() - 1
        });
        let vol = &mut vols[vi];
        let comps: Vec<&str> = tail.split('\\').filter(|c| !c.is_empty()).collect();
        if comps.is_empty() {
            continue;
        }
        let mut parent = 0u32;
        for (k, c) in comps.iter().enumerate() {
            let last = k + 1 == comps.len();
            let key = (parent, crate::volume::lowercase(c));
            let id = match vol.ids.get(&key) {
                Some(&id) => id,
                None => {
                    let attrs = if last { r.attrs | if r.attrs & ATTR_DIRECTORY != 0 { ATTR_DIRECTORY } else { 0 } } else { ATTR_DIRECTORY };
                    let (size, mtime, ctime) = if last { (if r.attrs & ATTR_DIRECTORY != 0 { 0 } else { r.size }, r.mtime, r.ctime) } else { (0, 0, 0) };
                    let id = vol.v.push_raw(c, parent, attrs, size, mtime, ctime, vol.next_frn);
                    vol.next_frn += 1;
                    vol.ids.insert(key, id);
                    id
                }
            };
            if last {
                // 暗黙に作ったフォルダに本物の行が来たら属性と日付を埋める
                let i = id as usize;
                if vol.v.attrs[i] & ATTR_DIRECTORY != 0 {
                    vol.v.attrs[i] |= r.attrs;
                } else {
                    vol.v.size[i] = r.size;
                }
                if r.mtime != 0 {
                    vol.v.set_mtime(id, r.mtime);
                }
                if r.ctime != 0 {
                    vol.v.ctime_s[i] = crate::volume::filetime_to_secs(r.ctime);
                }
            }
            parent = id;
        }
    }
    vols.into_iter()
        .map(|mut x| {
            x.v.recompute_dir_sizes();
            x.v
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn efu_roundtrip_tree() {
        let text = "Filename,Size,Date Modified,Date Created,Attributes\r\n\"C:\\a\\b.txt\",10,132000000000000000,131000000000000000,32\r\n\"C:\\a\",0,132000000000000000,132000000000000000,16\r\n\"\\\\srv\\share\\x,y.md\",3,0,0,32\r\n";
        let rows = parse(text);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[2].path, "\\\\srv\\share\\x,y.md");
        let vols = build(&rows);
        assert_eq!(vols.len(), 2);
        let c = &vols[0];
        assert_eq!(c.name(0), "C:");
        assert_eq!(c.len(), 3);
        assert!(c.is_dir(1));
        assert_eq!(c.mtime_s[1], crate::volume::filetime_to_secs(132000000000000000));
        assert_eq!(c.full_path(2), "C:\\a\\b.txt");
        assert_eq!(c.size[2], 10);
        assert_eq!(c.size[1], 10);
        let u = &vols[1];
        assert_eq!(u.full_path(1), "\\\\srv\\share\\x,y.md");
    }

    #[test]
    fn plain_list() {
        let rows = parse("D:\\one.txt\n\nD:\\dir\\two.txt\n");
        assert_eq!(rows.len(), 2);
        let vols = build(&rows);
        assert_eq!(vols.len(), 1);
        assert_eq!(vols[0].len(), 4);
    }
}
