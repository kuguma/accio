//! Everything 1.5 互換 IPC v3（名前付きパイプ `\\.\PIPE\Everything IPC Session <sid>[ (<instance>)]`）。
//!
//! 公式 SDK3（everything_sdk3 の Everything3.c）が話すプロトコルに合わせる:
//! 8 バイトのヘッダ {code:u32, size:u32} ＋ペイロード。応答コードは 200（完了）/100（続きあり）/4xx/5xx。
//! 文字列は UTF-8（NUL なし）、長さは VLQ、SIZE_T は検索フラグ 0x4000 で u64/u32 を切り替える。
//! 1 接続につき 1 スレッド。要求は接続内で直列。

use accio_index::Index;
use accio_query::{search, Request, SearchOpts, Sort, SortKey};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT};
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::Threading::GetCurrentProcessId;

const PIPE_ACCESS_DUPLEX: u32 = 0x3;

// コマンド
const CMD_GET_IPC_PIPE_VERSION: u32 = 0;
const CMD_GET_MAJOR_VERSION: u32 = 1;
const CMD_GET_MINOR_VERSION: u32 = 2;
const CMD_GET_REVISION: u32 = 3;
const CMD_GET_BUILD_NUMBER: u32 = 4;
const CMD_GET_TARGET_MACHINE: u32 = 5;
const CMD_FIND_PROPERTY_FROM_NAME: u32 = 6;
const CMD_SEARCH: u32 = 7;
const CMD_IS_DB_LOADED: u32 = 8;
const CMD_IS_PROPERTY_INDEXED: u32 = 9;
const CMD_IS_PROPERTY_FAST_SORT: u32 = 10;
const CMD_GET_PROPERTY_NAME: u32 = 11;
const CMD_GET_PROPERTY_CANONICAL_NAME: u32 = 12;
const CMD_GET_PROPERTY_TYPE: u32 = 13;
const CMD_IS_RESULT_CHANGE: u32 = 14;
const CMD_GET_RUN_COUNT: u32 = 15;
const CMD_SET_RUN_COUNT: u32 = 16;
const CMD_INC_RUN_COUNT: u32 = 17;
const CMD_GET_FOLDER_SIZE: u32 = 18;
const CMD_GET_FILE_ATTRIBUTES: u32 = 19;
const CMD_GET_FILE_ATTRIBUTES_EX: u32 = 20;
const CMD_GET_FIND_FIRST_FILE: u32 = 21;
const CMD_GET_RESULTS: u32 = 22;
const CMD_SORT: u32 = 23;
const CMD_WAIT_FOR_RESULT_CHANGE: u32 = 24;
const CMD_IS_PROPERTY_RIGHT_ALIGNED: u32 = 25;
const CMD_IS_PROPERTY_SORT_DESCENDING: u32 = 26;
const CMD_GET_PROPERTY_DEFAULT_WIDTH: u32 = 27;

// 応答
const RSP_OK: u32 = 200;
const RSP_BAD_REQUEST: u32 = 400;
const RSP_NOT_FOUND: u32 = 404;
const RSP_INVALID_COMMAND: u32 = 501;

// 検索フラグ
const F_MATCH_CASE: u32 = 0x1;
const F_MATCH_WHOLEWORD: u32 = 0x2;
const F_MATCH_PATH: u32 = 0x4;
const F_REGEX: u32 = 0x8;
const F_MATCH_DIACRITICS: u32 = 0x10;
const F_FOLDERS_FIRST_MASK: u32 = 0x600;
const F_TOTAL_SIZE: u32 = 0x800;
const F_SORT_MIX: u32 = 0x2000;
const F_64BIT: u32 = 0x4000;

// プロパティ
const P_NAME: u32 = 0;
const P_PATH: u32 = 1;
const P_SIZE: u32 = 2;
const P_EXTENSION: u32 = 3;
const P_TYPE: u32 = 4;
const P_DATE_MODIFIED: u32 = 5;
const P_DATE_CREATED: u32 = 6;
const P_DATE_ACCESSED: u32 = 7;
const P_ATTRIBUTES: u32 = 8;
const P_DATE_RECENTLY_CHANGED: u32 = 9;
const P_RUN_COUNT: u32 = 10;
const P_DATE_RUN: u32 = 11;
const P_FILE_LIST_NAME: u32 = 12;
const P_SIZE_ON_DISK: u32 = 41;
const P_NAME_LENGTH: u32 = 48;
const P_FULL_PATH_LENGTH: u32 = 49;
const P_FULL_PATH: u32 = 240;
const P_INVALID: u32 = 0xFFFF_FFFF;

// 値型
const VT_NULL: u8 = 0;
const VT_DWORD: u8 = 3;
const VT_UINT64: u8 = 5;
const VT_PSTRING_STRING_REFERENCE: u8 = 10;
const VT_SIZE_T: u8 = 11;
const VT_PSTRING_FOLDER_REFERENCE: u8 = 14;
const VT_PSTRING_FILE_OR_FOLDER_REFERENCE: u8 = 15;

const PROPERTIES: &[(u32, &str, &str, u8)] = &[
    (P_NAME, "Name", "name", VT_PSTRING_STRING_REFERENCE),
    (P_PATH, "Path", "path", VT_PSTRING_FOLDER_REFERENCE),
    (P_SIZE, "Size", "size", VT_UINT64),
    (P_EXTENSION, "Extension", "extension", VT_PSTRING_STRING_REFERENCE),
    (P_TYPE, "Type", "type", VT_PSTRING_STRING_REFERENCE),
    (P_DATE_MODIFIED, "Date Modified", "date-modified", VT_UINT64),
    (P_DATE_CREATED, "Date Created", "date-created", VT_UINT64),
    (P_DATE_ACCESSED, "Date Accessed", "date-accessed", VT_UINT64),
    (P_ATTRIBUTES, "Attributes", "attributes", VT_DWORD),
    (P_DATE_RECENTLY_CHANGED, "Date Recently Changed", "date-recently-changed", VT_UINT64),
    (P_RUN_COUNT, "Run Count", "run-count", VT_DWORD),
    (P_DATE_RUN, "Date Run", "date-run", VT_UINT64),
    (P_FILE_LIST_NAME, "File List Name", "file-list-name", VT_PSTRING_STRING_REFERENCE),
    (P_SIZE_ON_DISK, "Size on Disk", "size-on-disk", VT_UINT64),
    (P_NAME_LENGTH, "Name Length", "name-length", VT_SIZE_T),
    (P_FULL_PATH_LENGTH, "Full Path Length", "full-path-length", VT_SIZE_T),
    (P_FULL_PATH, "Full Path", "full-path", VT_PSTRING_FILE_OR_FOLDER_REFERENCE),
];

fn prop(id: u32) -> Option<&'static (u32, &'static str, &'static str, u8)> {
    PROPERTIES.iter().find(|p| p.0 == id)
}

pub struct Ipc3Context {
    pub index: Arc<Index>,
    pub db_loaded: Arc<AtomicBool>,
    pub generation: Arc<AtomicU64>,
    pub journal: Arc<parking_lot::Mutex<crate::engine::Journal>>,
}

const CMD_GET_JOURNAL_INFO: u32 = 28;
const CMD_READ_JOURNAL: u32 = 29;

const RJ_CHANGE_ID: u32 = 0x1;
const RJ_TIMESTAMP: u32 = 0x2;
const RJ_SOURCE_TIMESTAMP: u32 = 0x4;
const RJ_OLD_PARENT_DM: u32 = 0x8;
const RJ_OLD_PATH: u32 = 0x10;
const RJ_OLD_NAME: u32 = 0x20;
const RJ_SIZE: u32 = 0x40;
const RJ_DATE_CREATED: u32 = 0x80;
const RJ_DATE_MODIFIED: u32 = 0x100;
const RJ_DATE_ACCESSED: u32 = 0x200;
const RJ_ATTRIBUTES: u32 = 0x400;
const RJ_NEW_PARENT_DM: u32 = 0x800;
const RJ_NEW_PATH: u32 = 0x1000;
const RJ_NEW_NAME: u32 = 0x2000;

/// SDK3 の `_everything3_read_journal` が読む順序で 1 件を符号化する。
fn encode_change(o: &mut Vec<u8>, id: u64, ts: i64, c: &accio_index::update::Change, flags: u32) {
    use accio_index::update::*;
    o.push(c.kind);
    if flags & RJ_CHANGE_ID != 0 {
        put_u64(o, id);
    }
    if flags & RJ_TIMESTAMP != 0 {
        put_u64(o, ts as u64);
    }
    if flags & RJ_SOURCE_TIMESTAMP != 0 {
        put_u64(o, ts as u64);
    }
    if flags & RJ_OLD_PARENT_DM != 0 {
        put_u64(o, u64::MAX);
    }
    if flags & RJ_OLD_PATH != 0 {
        put_str(o, &c.old_path);
    }
    if flags & RJ_OLD_NAME != 0 {
        put_str(o, &c.old_name);
    }
    let has_props = matches!(c.kind, CH_FILE_CREATE | CH_FILE_MODIFY | CH_FILE_RENAME | CH_FILE_MOVE | CH_FOLDER_CREATE | CH_FOLDER_MODIFY | CH_FOLDER_RENAME | CH_FOLDER_MOVE);
    if has_props {
        if matches!(c.kind, CH_FILE_CREATE | CH_FILE_MODIFY | CH_FILE_RENAME | CH_FILE_MOVE) && flags & RJ_SIZE != 0 {
            put_u64(o, c.size);
        }
        if flags & RJ_DATE_CREATED != 0 {
            put_u64(o, c.ctime as u64);
        }
        if flags & RJ_DATE_MODIFIED != 0 {
            put_u64(o, c.mtime as u64);
        }
        if flags & RJ_DATE_ACCESSED != 0 {
            put_u64(o, u64::MAX);
        }
        if flags & RJ_ATTRIBUTES != 0 {
            put_u32(o, c.attrs);
        }
    }
    match c.kind {
        CH_FILE_RENAME | CH_FOLDER_RENAME => {
            if flags & RJ_NEW_NAME != 0 {
                put_str(o, &c.new_name);
            }
        }
        CH_FILE_MOVE | CH_FOLDER_MOVE => {
            if flags & RJ_NEW_PARENT_DM != 0 {
                put_u64(o, u64::MAX);
            }
            if flags & RJ_NEW_PATH != 0 {
                put_str(o, &c.new_path);
            }
            if flags & RJ_NEW_NAME != 0 {
                put_str(o, &c.new_name);
            }
        }
        _ => {}
    }
}

// ---------- 符号化 ----------

struct Reader<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Reader<'a> {
    fn u8(&mut self) -> Option<u8> {
        let v = *self.b.get(self.p)?;
        self.p += 1;
        Some(v)
    }
    fn u16(&mut self) -> Option<u16> {
        let s = self.b.get(self.p..self.p + 2)?;
        self.p += 2;
        Some(u16::from_le_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        let s = self.b.get(self.p..self.p + 4)?;
        self.p += 4;
        Some(u32::from_le_bytes(s.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        let s = self.b.get(self.p..self.p + 8)?;
        self.p += 8;
        Some(u64::from_le_bytes(s.try_into().ok()?))
    }
    fn vlq(&mut self) -> Option<u64> {
        let a = self.u8()? as u64;
        if a < 0xFF {
            return Some(a);
        }
        let b = self.u16()? as u64;
        if b < 0xFFFF {
            return Some(0xFF + b);
        }
        let c = self.u32()? as u64;
        if c < 0xFFFF_FFFF {
            return Some(0xFF + 0xFFFF + c);
        }
        let d = self.u64()?;
        Some(0xFF + 0xFFFF + 0xFFFF_FFFF + d)
    }
    fn size_t(&mut self, wide: bool) -> Option<u64> {
        if wide {
            self.u64()
        } else {
            self.u32().map(|v| v as u64)
        }
    }
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.p..self.p + n)?;
        self.p += n;
        Some(s)
    }
}

fn put_u32(o: &mut Vec<u8>, v: u32) {
    o.extend_from_slice(&v.to_le_bytes());
}
fn put_u64(o: &mut Vec<u8>, v: u64) {
    o.extend_from_slice(&v.to_le_bytes());
}
fn put_vlq(o: &mut Vec<u8>, mut v: u64) {
    if v < 0xFF {
        o.push(v as u8);
        return;
    }
    o.push(0xFF);
    v -= 0xFF;
    if v < 0xFFFF {
        o.extend_from_slice(&(v as u16).to_le_bytes());
        return;
    }
    o.extend_from_slice(&0xFFFFu16.to_le_bytes());
    v -= 0xFFFF;
    if v < 0xFFFF_FFFF {
        o.extend_from_slice(&(v as u32).to_le_bytes());
        return;
    }
    o.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    v -= 0xFFFF_FFFF;
    o.extend_from_slice(&v.to_le_bytes());
}
fn put_str(o: &mut Vec<u8>, s: &str) {
    put_vlq(o, s.len() as u64);
    o.extend_from_slice(s.as_bytes());
}
fn put_size_t(o: &mut Vec<u8>, v: u64, wide: bool) {
    if wide {
        put_u64(o, v)
    } else {
        put_u32(o, v.min(u32::MAX as u64) as u32)
    }
}

// ---------- 検索 ----------

struct SearchPacket {
    flags: u32,
    text: String,
    offset: u64,
    count: u64,
    sorts: Vec<(u32, u32)>,
    props: Vec<(u32, u32)>,
}

fn parse_search(b: &[u8]) -> Option<SearchPacket> {
    let mut r = Reader { b, p: 0 };
    let flags = r.u32()?;
    let wide = flags & F_64BIT != 0;
    let len = r.vlq()? as usize;
    let text = String::from_utf8_lossy(r.bytes(len)?).into_owned();
    let offset = r.size_t(wide)?;
    let count = r.size_t(wide)?;
    let ns = r.vlq()? as usize;
    let mut sorts = Vec::with_capacity(ns.min(16));
    for _ in 0..ns {
        sorts.push((r.u32()?, r.u32()?));
    }
    let np = r.vlq()? as usize;
    let mut props = Vec::with_capacity(np.min(64));
    for _ in 0..np {
        props.push((r.u32()?, r.u32()?));
    }
    Some(SearchPacket { flags, text, offset, count, sorts, props })
}

fn sort_from(sorts: &[(u32, u32)]) -> Sort {
    let (id, f) = sorts.first().copied().unwrap_or((P_NAME, 0));
    let key = match id {
        P_PATH | P_FULL_PATH => SortKey::Path,
        P_SIZE | P_SIZE_ON_DISK => SortKey::Size,
        P_EXTENSION => SortKey::Extension,
        P_DATE_MODIFIED | P_DATE_RECENTLY_CHANGED => SortKey::DateModified,
        P_DATE_CREATED => SortKey::DateCreated,
        P_DATE_ACCESSED => SortKey::DateAccessed,
        P_RUN_COUNT => SortKey::RunCount,
        P_DATE_RUN => SortKey::DateRun,
        _ => SortKey::Name,
    };
    Sort { key, ascending: f & 1 == 0 }
}

fn fmt_size(n: u64) -> String {
    let mut s = n.to_string();
    let mut i = s.len();
    while i > 3 {
        i -= 3;
        s.insert(i, ',');
    }
    s
}

fn build_search_reply(ctx: &Ipc3Context, pk: &SearchPacket) -> Vec<u8> {
    let wide = pk.flags & F_64BIT != 0;
    let opts = SearchOpts {
        match_case: pk.flags & F_MATCH_CASE != 0,
        whole_word: pk.flags & F_MATCH_WHOLEWORD != 0,
        match_path: pk.flags & F_MATCH_PATH != 0,
        regex: pk.flags & F_REGEX != 0,
        match_diacritics: pk.flags & F_MATCH_DIACRITICS != 0,
    };
    let vols = ctx.index.snapshot();
    let req = Request { query: pk.text.clone(), opts, sort: sort_from(&pk.sorts), index_generation: ctx.generation.load(Ordering::Relaxed).max(1) };
    let mut res = search(&vols, &req, &|| false).unwrap_or_default();
    // FOLDERS_FIRST（既定: 昇順ソート時はフォルダ先頭）。SORT_MIX ならそのまま
    if pk.flags & F_SORT_MIX == 0 {
        let mode = pk.flags & F_FOLDERS_FIRST_MASK;
        let folders_first = match mode {
            0x0 => req.sort.ascending,
            0x200 => true,
            0x400 => false,
            _ => !req.sort.ascending,
        };
        // フォルダ優先: 安定な分割（並べ替えではなく O(N)）。ロックはボリュームごとに 1 回
        let guards: Vec<_> = vols.iter().map(|v| v.read()).collect();
        let is_dir = |h: &accio_query::Hit| guards.get(h.vol as usize).map(|g| (h.id as usize) < g.len() && g.is_dir(h.id)).unwrap_or(false);
        let (first, second): (Vec<_>, Vec<_>) = res.hits.iter().partition(|h| is_dir(h) == folders_first);
        res.hits = first;
        res.hits.extend(second);
    }
    let needles = accio_query::highlight_needles(&pk.text);
    let total_size: u64 = if pk.flags & F_TOTAL_SIZE != 0 {
        let guards: Vec<_> = vols.iter().map(|v| v.read()).collect();
        res.hits
            .iter()
            .map(|h| guards.get(h.vol as usize).map(|g| if (h.id as usize) >= g.len() || g.is_dir(h.id) { 0 } else { g.size[h.id as usize] }).unwrap_or(0))
            .sum()
    } else {
        0
    };
    let start = (pk.offset as usize).min(res.hits.len());
    let end = start.saturating_add(pk.count.min(usize::MAX as u64) as usize).min(res.hits.len());
    let mut o = Vec::new();
    put_u32(&mut o, pk.flags);
    put_size_t(&mut o, res.folders as u64, wide);
    put_size_t(&mut o, res.files as u64, wide);
    if pk.flags & F_TOTAL_SIZE != 0 {
        put_u64(&mut o, total_size);
    }
    put_size_t(&mut o, start as u64, wide);
    put_size_t(&mut o, (end - start) as u64, wide);
    put_vlq(&mut o, pk.sorts.len() as u64);
    for (id, f) in &pk.sorts {
        put_u32(&mut o, *id);
        put_u32(&mut o, *f);
    }
    put_vlq(&mut o, pk.props.len() as u64);
    for (id, f) in &pk.props {
        put_u32(&mut o, *id);
        put_u32(&mut o, *f);
        let vt = if *f & 0x3 != 0 { VT_PSTRING_STRING_REFERENCE } else { prop(*id).map(|p| p.3).unwrap_or(VT_NULL) };
        o.push(vt);
    }
    for h in &res.hits[start..end] {
        let Some(v) = vols.get(h.vol as usize) else { continue };
        let g = v.read();
        let is_dir = g.is_dir(h.id);
        let mut flags = 0u8;
        if is_dir {
            flags |= 1;
        }
        if h.id == 0 {
            flags |= 2;
        }
        o.push(flags);
        for (id, f) in &pk.props {
            let formatted = *f & 0x3 != 0;
            let highlight = *f & 0x2 != 0;
            match *id {
                P_NAME => {
                    let n = g.name(h.id);
                    if highlight {
                        put_str(&mut o, &crate::ipc::highlight_markup(n, &needles));
                    } else {
                        put_str(&mut o, n);
                    }
                }
                P_PATH => put_str(&mut o, &g.parent_path(h.id)),
                P_FULL_PATH => put_str(&mut o, &g.full_path(h.id)),
                P_EXTENSION => put_str(&mut o, &String::from_utf8_lossy(g.ext(h.id))),
                P_TYPE => put_str(&mut o, &crate::shell::type_name(&String::from_utf8_lossy(g.ext(h.id)), is_dir)),
                P_FILE_LIST_NAME => put_str(&mut o, ""),
                P_SIZE | P_SIZE_ON_DISK => {
                    if formatted {
                        put_str(&mut o, &fmt_size(g.size[h.id as usize]));
                    } else {
                        put_u64(&mut o, g.size[h.id as usize]);
                    }
                }
                P_DATE_MODIFIED | P_DATE_RECENTLY_CHANGED => {
                    if formatted {
                        put_str(&mut o, &accio_query::datetime::format_filetime(g.mtime(h.id)));
                    } else {
                        put_u64(&mut o, g.mtime(h.id) as u64);
                    }
                }
                P_DATE_CREATED => {
                    if formatted {
                        put_str(&mut o, &accio_query::datetime::format_filetime(g.ctime(h.id)));
                    } else {
                        put_u64(&mut o, g.ctime(h.id) as u64);
                    }
                }
                P_DATE_ACCESSED => {
                    if formatted {
                        put_str(&mut o, &accio_query::datetime::format_filetime(g.atime(h.id)));
                    } else {
                        put_u64(&mut o, g.atime(h.id) as u64);
                    }
                }
                P_DATE_RUN => {
                    let dr = accio_query::run_history_get(&g.full_path(h.id)).map(|(_, t)| t).unwrap_or(0);
                    if formatted {
                        put_str(&mut o, &if dr > 0 { accio_query::datetime::format_filetime(dr) } else { String::new() });
                    } else {
                        put_u64(&mut o, dr as u64);
                    }
                }
                P_ATTRIBUTES => {
                    if formatted {
                        put_str(&mut o, &format!("{}", g.attrs[h.id as usize]));
                    } else {
                        put_u32(&mut o, g.attrs[h.id as usize]);
                    }
                }
                P_RUN_COUNT => {
                    let rc = accio_query::run_history_get(&g.full_path(h.id)).map(|(c, _)| c).unwrap_or(0);
                    if formatted {
                        put_str(&mut o, &rc.to_string());
                    } else {
                        put_u32(&mut o, rc);
                    }
                }
                P_NAME_LENGTH => {
                    let n = g.name(h.id).chars().count() as u64;
                    if formatted {
                        put_str(&mut o, &n.to_string());
                    } else {
                        put_size_t(&mut o, n, wide);
                    }
                }
                P_FULL_PATH_LENGTH => {
                    let n = g.full_path(h.id).chars().count() as u64;
                    if formatted {
                        put_str(&mut o, &n.to_string());
                    } else {
                        put_size_t(&mut o, n, wide);
                    }
                }
                _ => {
                    if formatted {
                        put_str(&mut o, "");
                    }
                }
            }
        }
    }
    o
}

/// `c:\foo\bar` をエントリへ解決する（どのボリュームでも）。
fn resolve(ctx: &Ipc3Context, path: &str) -> Option<(accio_index::SharedVolume, u32)> {
    let lower = accio_index::volume::lowercase(path.trim_end_matches('\\'));
    for v in ctx.index.snapshot() {
        let g = v.read();
        if let Some(id) = accio_query::eval::resolve_path(&g, &lower, &|| false) {
            drop(g);
            return Some((v, id));
        }
        // ファイルも引く: 親を解決して直下の名前を比較
        if let Some((parent, name)) = lower.rsplit_once('\\') {
            if let Some(pid) = accio_query::eval::resolve_path(&g, parent, &|| false) {
                for i in 0..g.len() as u32 {
                    if g.parent[i as usize] == pid && !g.is_deleted(i) && g.lower_name(i) == name.as_bytes() {
                        drop(g);
                        return Some((v, i));
                    }
                }
            }
        }
    }
    None
}

fn find_data(g: &accio_index::VolumeIndex, id: u32) -> Vec<u8> {
    let mut o = Vec::new();
    put_u64(&mut o, g.ctime(id) as u64);
    put_u64(&mut o, u64::MAX); // accessed: 未索引
    put_u64(&mut o, g.mtime(id) as u64);
    put_u64(&mut o, if g.is_dir(id) { u64::MAX } else { g.size[id as usize] });
    put_u32(&mut o, g.attrs[id as usize]);
    o
}

fn handle(ctx: &Ipc3Context, code: u32, body: &[u8]) -> (u32, Vec<u8>) {
    let s = |v: u32| (RSP_OK, v.to_le_bytes().to_vec());
    let prop_id = || Reader { b: body, p: 0 }.u32();
    let text = || String::from_utf8_lossy(body).into_owned();
    match code {
        CMD_GET_IPC_PIPE_VERSION => s(1),
        CMD_GET_MAJOR_VERSION => s(1),
        CMD_GET_MINOR_VERSION => s(5),
        CMD_GET_REVISION => s(0),
        CMD_GET_BUILD_NUMBER => s(1397),
        CMD_GET_TARGET_MACHINE => s(2),
        CMD_FIND_PROPERTY_FROM_NAME => {
            let name = text().to_ascii_lowercase();
            let id = PROPERTIES.iter().find(|p| p.2 == name || p.1.eq_ignore_ascii_case(&name)).map(|p| p.0).unwrap_or(P_INVALID);
            s(id)
        }
        CMD_SEARCH | CMD_GET_RESULTS | CMD_SORT => match parse_search(body) {
            Some(pk) => (RSP_OK, build_search_reply(ctx, &pk)),
            None => (RSP_BAD_REQUEST, Vec::new()),
        },
        CMD_IS_DB_LOADED => s(ctx.db_loaded.load(Ordering::Relaxed) as u32),
        CMD_IS_PROPERTY_INDEXED => s(matches!(prop_id(), Some(P_NAME | P_PATH | P_SIZE | P_EXTENSION | P_DATE_MODIFIED | P_DATE_CREATED | P_ATTRIBUTES | P_FULL_PATH | P_SIZE_ON_DISK | P_NAME_LENGTH | P_FULL_PATH_LENGTH)) as u32),
        CMD_IS_PROPERTY_FAST_SORT => s(matches!(prop_id(), Some(P_NAME | P_SIZE | P_DATE_MODIFIED | P_DATE_CREATED | P_EXTENSION)) as u32),
        CMD_GET_PROPERTY_NAME => match prop_id().and_then(prop) {
            Some(p) => (RSP_OK, p.1.as_bytes().to_vec()),
            None => (RSP_NOT_FOUND, Vec::new()),
        },
        CMD_GET_PROPERTY_CANONICAL_NAME => match prop_id().and_then(prop) {
            Some(p) => (RSP_OK, p.2.as_bytes().to_vec()),
            None => (RSP_NOT_FOUND, Vec::new()),
        },
        CMD_GET_PROPERTY_TYPE => s(if prop_id().and_then(prop).is_some() { 2 } else { 0 }),
        CMD_IS_RESULT_CHANGE => s(0),
        CMD_GET_RUN_COUNT | CMD_INC_RUN_COUNT => s(0),
        CMD_SET_RUN_COUNT => (RSP_OK, Vec::new()),
        CMD_GET_FOLDER_SIZE => match resolve(ctx, &text()) {
            Some((v, id)) => {
                let g = v.read();
                (RSP_OK, g.size[id as usize].to_le_bytes().to_vec())
            }
            None => (RSP_OK, u64::MAX.to_le_bytes().to_vec()),
        },
        CMD_GET_FILE_ATTRIBUTES => match resolve(ctx, &text()) {
            Some((v, id)) => s(v.read().attrs[id as usize]),
            None => s(u32::MAX),
        },
        CMD_GET_FILE_ATTRIBUTES_EX => match resolve(ctx, &text()) {
            Some((v, id)) => {
                let g = v.read();
                let mut o = find_data(&g, id);
                o.extend_from_slice(g.name(id).as_bytes());
                (RSP_OK, o)
            }
            None => (RSP_NOT_FOUND, Vec::new()),
        },
        CMD_GET_FIND_FIRST_FILE => match resolve(ctx, &text()) {
            Some((v, id)) => {
                let g = v.read();
                let mut o = Vec::new();
                if g.is_dir(id) {
                    for i in 0..g.len() as u32 {
                        if g.parent[i as usize] == id && !g.is_deleted(i) {
                            o.extend_from_slice(&find_data(&g, i));
                            put_str(&mut o, g.name(i));
                        }
                    }
                }
                (RSP_OK, o)
            }
            None => (RSP_NOT_FOUND, Vec::new()),
        },
        CMD_WAIT_FOR_RESULT_CHANGE => {
            let start = ctx.generation.load(Ordering::Relaxed);
            let t = std::time::Instant::now();
            while ctx.generation.load(Ordering::Relaxed) == start && t.elapsed() < std::time::Duration::from_secs(60) {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            s((ctx.generation.load(Ordering::Relaxed) != start) as u32)
        }
        CMD_IS_PROPERTY_RIGHT_ALIGNED => s(matches!(prop_id(), Some(P_SIZE | P_SIZE_ON_DISK | P_RUN_COUNT | P_NAME_LENGTH | P_FULL_PATH_LENGTH)) as u32),
        CMD_IS_PROPERTY_SORT_DESCENDING => s(matches!(prop_id(), Some(P_SIZE | P_DATE_MODIFIED | P_DATE_CREATED | P_DATE_ACCESSED | P_RUN_COUNT | P_DATE_RUN)) as u32),
        CMD_GET_PROPERTY_DEFAULT_WIDTH => s(match prop_id() {
            Some(P_NAME) => 300,
            Some(P_PATH | P_FULL_PATH) => 400,
            _ => 100,
        }),
        CMD_GET_JOURNAL_INFO => {
            let j = ctx.journal.lock();
            let mut o = Vec::with_capacity(40);
            put_u64(&mut o, j.id);
            put_u64(&mut o, j.first);
            put_u64(&mut o, j.next);
            put_u64(&mut o, (j.entries.len() * 64) as u64);
            put_u64(&mut o, (j.max_entries * 64) as u64);
            (RSP_OK, o)
        }
        _ => (RSP_INVALID_COMMAND, Vec::new()),
    }
}

// ---------- パイプ ----------

struct Pipe(windows_sys::Win32::Foundation::HANDLE);
unsafe impl Send for Pipe {}
impl Drop for Pipe {
    fn drop(&mut self) {
        // SAFETY: 所有しているハンドル。
        unsafe {
            DisconnectNamedPipe(self.0);
            CloseHandle(self.0);
        }
    }
}
impl Read for Pipe {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut n = 0u32;
        // SAFETY: バッファと長さを渡す同期読み取り。
        let ok = unsafe { ReadFile(self.0, buf.as_mut_ptr(), buf.len() as u32, &mut n, std::ptr::null_mut()) };
        if ok == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(n as usize)
        }
    }
}
impl Write for Pipe {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut n = 0u32;
        // SAFETY: バッファと長さを渡す同期書き込み。
        let ok = unsafe { WriteFile(self.0, buf.as_ptr(), buf.len() as u32, &mut n, std::ptr::null_mut()) };
        if ok == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(n as usize)
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// パイプ用のセキュリティ記述子（SDDL）: Everyone と AppContainer に汎用アクセス、低完全性からの書き込みを許可。
fn pipe_security_descriptor() -> Option<*mut core::ffi::c_void> {
    use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
    let sddl: Vec<u16> = "D:(A;;GA;;;WD)(A;;GA;;;AC)S:(ML;;NW;;;LW)".encode_utf16().chain(std::iter::once(0)).collect();
    let mut sd: *mut core::ffi::c_void = std::ptr::null_mut();
    // SAFETY: NUL 終端の SDDL。戻り値を検査する。記述子は LocalAlloc され、プロセスの寿命いっぱい使う。
    let ok = unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), 1, &mut sd as *mut _ as *mut _, std::ptr::null_mut()) };
    if ok == 0 || sd.is_null() {
        log::warn!("パイプのセキュリティ記述子を作れない: {}", std::io::Error::last_os_error());
        return None;
    }
    Some(sd)
}

fn serve(ctx: Arc<Ipc3Context>, mut pipe: Pipe) {
    loop {
        let mut hdr = [0u8; 8];
        if pipe.read_exact(&mut hdr).is_err() {
            return;
        }
        let code = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
        let size = u32::from_le_bytes(hdr[4..8].try_into().unwrap()) as usize;
        if size > 64 << 20 {
            return;
        }
        let mut body = vec![0u8; size];
        if size > 0 && pipe.read_exact(&mut body).is_err() {
            return;
        }
        if code == CMD_READ_JOURNAL {
            // 終わりのないストリーム: 既存分を 100 で送り、以後は新しい変更が来るたびに 100 で送る
            let mut r = Reader { b: &body, p: 0 };
            let (Some(jid), Some(mut change_id), Some(flags)) = (r.u64(), r.u64(), r.u32()) else {
                let mut out = Vec::new();
                put_u32(&mut out, RSP_BAD_REQUEST);
                put_u32(&mut out, 0);
                let _ = pipe.write_all(&out);
                continue;
            };
            if jid != ctx.journal.lock().id {
                let mut out = Vec::new();
                put_u32(&mut out, RSP_BAD_REQUEST);
                put_u32(&mut out, 0);
                let _ = pipe.write_all(&out);
                continue;
            }
            if change_id == u64::MAX {
                change_id = ctx.journal.lock().next;
            }
            loop {
                let mut payload = Vec::new();
                {
                    let j = ctx.journal.lock();
                    for (id, ts, c) in j.entries.iter() {
                        if *id >= change_id {
                            encode_change(&mut payload, *id, *ts, c, flags);
                            change_id = *id + 1;
                        }
                    }
                }
                if !payload.is_empty() {
                    let mut out = Vec::with_capacity(8 + payload.len());
                    put_u32(&mut out, 100);
                    put_u32(&mut out, payload.len() as u32);
                    out.extend_from_slice(&payload);
                    if pipe.write_all(&out).is_err() {
                        return;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        }
        let (rsp, payload) = handle(&ctx, code, &body);
        let mut out = Vec::with_capacity(8 + payload.len());
        put_u32(&mut out, rsp);
        put_u32(&mut out, payload.len() as u32);
        out.extend_from_slice(&payload);
        if pipe.write_all(&out).is_err() {
            return;
        }
    }
}

fn escape_instance(i: &str) -> String {
    let mut o = String::new();
    for c in i.chars() {
        match c {
            '%' | ':' | '\\' => o.push_str(&format!("%{:02X}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

/// パイプ名（セッション付き）。
pub fn pipe_name(instance: &Option<String>) -> String {
    // SAFETY: 自プロセス ID の照会。
    let mut sid = 0u32;
    unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut sid) };
    let mut name = format!("\\\\.\\PIPE\\Everything IPC Session {sid}");
    if let Some(i) = instance {
        if !i.is_empty() {
            name.push_str(&format!(" ({})", escape_instance(i)));
        }
    }
    name
}

/// リスナースレッドを開始する。1 接続ごとにハンドラスレッドを起こす。
pub fn start(instance: Option<String>, ctx: Arc<Ipc3Context>) {
    let name = pipe_name(&instance);
    let wname: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    std::thread::Builder::new()
        .name("ipc3".into())
        .spawn(move || {
            log::info!("IPC v3 パイプ {name}");
            // 昇格プロセスが作るパイプは既定では非昇格のクライアントが開けない（error 5）。
            // 全ユーザーに読み書きを許し、完全性レベルの壁も下げる（Everything のサービスと同じ）
            let sd = pipe_security_descriptor();
            let sa = windows_sys::Win32::Security::SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<windows_sys::Win32::Security::SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: sd.map(|p| p as *mut _).unwrap_or(std::ptr::null_mut()),
                bInheritHandle: 0,
            };
            loop {
                // SAFETY: NUL 終端のパイプ名。バイトモード、同期。
                let h = unsafe {
                    CreateNamedPipeW(
                        wname.as_ptr(),
                        PIPE_ACCESS_DUPLEX,
                        PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                        PIPE_UNLIMITED_INSTANCES,
                        1 << 20,
                        1 << 20,
                        0,
                        if sd.is_some() { &sa } else { std::ptr::null() },
                    )
                };
                if h == INVALID_HANDLE_VALUE {
                    log::warn!("IPC v3 パイプを作成できない: {}", std::io::Error::last_os_error());
                    std::thread::sleep(std::time::Duration::from_secs(10));
                    continue;
                }
                let ok = unsafe { ConnectNamedPipe(h, std::ptr::null_mut()) };
                if ok == 0 && std::io::Error::last_os_error().raw_os_error() != Some(535) {
                    unsafe { CloseHandle(h) };
                    continue;
                }
                let ctx = ctx.clone();
                let pipe = Pipe(h);
                std::thread::spawn(move || serve(ctx, pipe));
            }
        })
        .ok();
}
