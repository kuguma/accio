//! Everything 互換 IPC（1.4 の WM_COPYDATA プロトコル）。
//!
//! `EVERYTHING_TASKBAR_NOTIFICATION` クラスの不可視ウィンドウを専用スレッドで持ち、
//! `EVERYTHING_IPC_QUERYW`（v1）と `EVERYTHING_IPC_QUERY2`（v2）に応答する。
//! `WM_USER` の照会（バージョン、DB ロード状態、IS_ADMIN など）にも答える。
//! これにより es.exe・Everything SDK（Everything64.dll）・各種連携ツールがそのまま動く。

use accio_index::Index;
use accio_query::{search, Request, SearchOpts, Sort, SortKey};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::DataExchange::COPYDATASTRUCT;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    ChangeWindowMessageFilterEx, CreateWindowExW, DefWindowProcW, DispatchMessageW, FindWindowW, GetMessageW, RegisterClassW,
    SendMessageW, TranslateMessage, MSG, MSGFLT_ALLOW, WM_CLOSE, WM_COPYDATA, WM_USER, WNDCLASSW,
};

/// accio 独自: 二重起動時のコマンドライン転送（UTF-8 の引数列、NUL 区切り）
const COPYDATA_ACCIO_ARGS: usize = 0x4143_4349; // 'ACCI'

#[derive(Debug, Clone)]
pub enum ExternalCommand {
    Search(String),
    Show,
    Toggle,
    Exit,
    Reindex,
    SaveDb,
    /// 新しいタブで検索する（-new-tab [-s query]）
    NewTab(String),
}

/// 引数列をコマンドに変換する（起動中インスタンスが受け取った側で解釈）。
pub fn commands_from_args(args: &[String]) -> Vec<ExternalCommand> {
    let mut out = Vec::new();
    let mut it = args.iter();
    let mut any_search = false;
    let new_tab = args.iter().any(|a| a.eq_ignore_ascii_case("-new-tab") || a.eq_ignore_ascii_case("-newtab"));
    while let Some(a) = it.next() {
        match a.to_ascii_lowercase().as_str() {
            "-search" | "-s" => {
                if let Some(q) = it.next() {
                    out.push(if new_tab { ExternalCommand::NewTab(q.clone()) } else { ExternalCommand::Search(q.clone()) });
                    any_search = true;
                }
            }
            "-url" => {
                if let Some(u) = it.next() {
                    let q = u.strip_prefix("es:").or_else(|| u.strip_prefix("ES:")).unwrap_or(u).trim_start_matches("//");
                    out.push(ExternalCommand::Search(crate::url_decode(q)));
                    any_search = true;
                }
            }
            "-new-tab" | "-newtab" => {
                if !args.iter().any(|a| a.eq_ignore_ascii_case("-s") || a.eq_ignore_ascii_case("-search")) {
                    out.push(ExternalCommand::NewTab(String::new()));
                    any_search = true;
                }
            }
            "-toggle-window" => out.push(ExternalCommand::Toggle),
            "-exit" | "-quit" => out.push(ExternalCommand::Exit),
            "-reindex" | "-rebuild" => {
                out.push(ExternalCommand::Reindex);
                any_search = true;
            }
            "-save-db" | "-save-db-now" => {
                out.push(ExternalCommand::SaveDb);
                any_search = true;
            }
            _ => {}
        }
    }
    if !any_search && !out.iter().any(|c| matches!(c, ExternalCommand::Exit | ExternalCommand::Toggle)) {
        out.push(ExternalCommand::Show);
    }
    out
}

/// 同じインスタンス名の accio が起動中ならコマンドラインを転送して true。
pub fn forward_to_running(instance: &Option<String>, args: &[String]) -> bool {
    let class_name = match instance {
        Some(i) if !i.is_empty() => format!("{WNDCLASS}_({i})"),
        _ => WNDCLASS.to_string(),
    };
    let class = wide(&class_name);
    let title = wide("accio");
    // SAFETY: NUL 終端文字列。見つからなければ null。
    let hwnd = unsafe { FindWindowW(class.as_ptr(), title.as_ptr()) };
    if hwnd.is_null() {
        return false;
    }
    let mut payload: Vec<u8> = Vec::new();
    for a in args {
        payload.extend_from_slice(a.as_bytes());
        payload.push(0);
    }
    let cds = COPYDATASTRUCT { dwData: COPYDATA_ACCIO_ARGS, cbData: payload.len() as u32, lpData: payload.as_ptr() as *mut _ };
    unsafe { SendMessageW(hwnd, WM_COPYDATA, 0, &cds as *const _ as LPARAM) == 1 }
}

const WNDCLASS: &str = "EVERYTHING_TASKBAR_NOTIFICATION";

const IPC_GET_MAJOR_VERSION: usize = 0;
const IPC_GET_MINOR_VERSION: usize = 1;
const IPC_GET_REVISION: usize = 2;
const IPC_GET_BUILD_NUMBER: usize = 3;
const IPC_EXIT: usize = 4;
const IPC_GET_TARGET_MACHINE: usize = 5;
const IPC_IS_NTFS_DRIVE_INDEXED: usize = 400;
const IPC_IS_DB_LOADED: usize = 401;
const IPC_IS_DB_BUSY: usize = 402;
const IPC_IS_ADMIN: usize = 403;
const IPC_IS_APPDATA: usize = 404;
const IPC_REBUILD_DB: usize = 405;
const IPC_SAVE_DB: usize = 407;
const IPC_IS_FAST_SORT: usize = 410;
const IPC_IS_FILE_INFO_INDEXED: usize = 411;

const COPYDATA_QUERYA: usize = 1;
const COPYDATA_QUERYW: usize = 2;
const COPYDATA_QUERY2A: usize = 17;
const COPYDATA_QUERY2W: usize = 18;

const MATCHCASE: u32 = 0x1;
const MATCHWHOLEWORD: u32 = 0x2;
const MATCHPATH: u32 = 0x4;
const REGEX: u32 = 0x8;
const MATCHACCENTS: u32 = 0x10;

const ITEM_FOLDER: u32 = 0x1;
const ITEM_DRIVE: u32 = 0x2;

const REQ_NAME: u32 = 0x1;
const REQ_PATH: u32 = 0x2;
const REQ_FULL_PATH: u32 = 0x4;
const REQ_EXTENSION: u32 = 0x8;
const REQ_SIZE: u32 = 0x10;
const REQ_DATE_CREATED: u32 = 0x20;
const REQ_DATE_MODIFIED: u32 = 0x40;
const REQ_DATE_ACCESSED: u32 = 0x80;
const REQ_ATTRIBUTES: u32 = 0x100;
const REQ_FILE_LIST_FILE_NAME: u32 = 0x200;
const REQ_RUN_COUNT: u32 = 0x400;
const REQ_DATE_RUN: u32 = 0x800;
const REQ_DATE_RECENTLY_CHANGED: u32 = 0x1000;
const REQ_HIGHLIGHTED_NAME: u32 = 0x2000;
const REQ_HIGHLIGHTED_PATH: u32 = 0x4000;
const REQ_HIGHLIGHTED_FULL_PATH: u32 = 0x8000;
/// このサーバーが実際に返せる要求フラグ
const SUPPORTED_REQ: u32 = REQ_NAME
    | REQ_PATH
    | REQ_FULL_PATH
    | REQ_EXTENSION
    | REQ_SIZE
    | REQ_DATE_CREATED
    | REQ_DATE_MODIFIED
    | REQ_DATE_ACCESSED
    | REQ_ATTRIBUTES
    | REQ_FILE_LIST_FILE_NAME
    | REQ_RUN_COUNT
    | REQ_DATE_RUN
    | REQ_DATE_RECENTLY_CHANGED
    | REQ_HIGHLIGHTED_NAME
    | REQ_HIGHLIGHTED_PATH
    | REQ_HIGHLIGHTED_FULL_PATH;

pub struct IpcContext {
    pub index: Arc<Index>,
    pub elevated: bool,
    pub db_loaded: Arc<AtomicBool>,
    pub on_exit: Box<dyn Fn() + Send + Sync>,
    /// UI が拾う外部コマンドのキュー
    pub external: Arc<parking_lot::Mutex<Vec<ExternalCommand>>>,
    pub repaint: Arc<dyn Fn() + Send + Sync>,
    pub reindex_requested: Arc<std::sync::atomic::AtomicU64>,
    pub save_requested: Arc<std::sync::atomic::AtomicU64>,
}

static CONTEXT: parking_lot::Mutex<Option<Arc<IpcContext>>> = parking_lot::Mutex::new(None);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn sort_from_ipc(t: u32) -> Sort {
    let (key, asc) = match t {
        1 | 2 => (SortKey::Name, t == 1),
        3 | 4 => (SortKey::Path, t == 3),
        5 | 6 => (SortKey::Size, t == 5),
        7 | 8 => (SortKey::Extension, t == 7),
        11 | 12 => (SortKey::DateCreated, t == 11),
        13 | 14 => (SortKey::DateModified, t == 13),
        19 | 20 => (SortKey::RunCount, t == 19),
        21 | 22 => (SortKey::DateModified, t == 21),
        23 | 24 => (SortKey::DateAccessed, t == 23),
        25 | 26 => (SortKey::DateRun, t == 25),
        _ => (SortKey::Name, true),
    };
    Sort { key, ascending: asc }
}

fn sort_to_ipc(s: Sort) -> u32 {
    let base = match s.key {
        SortKey::Name => 1,
        SortKey::Path => 3,
        SortKey::Size => 5,
        SortKey::Extension => 7,
        SortKey::DateCreated => 11,
        SortKey::DateModified => 13,
        SortKey::RunCount => 19,
        SortKey::DateAccessed => 23,
        SortKey::DateRun => 25,
    };
    if s.ascending {
        base
    } else {
        base + 1
    }
}

fn opts_from_flags(f: u32) -> SearchOpts {
    SearchOpts { match_case: f & MATCHCASE != 0, whole_word: f & MATCHWHOLEWORD != 0, match_path: f & MATCHPATH != 0, regex: f & REGEX != 0, match_diacritics: f & MATCHACCENTS != 0 }
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_wstr(out: &mut Vec<u8>, s: &str) {
    for u in s.encode_utf16() {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out.extend_from_slice(&0u16.to_le_bytes());
}

/// UTF-8 → システム ANSI コードページ（日本語環境なら CP932）。
fn to_ansi(s: &str) -> Vec<u8> {
    use windows_sys::Win32::Globalization::{WideCharToMultiByte, CP_ACP};
    let w: Vec<u16> = s.encode_utf16().collect();
    if w.is_empty() {
        return Vec::new();
    }
    // SAFETY: 入力スライスと長さを渡し、必要サイズを問い合わせてから書き込む。
    unsafe {
        let n = WideCharToMultiByte(CP_ACP, 0, w.as_ptr(), w.len() as i32, std::ptr::null_mut(), 0, std::ptr::null(), std::ptr::null_mut());
        if n <= 0 {
            return s.as_bytes().to_vec();
        }
        let mut out = vec![0u8; n as usize];
        WideCharToMultiByte(CP_ACP, 0, w.as_ptr(), w.len() as i32, out.as_mut_ptr(), n, std::ptr::null(), std::ptr::null_mut());
        out
    }
}

/// システム ANSI コードページ → UTF-8。
fn from_ansi(b: &[u8]) -> String {
    use windows_sys::Win32::Globalization::{MultiByteToWideChar, CP_ACP};
    if b.is_empty() {
        return String::new();
    }
    // SAFETY: 入力スライスと長さを渡し、必要サイズを問い合わせてから書き込む。
    unsafe {
        let n = MultiByteToWideChar(CP_ACP, 0, b.as_ptr(), b.len() as i32, std::ptr::null_mut(), 0);
        if n <= 0 {
            return String::from_utf8_lossy(b).into_owned();
        }
        let mut w = vec![0u16; n as usize];
        MultiByteToWideChar(CP_ACP, 0, b.as_ptr(), b.len() as i32, w.as_mut_ptr(), n);
        String::from_utf16_lossy(&w)
    }
}

fn put_astr(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&to_ansi(s));
    out.push(0);
}

/// v2 の文字列フィールド: DWORD 長（文字数）＋NUL 終端文字列
fn put_len_wstr(out: &mut Vec<u8>, s: &str) {
    let n = s.encode_utf16().count() as u32;
    put_u32(out, n);
    put_wstr(out, s);
}
fn put_len_astr(out: &mut Vec<u8>, s: &str) {
    let a = to_ansi(s);
    put_u32(out, a.len() as u32);
    out.extend_from_slice(&a);
    out.push(0);
}

/// `*text*` 形式のハイライト（Everything 互換: `*` 自体は `**`）
pub fn highlight_markup(name: &str, needles: &[String]) -> String {
    let lower = accio_index::volume::lowercase(name);
    if needles.is_empty() || lower.len() != name.len() {
        return name.replace('*', "**");
    }
    let mut marks = vec![false; name.len()];
    for n in needles {
        let mut start = 0;
        while let Some(p) = lower[start..].find(n.as_str()) {
            for m in &mut marks[start + p..start + p + n.len()] {
                *m = true;
            }
            start += p + n.len().max(1);
        }
    }
    let mut out = String::new();
    let mut on = false;
    for (i, ch) in name.char_indices() {
        if marks[i] != on {
            out.push('*');
            on = marks[i];
        }
        if ch == '*' {
            out.push_str("**");
        } else {
            out.push(ch);
        }
    }
    if on {
        out.push('*');
    }
    out
}

/// v1 応答 (EVERYTHING_IPC_LISTW/A)
fn build_list_v1(ctx: &IpcContext, query: &str, flags: u32, offset: u32, max: u32, unicode: bool) -> Vec<u8> {
    let vols = ctx.index.snapshot();
    let req = Request { query: query.to_string(), opts: opts_from_flags(flags), sort: Sort::default(), ..Default::default() };
    let res = search(&vols, &req, &|| false).unwrap_or_default();
    let start = (offset as usize).min(res.hits.len());
    let end = (start + max as usize).min(res.hits.len());
    let slice = &res.hits[start..end];
    let mut numfolders = 0u32;
    let mut numfiles = 0u32;
    let mut items: Vec<u8> = Vec::with_capacity(slice.len() * 12);
    let mut strings: Vec<u8> = Vec::new();
    let header = 7 * 4;
    let items_bytes = slice.len() * 12;
    for h in slice {
        let g = vols[h.vol as usize].read();
        let is_dir = g.is_dir(h.id);
        let mut f = 0;
        if is_dir {
            f |= ITEM_FOLDER;
            numfolders += 1;
        } else {
            numfiles += 1;
        }
        if h.id == 0 {
            f |= ITEM_DRIVE;
        }
        let name = g.name(h.id).to_string();
        let path = g.parent_path(h.id);
        drop(g);
        let name_off = (header + items_bytes + strings.len()) as u32;
        if unicode {
            put_wstr(&mut strings, &name);
        } else {
            put_astr(&mut strings, &name);
        }
        let path_off = (header + items_bytes + strings.len()) as u32;
        if unicode {
            put_wstr(&mut strings, &path);
        } else {
            put_astr(&mut strings, &path);
        }
        put_u32(&mut items, f);
        put_u32(&mut items, name_off);
        put_u32(&mut items, path_off);
    }
    let mut out = Vec::with_capacity(header + items_bytes + strings.len());
    put_u32(&mut out, res.folders as u32);
    put_u32(&mut out, res.files as u32);
    put_u32(&mut out, res.total as u32);
    put_u32(&mut out, numfolders);
    put_u32(&mut out, numfiles);
    put_u32(&mut out, slice.len() as u32);
    put_u32(&mut out, start as u32);
    out.extend_from_slice(&items);
    out.extend_from_slice(&strings);
    out
}

/// v2 応答 (EVERYTHING_IPC_LIST2 + ITEM2 + データ)
#[allow(clippy::too_many_arguments)]
fn build_list_v2(ctx: &IpcContext, query: &str, flags: u32, offset: u32, max: u32, request: u32, sort_type: u32, unicode: bool) -> Vec<u8> {
    let vols = ctx.index.snapshot();
    let sort = sort_from_ipc(sort_type);
    let req = Request { query: query.to_string(), opts: opts_from_flags(flags), sort, ..Default::default() };
    let res = search(&vols, &req, &|| false).unwrap_or_default();
    let needles = accio_query::highlight_needles(query);
    let request = request & SUPPORTED_REQ;
    let start = (offset as usize).min(res.hits.len());
    let end = (start + max as usize).min(res.hits.len());
    let slice = &res.hits[start..end];
    let header = 5 * 4;
    let items_bytes = slice.len() * 8;
    let mut items: Vec<u8> = Vec::with_capacity(items_bytes);
    let mut data: Vec<u8> = Vec::new();
    let put_s = |d: &mut Vec<u8>, s: &str| if unicode { put_len_wstr(d, s) } else { put_len_astr(d, s) };
    for h in slice {
        let g = vols[h.vol as usize].read();
        let is_dir = g.is_dir(h.id);
        let mut f = 0;
        if is_dir {
            f |= ITEM_FOLDER;
        }
        if h.id == 0 {
            f |= ITEM_DRIVE;
        }
        let name = g.name(h.id).to_string();
        let path = g.parent_path(h.id);
        let full = g.full_path(h.id);
        let ext = String::from_utf8_lossy(g.ext(h.id)).into_owned();
        let size = g.size[h.id as usize];
        let mtime = g.mtime(h.id);
        let ctime = g.ctime(h.id);
        let atime = g.atime(h.id);
        let attrs = g.attrs[h.id as usize];
        drop(g);
        put_u32(&mut items, f);
        put_u32(&mut items, (header + items_bytes + data.len()) as u32);
        if request & REQ_NAME != 0 {
            put_s(&mut data, &name);
        }
        if request & REQ_PATH != 0 {
            put_s(&mut data, &path);
        }
        if request & REQ_FULL_PATH != 0 {
            put_s(&mut data, &full);
        }
        if request & REQ_EXTENSION != 0 {
            put_s(&mut data, &ext);
        }
        if request & REQ_SIZE != 0 {
            data.extend_from_slice(&size.to_le_bytes());
        }
        if request & REQ_DATE_CREATED != 0 {
            data.extend_from_slice(&ctime.to_le_bytes());
        }
        if request & REQ_DATE_MODIFIED != 0 {
            data.extend_from_slice(&mtime.to_le_bytes());
        }
        if request & REQ_DATE_ACCESSED != 0 {
            data.extend_from_slice(&atime.to_le_bytes());
        }
        if request & REQ_ATTRIBUTES != 0 {
            put_u32(&mut data, attrs);
        }
        if request & REQ_FILE_LIST_FILE_NAME != 0 {
            put_s(&mut data, "");
        }
        if request & (REQ_RUN_COUNT | REQ_DATE_RUN) != 0 {
            let (rc, dr) = accio_query::run_history_get(&full).unwrap_or((0, 0));
            if request & REQ_RUN_COUNT != 0 {
                put_u32(&mut data, rc);
            }
            if request & REQ_DATE_RUN != 0 {
                data.extend_from_slice(&dr.to_le_bytes());
            }
        }
        if request & REQ_DATE_RECENTLY_CHANGED != 0 {
            data.extend_from_slice(&mtime.to_le_bytes());
        }
        if request & REQ_HIGHLIGHTED_NAME != 0 {
            put_s(&mut data, &highlight_markup(&name, &needles));
        }
        if request & REQ_HIGHLIGHTED_PATH != 0 {
            put_s(&mut data, &path.replace('*', "**"));
        }
        if request & REQ_HIGHLIGHTED_FULL_PATH != 0 {
            put_s(&mut data, &format!("{}\\{}", path.replace('*', "**"), highlight_markup(&name, &needles)));
        }
    }
    let mut out = Vec::with_capacity(header + items_bytes + data.len());
    put_u32(&mut out, res.total as u32);
    put_u32(&mut out, slice.len() as u32);
    put_u32(&mut out, start as u32);
    put_u32(&mut out, request);
    put_u32(&mut out, sort_to_ipc(sort));
    out.extend_from_slice(&items);
    out.extend_from_slice(&data);
    out
}

fn reply(reply_hwnd: u32, reply_msg: u32, from: HWND, payload: Vec<u8>) {
    let cds = COPYDATASTRUCT { dwData: reply_msg as usize, cbData: payload.len() as u32, lpData: payload.as_ptr() as *mut _ };
    // SAFETY: 送信が完了するまで payload は生きている（SendMessage は同期）。
    unsafe {
        SendMessageW(reply_hwnd as usize as HWND, WM_COPYDATA, from as usize, &cds as *const _ as LPARAM);
    }
}

fn read_query_string(bytes: &[u8], unicode: bool) -> String {
    if unicode {
        let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
        String::from_utf16_lossy(&units[..end])
    } else {
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        from_ansi(&bytes[..end])
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let ctx = CONTEXT.lock().clone();
    let Some(ctx) = ctx else { return DefWindowProcW(hwnd, msg, wparam, lparam) };
    match msg {
        WM_COPYDATA => {
            let cds = &*(lparam as *const COPYDATASTRUCT);
            let bytes = std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize);
            match cds.dwData {
                COPYDATA_ACCIO_ARGS => {
                    let args: Vec<String> = bytes.split(|&b| b == 0).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
                    let cmds = commands_from_args(&args);
                    for c in &cmds {
                        match c {
                            ExternalCommand::Search(_) | ExternalCommand::Show | ExternalCommand::NewTab(_) => crate::winshow::show(),
                            ExternalCommand::Toggle => crate::winshow::toggle(),
                            _ => {}
                        }
                    }
                    ctx.external.lock().extend(cmds);
                    (ctx.repaint)();
                    1
                }
                COPYDATA_QUERYA | COPYDATA_QUERYW if bytes.len() >= 20 => {
                    let unicode = cds.dwData == COPYDATA_QUERYW;
                    let reply_hwnd = u32_at(bytes, 0);
                    let reply_msg = u32_at(bytes, 4);
                    let flags = u32_at(bytes, 8);
                    let offset = u32_at(bytes, 12);
                    let max = u32_at(bytes, 16);
                    let q = read_query_string(&bytes[20..], unicode);
                    let Ok(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| build_list_v1(&ctx, &q, flags, offset, max, unicode))) else { return 0 };
                    reply(reply_hwnd, reply_msg, hwnd, payload);
                    1
                }
                COPYDATA_QUERY2A | COPYDATA_QUERY2W if bytes.len() >= 28 => {
                    let unicode = cds.dwData == COPYDATA_QUERY2W;
                    let reply_hwnd = u32_at(bytes, 0);
                    let reply_msg = u32_at(bytes, 4);
                    let flags = u32_at(bytes, 8);
                    let offset = u32_at(bytes, 12);
                    let max = u32_at(bytes, 16);
                    let request = u32_at(bytes, 20);
                    let sort_type = u32_at(bytes, 24);
                    let q = read_query_string(&bytes[28..], unicode);
                    let Ok(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| build_list_v2(&ctx, &q, flags, offset, max, request, sort_type, unicode))) else { return 0 };
                    reply(reply_hwnd, reply_msg, hwnd, payload);
                    1
                }
                _ => 0,
            }
        }
        WM_USER => match wparam {
            IPC_GET_MAJOR_VERSION => 1,
            IPC_GET_MINOR_VERSION => 4,
            IPC_GET_REVISION => 1,
            IPC_GET_BUILD_NUMBER => 1024,
            IPC_GET_TARGET_MACHINE => 2,
            IPC_IS_DB_LOADED => ctx.db_loaded.load(Ordering::Relaxed) as LRESULT,
            IPC_IS_DB_BUSY => 0,
            IPC_IS_ADMIN => ctx.elevated as LRESULT,
            IPC_IS_APPDATA => 1,
            IPC_IS_NTFS_DRIVE_INDEXED => {
                let letter = (b'A' + (lparam as u8 & 31)) as char;
                ctx.index.snapshot().iter().any(|v| v.read().meta.display_root.starts_with(letter)) as LRESULT
            }
            IPC_IS_FAST_SORT => matches!(lparam, 1 | 2 | 5 | 6 | 13 | 14) as LRESULT,
            IPC_IS_FILE_INFO_INDEXED => matches!(lparam, 1 | 2 | 3 | 4 | 6) as LRESULT,
            IPC_REBUILD_DB => {
                ctx.reindex_requested.fetch_add(1, Ordering::Relaxed);
                1
            }
            IPC_SAVE_DB => {
                ctx.save_requested.fetch_add(1, Ordering::Relaxed);
                1
            }
            IPC_EXIT => {
                (ctx.on_exit)();
                1
            }
            _ => 0,
        },
        WM_CLOSE => {
            (ctx.on_exit)();
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// IPC ウィンドウを専用スレッドで作成してメッセージループを回す。
pub fn start(instance: Option<String>, ctx: Arc<IpcContext>) {
    *CONTEXT.lock() = Some(ctx);
    let class_name = match instance {
        Some(i) if !i.is_empty() => format!("{WNDCLASS}_({i})"),
        _ => WNDCLASS.to_string(),
    };
    std::thread::Builder::new()
        .name("everything-ipc".into())
        .spawn(move || {
            let class = wide(&class_name);
            // SAFETY: 構造体はゼロ初期化からの明示設定。文字列は NUL 終端。
            unsafe {
                let mut wc: WNDCLASSW = std::mem::zeroed();
                wc.lpfnWndProc = Some(wndproc);
                wc.lpszClassName = class.as_ptr();
                if RegisterClassW(&wc) == 0 {
                    log::warn!("IPC ウィンドウクラスを登録できない（Everything が起動中？）");
                    return;
                }
                let hwnd = CreateWindowExW(
                    0,
                    class.as_ptr(),
                    wide("accio").as_ptr(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                );
                // 昇格して動いているとき、非昇格の es.exe / SDK クライアントからの WM_COPYDATA は UIPI で
                // 遮断される（error 5）。Everything と同じく明示的に許可する。
                if !hwnd.is_null() {
                    ChangeWindowMessageFilterEx(hwnd, WM_COPYDATA, MSGFLT_ALLOW, std::ptr::null_mut());
                }
                if hwnd.is_null() {
                    log::warn!("IPC ウィンドウを作成できない");
                    return;
                }
                log::info!("Everything 互換 IPC を開始 (class {class_name})");
                let mut msg: MSG = std::mem::zeroed();
                while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        })
        .ok();
}
