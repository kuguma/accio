//! 構文木を 1 ボリュームの索引に対して評価し、ビットセットを返す。
//!
//! 文字列項の基本経路は小文字化アリーナに対する SIMD 部分文字列走査（memchr::memmem）。
//! それ以外の述語はエントリ単位の並列評価。すべての経路は 64 エントリ境界に揃えたチャンクで
//! 並列化し、各チャンクはワード単位で非アトミックに書き込む。

use crate::bitset::Bitset;
use crate::parser::{ChildKind, DupeKey, Kind, Mods, Node, Term};
use std::collections::HashMap;

/// 全ボリューム横断で必要な前計算（重複検出）とボリューム固有の文脈。
#[derive(Default, Clone)]
pub struct EvalCtx {
    /// 重複キー → 2 回以上現れるハッシュ値（ソート済み、二分探索で引く）
    pub dupes: HashMap<DupeKey, std::sync::Arc<Vec<u64>>>,
    /// 評価中のボリュームの番号（`fsi:`）
    pub fsi: u32,
}

/// `runcount:` `dr:` 用の実行履歴（小文字フルパス, 回数, 最終実行 FILETIME）。アプリ層が更新する。
pub type RunHistorySnapshot = std::sync::Arc<Vec<(String, u32, i64)>>;
static RUN_HISTORY: std::sync::RwLock<Option<RunHistorySnapshot>> = std::sync::RwLock::new(None);

pub fn set_run_history(h: RunHistorySnapshot) {
    *RUN_HISTORY.write().unwrap_or_else(|e| e.into_inner()) = Some(h);
}

/// フルパス（原綴りでよい）の実行履歴 (回数, 最終実行 FILETIME)。IPC の Run Count / Date Run 用。
pub fn run_history_get(full_path: &str) -> Option<(u32, i64)> {
    let hist = RUN_HISTORY.read().unwrap_or_else(|e| e.into_inner()).clone()?;
    let key = lowercase(full_path);
    hist.iter().find(|(p, _, _)| *p == key).map(|(_, c, t)| (*c, *t))
}

/// 各エントリの実行履歴 (回数, 最終実行) を引いて述語 `f` を評価する。履歴にないエントリは `None`。
/// ボリュームの小文字ルート（`c:`）で始まる履歴だけを対象に、末尾名→候補の表を作ってから 1 パスで判定する。
fn run_history_pred(v: &VolumeIndex, cancel: Cancel, f: impl Fn(Option<(u32, i64)>) -> bool + Sync) -> Option<Bitset> {
    let hist = RUN_HISTORY.read().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default();
    let mut prefix = lowercase(v.name(0));
    prefix.push('\\');
    let mut by_name: HashMap<&[u8], Vec<usize>> = HashMap::new();
    for (k, (p, _, _)) in hist.iter().enumerate() {
        if p.starts_with(&prefix) {
            let last = p.rsplit('\\').next().unwrap_or(p);
            by_name.entry(last.as_bytes()).or_default().push(k);
        }
    }
    if by_name.is_empty() {
        return pred(v, cancel, |_| f(None));
    }
    pred(v, cancel, |i| {
        let Some(ks) = by_name.get(v.lower_name(i as u32)) else { return f(None) };
        thread_local! { static BUF: std::cell::RefCell<(Vec<u8>, Vec<u32>)> = const { std::cell::RefCell::new((Vec::new(), Vec::new())) }; }
        BUF.with(|b| {
            let mut b = b.borrow_mut();
            let (buf, segs) = &mut *b;
            lower_full_path(v, i, buf, segs);
            let found = ks.iter().find(|&&k| hist[k].0.as_bytes() == buf.as_slice()).map(|&k| (hist[k].1, hist[k].2));
            f(found)
        })
    })
}

/// `type:` 用の種別名提供者（拡張子, フォルダか）→ 種別名。アプリ層がシェルから登録する。
pub type TypeNameFn = dyn Fn(&str, bool) -> String + Send + Sync;
static TYPE_NAME: std::sync::OnceLock<Box<TypeNameFn>> = std::sync::OnceLock::new();

pub fn set_type_name_provider(f: Box<TypeNameFn>) {
    let _ = TYPE_NAME.set(f);
}

/// `content:` で読むファイルの上限
pub const CONTENT_MAX_FILES: usize = 200_000;
pub const CONTENT_MAX_BYTES: u64 = 32 << 20;

/// `needle` を含むファイルだけ残す（UTF-8 と UTF-16LE の両方で探す。ASCII は大文字小文字無視）。
fn content_filter(v: &VolumeIndex, cand: &Bitset, needle: &str, cancel: Cancel) -> Option<Bitset> {
    let lower = lowercase(needle);
    let n8 = lower.as_bytes().to_vec();
    let n16: Vec<u8> = lower.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let f8 = memmem::Finder::new(&n8);
    let f16 = memmem::Finder::new(&n16);
    let ids: Vec<usize> = cand.iter_ones().filter(|&i| !v.is_dir(i as u32)).take(CONTENT_MAX_FILES).collect();
    let out = Bitset::zeros(v.len());
    let words = out.atomic();
    let ok = ids.par_iter().try_for_each(|&i| {
        if cancel() {
            return Err(());
        }
        if v.size[i] > CONTENT_MAX_BYTES || v.size[i] == 0 {
            return Ok(());
        }
        let path = v.full_path(i as u32);
        let Ok(mut data) = std::fs::read(&path) else { return Ok(()) };
        // ASCII の大文字を小文字へ（非 ASCII はそのまま）
        for b in data.iter_mut() {
            if b.is_ascii_uppercase() {
                *b |= 0x20;
            }
        }
        if f8.find(&data).is_some() || f16.find(&data).is_some() {
            words[i >> 6].fetch_or(1u64 << (i & 63), Ordering::Relaxed);
        }
        Ok(())
    });
    ok.ok()?;
    Some(out)
}

fn is_content(n: &Node) -> bool {
    matches!(n, Node::Term(Term { kind: Kind::Content(_), .. }))
}

/// 安定した 64bit ハッシュ（FNV-1a）。衝突は重複検出では無視できる。
pub fn hash64(b: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &x in b {
        h ^= x as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// 重複キーに使う値のハッシュ。
pub fn dupe_hash(v: &VolumeIndex, i: usize, key: DupeKey) -> u64 {
    match key {
        DupeKey::Name => hash64(v.lower_name(i as u32)),
        DupeKey::NamePart => {
            let n = v.lower_name(i as u32);
            let stem = match n.iter().rposition(|&c| c == b'.') {
                Some(p) if p > 0 => &n[..p],
                _ => n,
            };
            hash64(stem)
        }
        DupeKey::Size => v.size[i] ^ 0x9e37_79b9_7f4a_7c15,
        DupeKey::DateModified => (v.mtime_s[i] as u64) ^ 0x5851_f42d_4c95_7f2d,
    }
}

/// 構文木に現れる重複キー。
pub fn dupe_keys(node: &Node, out: &mut Vec<DupeKey>) {
    match node {
        Node::Term(Term { kind: Kind::Dupe(k), .. }) => {
            if !out.contains(k) {
                out.push(*k);
            }
        }
        Node::And(v) | Node::Or(v) => v.iter().for_each(|n| dupe_keys(n, out)),
        Node::Not(b) => dupe_keys(b, out),
        _ => {}
    }
}
use accio_index::volume::{lowercase, lowercase_nofold, ATTR_DIRECTORY, FLAG_DELETED, NONE};
use accio_index::VolumeIndex;
use memchr::memmem;
use rayon::prelude::*;
use std::sync::atomic::Ordering;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SearchOpts {
    pub match_case: bool,
    pub match_path: bool,
    pub whole_word: bool,
    pub regex: bool,
    /// ダイアクリティカルマークを区別する（既定 off: é は e に一致）
    pub match_diacritics: bool,
}

const CHUNK: usize = 1 << 14;

pub type Cancel<'a> = &'a (dyn Fn() -> bool + Sync);

/// エントリ単位述語の並列評価。
fn pred(v: &VolumeIndex, cancel: Cancel, f: impl Fn(usize) -> bool + Sync) -> Option<Bitset> {
    let n = v.len();
    let bs = Bitset::zeros(n);
    let words = bs.atomic();
    let flags = &v.flags;
    let ok = (0..n.div_ceil(CHUNK)).into_par_iter().try_for_each(|c| {
        if cancel() {
            return Err(());
        }
        let start = c * CHUNK;
        let end = (start + CHUNK).min(n);
        let mut w = start / 64;
        let mut i = start;
        while i < end {
            let we = (i + 64).min(end);
            let mut bits = 0u64;
            #[allow(clippy::needless_range_loop)]
            for k in i..we {
                if flags[k] & FLAG_DELETED == 0 && f(k) {
                    bits |= 1u64 << (k & 63);
                }
            }
            words[w].store(bits, Ordering::Relaxed);
            w += 1;
            i = we;
        }
        Ok(())
    });
    ok.ok().map(|_| bs)
}

/// 小文字化アリーナの部分文字列走査。`check(entry, hit_pos_in_name)` で追加条件を課せる。
fn scan_substring(v: &VolumeIndex, needle: &[u8], cancel: Cancel, check: impl Fn(usize, usize) -> bool + Sync) -> Option<Bitset> {
    let n = v.len();
    let bs = Bitset::zeros(n);
    let words = bs.atomic();
    let finder = memmem::Finder::new(needle);
    let nl = needle.len() as u32;
    let ok = (0..n.div_ceil(CHUNK)).into_par_iter().try_for_each(|c| {
        if cancel() {
            return Err(());
        }
        let a = c * CHUNK;
        let b = (a + CHUNK).min(n);
        let start = v.name_off[a] as usize;
        let end = if b < n { v.name_off[b] as usize } else { v.arena.len() };
        let offs = &v.name_off[a..b];
        let mut local = vec![0u64; (b - a).div_ceil(64) + 1];
        for pos in finder.find_iter(&v.arena[start..end]) {
            let gp = (start + pos) as u32;
            // チャンク内で最後の name_off <= gp
            let k = offs.partition_point(|&o| o <= gp);
            if k == 0 {
                continue;
            }
            let e = a + k - 1;
            if v.flags[e] & FLAG_DELETED != 0 {
                continue;
            }
            let o = v.name_off[e];
            if gp + nl <= o + v.name_len[e] as u32 && check(e, (gp - o) as usize) {
                local[(e - a) >> 6] |= 1u64 << (e & 63);
            }
        }
        // a は 64 の倍数（CHUNK が 64 の倍数）なのでワード境界が一致する
        let w0 = a / 64;
        for (j, bits) in local.iter().enumerate() {
            if *bits != 0 && w0 + j < words.len() {
                words[w0 + j].fetch_or(*bits, Ordering::Relaxed);
            }
        }
        Ok(())
    });
    ok.ok()?;
    // overflow（主アリーナ外の名前）
    for (i, &f) in v.flags.iter().enumerate() {
        if f & accio_index::volume::FLAG_OVERFLOW != 0 && f & FLAG_DELETED == 0 {
            let name = v.lower_name(i as u32);
            if let Some(p) = finder.find(name) {
                if check(i, p) {
                    words[i >> 6].fetch_or(1u64 << (i & 63), Ordering::Relaxed);
                }
            }
        }
    }
    Some(bs)
}

#[inline]
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

fn whole_word_ok(name: &[u8], pos: usize, len: usize) -> bool {
    let before = pos == 0 || !is_word_byte(name[pos - 1]);
    let after = pos + len >= name.len() || !is_word_byte(name[pos + len]);
    before && after
}

/// ワイルドカード（`*` `?`）のファイル名全体一致。
pub fn glob_match(pat: &[u8], s: &[u8]) -> bool {
    let (mut p, mut i) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while i < s.len() {
        if p < pat.len() && (pat[p] == b'?' || pat[p] == s[i]) {
            p += 1;
            i += 1;
        } else if p < pat.len() && pat[p] == b'*' {
            star = p;
            mark = i;
            p += 1;
        } else if star != usize::MAX {
            p = star + 1;
            mark += 1;
            i = mark;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == b'*' {
        p += 1;
    }
    p == pat.len()
}

/// 小文字化フルパスを `buf` に組み立てる（`c:\a\b\name`）。
/// 親エントリ。構築中（`resolved == false`）の索引は `parent` に MFT レコード番号が仮置きされているので
/// 辿らない。範囲外の値も NONE 扱い（添字で落ちない）。
#[inline]
fn parent_of(v: &VolumeIndex, i: usize) -> u32 {
    if !v.resolved {
        return NONE;
    }
    match v.parent.get(i) {
        Some(&p) if (p as usize) < v.len() => p,
        _ => NONE,
    }
}

fn lower_full_path(v: &VolumeIndex, i: usize, buf: &mut Vec<u8>, segs: &mut Vec<u32>) {
    buf.clear();
    segs.clear();
    let mut p = i as u32;
    let mut guard = 0;
    while p != NONE && guard < 512 {
        segs.push(p);
        p = parent_of(v, p as usize);
        guard += 1;
    }
    for (k, &s) in segs.iter().rev().enumerate() {
        if k > 0 {
            buf.push(b'\\');
        }
        buf.extend_from_slice(v.lower_name(s));
    }
}

/// 名前一致ビットセットを配下へ伝播する。
/// `include_self` なら「自身または先祖が一致」、そうでなければ「真の先祖が一致」。
fn propagate_to_descendants(v: &VolumeIndex, m: &Bitset, include_self: bool) -> Bitset {
    let n = v.len();
    // state[i]: 0 未確定, 1 = 自身も先祖も不一致, 2 = 自身か先祖が一致
    let mut state = vec![0u8; n];
    let mut stack: Vec<u32> = Vec::with_capacity(64);
    for i in 0..n {
        if state[i] != 0 {
            continue;
        }
        stack.clear();
        let mut p = i as u32;
        let mut val = 1u8;
        while p != NONE {
            if state[p as usize] != 0 {
                val = state[p as usize];
                break;
            }
            stack.push(p);
            if m.get(p as usize) {
                val = 2;
                break;
            }
            p = parent_of(v, p as usize);
        }
        for &s in &stack {
            state[s as usize] = val;
        }
    }
    let mut out = Bitset::zeros(n);
    for i in 0..n {
        let hit = if include_self {
            state[i] == 2
        } else {
            let p = parent_of(v, i);
            p != NONE && state[p as usize] == 2
        };
        if hit && v.flags[i] & FLAG_DELETED == 0 {
            out.set(i);
        }
    }
    out
}

/// `c:\foo\bar` 形式（小文字）のパスをエントリへ解決する。
pub fn resolve_path(v: &VolumeIndex, lower_path: &str, cancel: Cancel) -> Option<u32> {
    let root = lowercase(v.name(0));
    let path = lower_path.trim_end_matches('\\');
    if path == root {
        return Some(0);
    }
    let last = path.rsplit('\\').next()?;
    if last.is_empty() {
        return None;
    }
    let cands = scan_substring(v, last.as_bytes(), cancel, |e, pos| pos == 0 && v.lower_name(e as u32).len() == last.len())?;
    let mut buf = Vec::new();
    let mut segs = Vec::new();
    for i in cands.iter_ones() {
        if !v.is_dir(i as u32) {
            continue;
        }
        lower_full_path(v, i, &mut buf, &mut segs);
        if buf == path.as_bytes() {
            return Some(i as u32);
        }
    }
    None
}

/// ダイアクリティカルマークを区別する照合: 折り畳み照合の結果（上位集合）から、
/// 小文字化のみの名前/パスが本当に含むものだけを残す。
fn diacritics_filter(v: &VolumeIndex, bs: Bitset, text: &str, path_mode: bool, wfn: bool) -> Bitset {
    let want = lowercase_nofold(text);
    let finder = memmem::Finder::new(want.as_bytes());
    let mut out = Bitset::zeros(v.len());
    for i in bs.iter_ones() {
        // 検索語も名前も ASCII なら折り畳みの影響を受けない（候補のまま）
        if !path_mode && want.is_ascii() && v.name(i as u32).is_ascii() {
            out.set(i);
            continue;
        }
        let hay = if path_mode { lowercase_nofold(&v.full_path(i as u32)) } else { lowercase_nofold(v.name(i as u32)) };
        let ok = if wfn { glob_match(want.as_bytes(), hay.as_bytes()) } else { finder.find(hay.as_bytes()).is_some() };
        if ok {
            out.set(i);
        }
    }
    out
}

fn eval_text(v: &VolumeIndex, text: &str, mods: &Mods, opts: &SearchOpts, cancel: Cancel) -> Option<Bitset> {
    let diac = mods.diacritics.unwrap_or(opts.match_diacritics);
    let bs = eval_text_folded(v, text, mods, opts, cancel)?;
    if !diac {
        return Some(bs);
    }
    let case = mods.case.unwrap_or(opts.match_case);
    if case {
        // 大小区別は原綴りで照合済み（折り畳みなし）
        return Some(bs);
    }
    let has_sep = text.contains('\\');
    let path_mode = mods.path.unwrap_or(opts.match_path) || has_sep;
    let wildcard = mods.wildcards.unwrap_or(true) && (text.contains('*') || text.contains('?'));
    let wfn = mods.wfn.unwrap_or(false) || wildcard;
    Some(diacritics_filter(v, bs, text, path_mode, wfn))
}

fn eval_text_folded(v: &VolumeIndex, text: &str, mods: &Mods, opts: &SearchOpts, cancel: Cancel) -> Option<Bitset> {
    let case = mods.case.unwrap_or(opts.match_case);
    let ww = mods.wholeword.unwrap_or(opts.whole_word);
    let has_sep = text.contains('\\');
    let path_mode = mods.path.unwrap_or(opts.match_path) || has_sep;
    let wildcard = mods.wildcards.unwrap_or(true) && (text.contains('*') || text.contains('?'));
    let wfn = mods.wfn.unwrap_or(false) || wildcard;
    let lower = lowercase(text);

    if path_mode {
        if !has_sep && !wfn && !case {
            let m = scan_substring(v, lower.as_bytes(), cancel, |e, pos| !ww || whole_word_ok(v.lower_name(e as u32), pos, lower.len()))?;
            return Some(propagate_to_descendants(v, &m, true));
        }
        let finder = memmem::Finder::new(if case { text.as_bytes() } else { lower.as_bytes() });
        let pat_lower = lower.as_bytes().to_vec();
        return pred(v, cancel, |i| {
            thread_local! { static BUF: std::cell::RefCell<(Vec<u8>, Vec<u32>, String)> = const { std::cell::RefCell::new((Vec::new(), Vec::new(), String::new())) }; }
            BUF.with(|b| {
                let mut b = b.borrow_mut();
                let (buf, segs, _) = &mut *b;
                if case {
                    // 原綴りのフルパス
                    let fp = v.full_path(i as u32);
                    if wfn {
                        return glob_match(text.as_bytes(), fp.as_bytes());
                    }
                    return finder.find(fp.as_bytes()).is_some();
                }
                lower_full_path(v, i, buf, segs);
                if wfn {
                    glob_match(&pat_lower, buf)
                } else {
                    finder.find(buf).is_some()
                }
            })
        });
    }

    if case {
        let finder = memmem::Finder::new(text.as_bytes());
        return pred(v, cancel, |i| {
            let n = v.name(i as u32).as_bytes();
            if wfn {
                glob_match(text.as_bytes(), n)
            } else {
                match finder.find(n) {
                    Some(p) => !ww || whole_word_ok(n, p, text.len()),
                    None => false,
                }
            }
        });
    }
    if wfn {
        let pat = lower.as_bytes().to_vec();
        return pred(v, cancel, |i| glob_match(&pat, v.lower_name(i as u32)));
    }
    if lower.is_empty() {
        return pred(v, cancel, |_| true);
    }
    scan_substring(v, lower.as_bytes(), cancel, |e, pos| !ww || whole_word_ok(v.lower_name(e as u32), pos, lower.len()))
}

fn eval_regex(v: &VolumeIndex, pat: &str, mods: &Mods, opts: &SearchOpts, cancel: Cancel) -> Option<Bitset> {
    let case = mods.case.unwrap_or(opts.match_case);
    let path_mode = mods.path.unwrap_or(opts.match_path);
    let re = match regex::bytes::RegexBuilder::new(pat).case_insensitive(!case).build() {
        Ok(r) => r,
        Err(_) => return Some(Bitset::zeros(v.len())),
    };
    pred(v, cancel, |i| {
        if path_mode {
            re.is_match(v.full_path(i as u32).as_bytes())
        } else {
            re.is_match(v.name(i as u32).as_bytes())
        }
    })
}

fn eval_term(v: &VolumeIndex, t: &Term, opts: &SearchOpts, ctx: &EvalCtx, cancel: Cancel) -> Option<Bitset> {
    let n = v.len();
    let mut bs = match &t.kind {
        Kind::Text(s) => {
            if t.mods.regex == Some(true) || (opts.regex && t.mods.regex != Some(false)) {
                eval_regex(v, s, &t.mods, opts, cancel)?
            } else {
                eval_text(v, s, &t.mods, opts, cancel)?
            }
        }
        Kind::Regex(s) => eval_regex(v, s, &t.mods, opts, cancel)?,
        Kind::Ext(list) => {
            let list: Vec<Vec<u8>> = list.iter().map(|s| s.as_bytes().to_vec()).collect();
            pred(v, cancel, |i| {
                let e = v.ext(i as u32);
                !v.is_dir(i as u32) && list.iter().any(|x| x == e)
            })?
        }
        Kind::Size(lo, hi) => pred(v, cancel, |i| (*lo..=*hi).contains(&v.size[i]))?,
        Kind::DateModified(lo, hi) => {
            let (lo, hi) = secs_range(*lo, *hi);
            pred(v, cancel, |i| (lo..=hi).contains(&v.mtime_s[i]))?
        }
        Kind::DateCreated(lo, hi) => {
            let (lo, hi) = secs_range(*lo, *hi);
            pred(v, cancel, |i| (lo..=hi).contains(&v.ctime_s[i]))?
        }
        Kind::DateAccessed(lo, hi) => {
            let (lo, hi) = secs_range(*lo, *hi);
            pred(v, cancel, |i| (lo..=hi).contains(&v.atime_s[i]))?
        }
        Kind::Folder => pred(v, cancel, |i| v.attrs[i] & ATTR_DIRECTORY != 0)?,
        Kind::File => pred(v, cancel, |i| v.attrs[i] & ATTR_DIRECTORY == 0)?,
        Kind::Root => {
            let mut b = Bitset::zeros(n);
            if n > 0 {
                b.set(0);
            }
            b
        }
        Kind::Empty => {
            let mut children = vec![0u32; n];
            for i in 1..n {
                if v.flags[i] & FLAG_DELETED == 0 {
                    let p = parent_of(v, i);
                    if p != NONE {
                        children[p as usize] += 1;
                    }
                }
            }
            pred(v, cancel, |i| v.attrs[i] & ATTR_DIRECTORY != 0 && children[i] == 0)?
        }
        Kind::Parent(path) => match resolve_path(v, path, cancel) {
            Some(target) => pred(v, cancel, |i| parent_of(v, i) == target)?,
            None => Bitset::zeros(n),
        },
        Kind::Ancestor(path) => match resolve_path(v, path, cancel) {
            Some(target) => {
                let mut m = Bitset::zeros(n);
                m.set(target as usize);
                propagate_to_descendants(v, &m, false)
            }
            None => Bitset::zeros(n),
        },
        Kind::Child(name) => {
            let lower = lowercase(name);
            let m = scan_substring(v, lower.as_bytes(), cancel, |e, pos| pos == 0 && v.lower_name(e as u32).len() == lower.len())?;
            let mut r = Bitset::zeros(n);
            for i in m.iter_ones() {
                let p = parent_of(v, i);
                if p != NONE {
                    r.set(p as usize);
                }
            }
            r
        }
        Kind::Attrib(mask) => pred(v, cancel, |i| v.attrs[i] & mask == *mask)?,
        Kind::Len(lo, hi) => pred(v, cancel, |i| {
            let l = v.name(i as u32).chars().count() as u32;
            (*lo..=*hi).contains(&l)
        })?,
        Kind::Depth(lo, hi) => pred(v, cancel, |i| (*lo..=*hi).contains(&v.depth(i as u32)))?,
        Kind::StartWith(s) => {
            let l = lowercase(s);
            pred(v, cancel, |i| v.lower_name(i as u32).starts_with(l.as_bytes()))?
        }
        Kind::EndWith(s) => {
            let l = lowercase(s);
            pred(v, cancel, |i| v.lower_name(i as u32).ends_with(l.as_bytes()))?
        }
        Kind::Count(_) => Bitset::ones(n),
        Kind::Dupe(key) => match ctx.dupes.get(key) {
            Some(set) => pred(v, cancel, |i| set.binary_search(&dupe_hash(v, i, *key)).is_ok())?,
            None => Bitset::zeros(n),
        },
        Kind::ChildCount(lo, hi, kind) => {
            let mut counts = vec![0u32; n];
            for i in 1..n {
                if v.flags[i] & FLAG_DELETED != 0 {
                    continue;
                }
                let is_dir = v.attrs[i] & ATTR_DIRECTORY != 0;
                let include = match kind {
                    ChildKind::All => true,
                    ChildKind::Files => !is_dir,
                    ChildKind::Folders => is_dir,
                };
                let p = parent_of(v, i);
                if include && p != NONE {
                    counts[p as usize] += 1;
                }
            }
            pred(v, cancel, |i| v.attrs[i] & ATTR_DIRECTORY != 0 && (*lo..=*hi).contains(&counts[i]))?
        }
        Kind::Content(text) => {
            // 単独で現れた場合: 全ファイルが候補（上限あり）
            let all = pred(v, cancel, |i| v.attrs[i] & ATTR_DIRECTORY == 0)?;
            content_filter(v, &all, text, cancel)?
        }
        Kind::Type(text) => {
            let want = lowercase(text);
            match TYPE_NAME.get() {
                Some(f) => {
                    // 拡張子ごとの種別名をキャッシュしてから並列判定
                    let mut cache: HashMap<Vec<u8>, bool> = HashMap::new();
                    for i in 0..n {
                        if v.flags[i] & FLAG_DELETED != 0 {
                            continue;
                        }
                        let key = if v.is_dir(i as u32) { b"\\dir".to_vec() } else { v.ext(i as u32).to_vec() };
                        cache.entry(key).or_insert_with_key(|k| {
                            let (ext, is_dir) = if k == b"\\dir" { ("", true) } else { (std::str::from_utf8(k).unwrap_or(""), false) };
                            lowercase(&f(ext, is_dir)).contains(&want)
                        });
                    }
                    pred(v, cancel, |i| {
                        let key: &[u8] = if v.is_dir(i as u32) { b"\\dir" } else { v.ext(i as u32) };
                        cache.get(key).copied().unwrap_or(false)
                    })?
                }
                None => Bitset::zeros(n),
            }
        }
        Kind::Frn(lo, hi) => pred(v, cancel, |i| (*lo..=*hi).contains(&v.frn(i as u32)))?,
        Kind::Fsi(lo, hi) => {
            if (*lo..=*hi).contains(&ctx.fsi) {
                pred(v, cancel, |_| true)?
            } else {
                Bitset::zeros(n)
            }
        }
        Kind::RunCount(lo, hi) => run_history_pred(v, cancel, |h| (*lo..=*hi).contains(&h.map(|(c, _)| c).unwrap_or(0)))?,
        Kind::DateRun(lo, hi) => run_history_pred(v, cancel, |h| h.map(|(_, t)| (*lo..=*hi).contains(&t)).unwrap_or(false))?,
        Kind::Unsupported(_) => Bitset::zeros(n),
    };
    if t.mods.folder_only && !matches!(t.kind, Kind::Folder) {
        let d = pred(v, cancel, |i| v.attrs[i] & ATTR_DIRECTORY != 0)?;
        bs.and_assign(&d);
    }
    if t.mods.file_only && !matches!(t.kind, Kind::File) {
        let d = pred(v, cancel, |i| v.attrs[i] & ATTR_DIRECTORY == 0)?;
        bs.and_assign(&d);
    }
    Some(bs)
}

pub fn eval(node: &Node, v: &VolumeIndex, opts: &SearchOpts, ctx: &EvalCtx, cancel: Cancel) -> Option<Bitset> {
    let n = v.len();
    match node {
        Node::All => pred(v, cancel, |_| true),
        Node::Term(t) => eval_term(v, t, opts, ctx, cancel),
        Node::Not(inner) => {
            let mut b = eval(inner, v, opts, ctx, cancel)?;
            b.not_assign();
            let live = pred(v, cancel, |_| true)?;
            b.and_assign(&live);
            Some(b)
        }
        Node::And(items) => {
            let mut acc: Option<Bitset> = None;
            // content: は他の条件で絞ったあとに評価する（ファイルを読むので）
            for it in items.iter().filter(|it| !is_content(it)) {
                let b = eval(it, v, opts, ctx, cancel)?;
                match &mut acc {
                    None => acc = Some(b),
                    Some(a) => a.and_assign(&b),
                }
                if acc.as_ref().map(|a| a.count() == 0).unwrap_or(false) {
                    break;
                }
            }
            for it in items.iter().filter(|it| is_content(it)) {
                if let Node::Term(Term { kind: Kind::Content(text), .. }) = it {
                    let cand = match &acc {
                        Some(a) => content_filter(v, a, text, cancel)?,
                        None => eval(it, v, opts, ctx, cancel)?,
                    };
                    acc = Some(cand);
                }
            }
            acc.or_else(|| Some(Bitset::ones(n)))
        }
        Node::Or(items) => {
            let mut acc = Bitset::zeros(n);
            for it in items {
                let b = eval(it, v, opts, ctx, cancel)?;
                acc.or_assign(&b);
            }
            Some(acc)
        }
    }
}

/// FILETIME の閉区間 [lo, hi] を秒の閉区間へ（lo は切り上げ、hi は切り捨て）。
fn secs_range(lo: i64, hi: i64) -> (u32, u32) {
    use accio_index::volume::filetime_to_secs;
    let lo_s = if lo == i64::MIN { 0 } else { filetime_to_secs(lo + 9_999_999) };
    let hi_s = if hi == i64::MAX { u32::MAX } else { filetime_to_secs(hi) };
    (lo_s, hi_s)
}

/// 構文木に `count:N` があればその値。
pub fn count_limit(node: &Node) -> Option<u32> {
    match node {
        Node::Term(Term { kind: Kind::Count(c), .. }) => Some(*c),
        Node::And(v) | Node::Or(v) => v.iter().find_map(count_limit),
        Node::Not(b) => count_limit(b),
        _ => None,
    }
}
