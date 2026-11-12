//! Everything 互換 ETP/FTP サーバー（既定は無効。accio.ini の etp_server=1 / etp_port / etp_bind / etp_user / etp_password / etp_allow_download）。
//!
//! Everything 1.4 クライアント（ツール → ETP サーバーに接続）が送る順序:
//! USER/PASS → OPTS UTF8 ON → EVERYTHING <option> … → EVERYTHING SEARCH <text> → EVERYTHING QUERY。
//! QUERY の結果は制御接続上の複数行 200 応答（`200-Query results` … `200 End.`）。
//! LIST/MLSD/SIZE/MDTM は索引から答える。RETR は etp_allow_download=1 のときだけ実ファイルを送る。
//! 応答文字列は voidtools/etp_server（1.5 用公式プラグイン）に合わせている。

use accio_index::Index;
use accio_query::{search, Request, SearchOpts, Sort, SortKey};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

pub struct EtpConfig {
    pub bind: String,
    pub port: u16,
    pub user: Option<String>,
    pub password: Option<String>,
    pub allow_download: bool,
}

#[derive(Default, Clone)]
struct Options {
    case: bool,
    whole_word: bool,
    path: bool,
    regex: bool,
    diacritics: bool,
    hide_empty: bool,
    search: String,
    filter_search: String,
    sort: Sort,
    offset: u32,
    count: u32,
    size_col: bool,
    attr_col: bool,
    dm_col: bool,
    dc_col: bool,
    path_col: bool,
    fl_col: bool,
}

struct Session {
    index: Arc<Index>,
    cfg: Arc<EtpConfig>,
    stream: TcpStream,
    /// 制御接続の読み取り側（QUERY 実行中の割り込み検出にも使う）
    reader: BufReader<TcpStream>,
    logged_in: bool,
    user: String,
    utf8: bool,
    cwd: String, // "/" or "/C:/dir"
    opts: Options,
    pasv: Option<TcpListener>,
}

fn parse_sort(name: &str) -> Option<Sort> {
    let n = name.trim().to_ascii_uppercase();
    let (key, asc) = if let Some(k) = n.strip_suffix("_ASCENDING") {
        (k.to_string(), true)
    } else if let Some(k) = n.strip_suffix("_DESCENDING") {
        (k.to_string(), false)
    } else {
        (n.clone(), true)
    };
    let key = match key.as_str() {
        "NAME" => SortKey::Name,
        "PATH" => SortKey::Path,
        "SIZE" | "INVERSE_SIZE" => SortKey::Size,
        "EXTENSION" => SortKey::Extension,
        "DATE_CREATED" => SortKey::DateCreated,
        "DATE_ACCESSED" => SortKey::DateAccessed,
        "RUN_COUNT" => SortKey::RunCount,
        "DATE_RUN" => SortKey::DateRun,
        "DATE_MODIFIED" | "DATE_RECENTLY_CHANGED" => SortKey::DateModified,
        "ATTRIBUTES" | "FILE_LIST_FILENAME" => SortKey::Name,
        _ => return None,
    };
    let asc = if key == SortKey::Size && n.starts_with("INVERSE_") { !asc } else { asc };
    Some(Sort { key, ascending: asc })
}

/// FTP パス (`/C:/dir/sub`) → 索引パス (`C:\dir\sub`)。ルートは None。
fn ftp_to_index(cwd: &str, arg: &str) -> Option<String> {
    let a = arg.trim();
    let joined = if a.starts_with('/') {
        a.to_string()
    } else if a.is_empty() || a == "." {
        cwd.to_string()
    } else if cwd == "/" {
        format!("/{a}")
    } else {
        format!("{cwd}/{a}")
    };
    // 正規化（.. と .）
    let mut parts: Vec<&str> = Vec::new();
    for seg in joined.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("\\"))
}

fn index_to_ftp(p: &str) -> String {
    format!("/{}", p.replace('\\', "/"))
}

fn filetime_to_ymdhms(ft: i64) -> String {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::FileTimeToSystemTime;
    let f = FILETIME { dwLowDateTime: ft as u32, dwHighDateTime: (ft >> 32) as u32 };
    // SAFETY: 出力専用構造体。
    let mut st: SYSTEMTIME = unsafe { std::mem::zeroed() };
    if unsafe { FileTimeToSystemTime(&f, &mut st) } == 0 {
        return "19700101000000".into();
    }
    format!("{:04}{:02}{:02}{:02}{:02}{:02}", st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond)
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

fn unix_list_line(is_dir: bool, size: u64, mtime_ft: i64, name: &str) -> String {
    let t = accio_query::datetime::filetime_to_local(mtime_ft);
    let now = accio_query::datetime::now_local();
    let (mon, day, tail) = match t {
        Some(t) => {
            let recent = (now.year as i32 * 12 + now.month as i32) - (t.year as i32 * 12 + t.month as i32) < 6;
            (MONTHS[(t.month.max(1) - 1) as usize % 12], t.day, if recent { format!("{:02}:{:02}", t.hour, t.minute) } else { format!("{:5}", t.year) })
        }
        None => ("Jan", 1, "1970".to_string()),
    };
    format!("{}r--r--r-- 1 ftp ftp {:13} {} {:02} {} {}\r\n", if is_dir { 'd' } else { '-' }, size, mon, day, tail, name)
}

impl Session {
    fn send(&mut self, line: &str) -> std::io::Result<()> {
        self.stream.write_all(line.as_bytes())?;
        self.stream.write_all(b"\r\n")
    }

    /// 索引パスをエントリに解決する。
    fn resolve(&self, path: &str) -> Option<(accio_index::SharedVolume, u32)> {
        let lower = accio_index::volume::lowercase(path.trim_end_matches('\\'));
        for v in self.index.snapshot() {
            let g = v.read();
            if let Some(id) = accio_query::eval::resolve_path(&g, &lower, &|| false) {
                drop(g);
                return Some((v, id));
            }
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

    /// ディレクトリ一覧 (is_dir, size, mtime, name)。None = ルート（ボリューム一覧）。
    fn list(&self, path: Option<&str>) -> Option<Vec<(bool, u64, i64, String)>> {
        match path {
            None => Some(self.index.snapshot().iter().map(|v| {
                let g = v.read();
                (true, g.size[0], g.mtime(0), g.name(0).to_string())
            }).collect()),
            Some(p) => {
                let (v, id) = self.resolve(p)?;
                let g = v.read();
                if !g.is_dir(id) {
                    return None;
                }
                let mut out = Vec::new();
                for i in 0..g.len() as u32 {
                    if g.parent[i as usize] == id && !g.is_deleted(i) {
                        out.push((g.is_dir(i), g.size[i as usize], g.mtime(i), g.name(i).to_string()));
                    }
                }
                Some(out)
            }
        }
    }

    fn open_data(&mut self) -> Option<TcpStream> {
        let l = self.pasv.take()?;
        l.set_nonblocking(false).ok();
        let (s, _) = l.accept().ok()?;
        Some(s)
    }

    /// QUERY: 検索はワーカースレッドで行い、その間に次のコマンドが届いたら `426 Query aborted.` を返して
    /// 検索を打ち切り、届いたコマンドを処理する（voidtools/etp_server と同じ）。
    fn query(&mut self) -> std::io::Result<bool> {
        let o = self.opts.clone();
        let index = self.index.clone();
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel2 = cancel.clone();
        let (tx, rx) = std::sync::mpsc::channel::<Option<String>>();
        std::thread::spawn(move || {
            if let Ok(ms) = std::env::var("ACCIO_ETP_SLOW_QUERY_MS") {
                // 検証用: 中断の再現
                std::thread::sleep(Duration::from_millis(ms.parse().unwrap_or(0)));
            }
            let _ = tx.send(build_query_reply(&index, &o, &|| cancel2.load(Ordering::Relaxed)));
        });
        let _ = self.reader.get_ref().set_read_timeout(Some(Duration::from_millis(50)));
        let mut pending_line: Option<String> = None;
        let outcome = loop {
            match rx.try_recv() {
                Ok(reply) => break reply,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
            let mut line = Vec::new();
            match self.reader.read_until(b'\n', &mut line) {
                Ok(0) => {
                    cancel.store(true, Ordering::Relaxed);
                    let _ = self.reader.get_ref().set_read_timeout(Some(Duration::from_secs(3600)));
                    return Ok(false);
                }
                Ok(_) => {
                    while line.last().map(|&b| b == b'\n' || b == b'\r').unwrap_or(false) {
                        line.pop();
                    }
                    cancel.store(true, Ordering::Relaxed);
                    pending_line = Some(String::from_utf8_lossy(&line).into_owned());
                    break None;
                }
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut || e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => {
                    cancel.store(true, Ordering::Relaxed);
                    let _ = self.reader.get_ref().set_read_timeout(Some(Duration::from_secs(3600)));
                    return Ok(false);
                }
            }
        };
        let _ = self.reader.get_ref().set_read_timeout(Some(Duration::from_secs(3600)));
        if let Some(line) = pending_line {
            self.send("426 Query aborted.")?;
            return self.handle(&line);
        }
        match outcome {
            Some(reply) => {
                self.stream.write_all(reply.as_bytes())?;
                Ok(true)
            }
            None => self.send("426 Query aborted.").map(|_| true),
        }
    }
}

/// QUERY の応答本文（制御接続上の複数行 200 応答）。
fn build_query_reply(index: &Index, o: &Options, cancel: &(dyn Fn() -> bool + Sync)) -> Option<String> {
    {
        let mut lines: Vec<String> = vec!["200-Query results".into()];
        if o.hide_empty && o.search.is_empty() {
            lines.push(" RESULT_COUNT 0".into());
        } else {
            let mut q = o.search.clone();
            if !o.filter_search.is_empty() {
                q = format!("{} <{}>", q, o.filter_search);
            }
            let vols = index.snapshot();
            let req = Request { query: q, opts: SearchOpts { match_case: o.case, whole_word: o.whole_word, match_path: o.path, regex: o.regex, match_diacritics: o.diacritics }, sort: o.sort, ..Default::default() };
            let res = search(&vols, &req, cancel)?;
            lines.push(format!(" RESULT_COUNT {}", res.total));
            let start = (o.offset as usize).min(res.hits.len());
            let end = start.saturating_add(o.count as usize).min(res.hits.len());
            for h in &res.hits[start..end] {
                let Some(v) = vols.get(h.vol as usize) else { continue };
                let g = v.read();
                if o.path_col {
                    lines.push(format!(" PATH {}", g.parent_path(h.id)));
                }
                if o.attr_col {
                    lines.push(format!(" ATTRIBUTES {}", g.attrs[h.id as usize]));
                }
                if o.size_col {
                    let s = if g.is_dir(h.id) && g.size[h.id as usize] == 0 { u64::MAX } else { g.size[h.id as usize] };
                    lines.push(format!(" SIZE {s}"));
                }
                if o.dm_col {
                    lines.push(format!(" DATE_MODIFIED {}", g.mtime(h.id) as u64));
                }
                if o.dc_col {
                    lines.push(format!(" DATE_CREATED {}", g.ctime(h.id) as u64));
                }
                if o.fl_col {
                    lines.push(" FILE_LIST_FILENAME ".into());
                }
                lines.push(format!(" {} {}", if g.is_dir(h.id) { "FOLDER" } else { "FILE" }, g.name(h.id)));
            }
        }
        lines.push("200 End.".into());
        Some(lines.join("\r\n") + "\r\n")
    }
}

impl Session {
    fn everything(&mut self, rest: &str) -> std::io::Result<bool> {
        let (sub, param) = match rest.find(|c: char| c.is_ascii_whitespace()) {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => (rest, ""),
        };
        let sub = sub.to_ascii_uppercase();
        let flag = param.trim().parse::<i64>().map(|v| v != 0).unwrap_or(false);
        let n = flag as u8;
        let msg = match sub.as_str() {
            "" => return self.send("500 SITE: Missing argument.").map(|_| true),
            "CASE" => { self.opts.case = flag; format!("200 Case set to ({n}).") }
            "WHOLE_WORD" => { self.opts.whole_word = flag; format!("200 Whole word set to ({n}).") }
            "PATH" => { self.opts.path = flag; format!("200 Path set to ({n}).") }
            "DIACRITICS" => { self.opts.diacritics = flag; format!("200 Diacritics set to ({n}).") }
            "PREFIX" => format!("200 Prefix set to ({n})."),
            "SUFFIX" => format!("200 Suffix set to ({n})."),
            "IGNORE_PUNCTUATION" => format!("200 Ignore punctuation set to ({n})."),
            "IGNORE_WHITESPACE" => format!("200 Ignore whitespace set to ({n})."),
            "REGEX" => { self.opts.regex = flag; format!("200 Regex set to ({n}).") }
            "HIDE_EMPTY_SEARCH_RESULTS" => { self.opts.hide_empty = flag; format!("200 Hide empty search results set to ({n}).") }
            "SEARCH" => { self.opts.search = param.to_string(); format!("200 Search set to ({param}).") }
            "FILTER_SEARCH" => { self.opts.filter_search = param.to_string(); format!("200 Filter search set to ({param}).") }
            "FILTER_CASE" => format!("200 Filter case set to ({n})."),
            "FILTER_WHOLE_WORD" => format!("200 Filter whole word set to ({n})."),
            "FILTER_PATH" => format!("200 Filter path set to ({n})."),
            "FILTER_DIACRITICS" => format!("200 Filter diacritics set to ({n})."),
            "FILTER_PREFIX" => format!("200 Filter prefix set to ({n})."),
            "FILTER_SUFFIX" | "FILTER_IGNORE_PUNCTUATION" => format!("200 Filter ignore punctuation set to ({n})."),
            "FILTER_IGNORE_WHITESPACE" => format!("200 Filter ignore whitespace set to ({n})."),
            "FILTER_REGEX" => format!("200 Filter regex set to ({n})."),
            "SORT" => match parse_sort(param) {
                Some(s) => { self.opts.sort = s; format!("200 Sort set to ({}).", param.trim()) }
                None => "500 Unknown sort type.".into(),
            },
            "OFFSET" => { self.opts.offset = param.trim().parse().unwrap_or(0); format!("200 Offset set to ({}).", self.opts.offset) }
            "COUNT" => { self.opts.count = param.trim().parse().unwrap_or(u32::MAX); format!("200 Count set to ({}).", self.opts.count) }
            "SIZE_COLUMN" => { self.opts.size_col = flag; format!("200 Size column set to ({n}).") }
            "ATTRIBUTES_COLUMN" => { self.opts.attr_col = flag; format!("200 Attributes column set to ({n}).") }
            "DATE_MODIFIED_COLUMN" => { self.opts.dm_col = flag; format!("200 Date modified column set to ({n}).") }
            "DATE_CREATED_COLUMN" => { self.opts.dc_col = flag; format!("200 Date created column set to ({n}).") }
            "PATH_COLUMN" => { self.opts.path_col = flag; format!("200 Path column set to ({n}).") }
            "FILE_LIST_FILENAME_COLUMN" => { self.opts.fl_col = flag; format!("200 File list filename column set to ({n}).") }
            "DATE_RECENTLY_CHANGED_COLUMN" => format!("200 Date recently changed column set to ({n})."),
            "QUERY" => return self.query(),
            _ => "500 Unknown Everything command.".into(),
        };
        self.send(&msg).map(|_| true)
    }

    fn handle(&mut self, line: &str) -> std::io::Result<bool> {
        let (cmd, arg) = match line.find(|c: char| c.is_ascii_whitespace()) {
            Some(i) => (&line[..i], &line[i + 1..]),
            None => (line, ""),
        };
        let cmd = cmd.to_ascii_uppercase();
        if !self.logged_in {
            match cmd.as_str() {
                "USER" => {
                    self.user = arg.trim().to_string();
                    let user_ok = self.cfg.user.as_deref().map(|u| u.is_empty() || u == self.user).unwrap_or(true);
                    if !user_ok || self.cfg.password.as_deref().map(|p| !p.is_empty()).unwrap_or(false) {
                        self.send(&format!("331 Password required for {}", self.user))?;
                    } else {
                        self.logged_in = true;
                        self.send("230 Logged on.")?;
                    }
                }
                "PASS" => {
                    let user_ok = self.cfg.user.as_deref().map(|u| u.is_empty() || u == self.user).unwrap_or(true);
                    let pass_ok = self.cfg.password.as_deref().map(|p| p.is_empty() || p == arg.trim()).unwrap_or(true);
                    if user_ok && pass_ok {
                        self.logged_in = true;
                        self.send("230 Logged on.")?;
                    } else {
                        self.send("530 Login or password incorrect!")?;
                    }
                }
                "QUIT" => {
                    self.send("221 Goodbye.")?;
                    return Ok(false);
                }
                _ => self.send("530 Not logged on.")?,
            }
            return Ok(true);
        }
        match cmd.as_str() {
            "PASS" => self.send("503 Invalid sequence of commands.")?,
            "USER" => self.send("230 Logged on.")?,
            "FEAT" => self.send("211-Features:\r\n MDTM\r\n REST STREAM\r\n SIZE\r\n MLST type*;size*;modify*;\r\n MLSD\r\n UTF8\r\n EVERYTHING\r\n211 End")?,
            "HELP" => self.send("214-Site commands:\r\n EVERYTHING\r\n214 End")?,
            "SYST" => self.send("215 UNIX emulated by Everything")?,
            "OPTS" => {
                let a = arg.trim().to_ascii_uppercase();
                if a == "UTF8 ON" {
                    self.utf8 = true;
                    self.send("200 UTF8 mode enabled.")?;
                } else if a == "UTF8 OFF" {
                    self.utf8 = false;
                    self.send("200 UTF8 mode disabled.")?;
                } else {
                    self.send("501 Invalid option.")?;
                }
            }
            "TYPE" => match arg.trim().to_ascii_uppercase().as_str() {
                "A" => self.send("200 Type set to ASCII (A).")?,
                "I" => self.send("200 Type set to Image (I).")?,
                "E" => self.send("504 Type E not implemented.")?,
                t => self.send(&format!("500 Type {t} not supported."))?,
            },
            "MODE" | "STRU" | "NOOP" => self.send("200 OK.")?,
            "PWD" | "XPWD" => self.send(&format!("257 \"{}\" is current directory.", self.cwd))?,
            "CWD" | "XCWD" => {
                let target = ftp_to_index(&self.cwd, arg);
                let ok = match &target {
                    None => true,
                    Some(p) => self.resolve(p).map(|(v, id)| v.read().is_dir(id)).unwrap_or(false),
                };
                if ok {
                    self.cwd = target.map(|p| index_to_ftp(&p)).unwrap_or_else(|| "/".into());
                    self.send(&format!("250 CWD successful. \"{}\" is current directory.", self.cwd))?;
                } else {
                    self.send("550 Directory not found.")?;
                }
            }
            "CDUP" => {
                let target = ftp_to_index(&self.cwd, "..");
                self.cwd = target.map(|p| index_to_ftp(&p)).unwrap_or_else(|| "/".into());
                self.send(&format!("250 CDUP successful. \"{}\" is current directory.", self.cwd))?;
            }
            "REST" => self.send(&format!("350 Restarting at {}.", arg.trim()))?,
            "PASV" | "EPSV" => {
                let local = self.stream.local_addr()?;
                let l = TcpListener::bind(SocketAddr::new(local.ip(), 0))?;
                let port = l.local_addr()?.port();
                self.pasv = Some(l);
                if cmd == "PASV" {
                    let ip = match local.ip() {
                        std::net::IpAddr::V4(v4) => v4,
                        _ => Ipv4Addr::LOCALHOST,
                    };
                    let o = ip.octets();
                    self.send(&format!("227 Entering Passive Mode. ({},{},{},{},{},{})", o[0], o[1], o[2], o[3], port >> 8, port & 0xFF))?;
                } else {
                    self.send(&format!("229 Entering Extended Passive Mode (|||{port}|)"))?;
                }
            }
            "PORT" | "EPRT" => self.send("501 Invalid option.")?,
            "LIST" | "NLST" | "MLSD" => {
                // `LIST -la path` のオプションを捨てる
                let a: String = arg.split_whitespace().filter(|s| !s.starts_with('-')).collect::<Vec<_>>().join(" ");
                let target = ftp_to_index(&self.cwd, &a);
                let Some(entries) = self.list(target.as_deref()) else {
                    self.send("550 Directory not found.")?;
                    return Ok(true);
                };
                let Some(mut data) = self.open_data() else {
                    self.send("425 Can't open data connection.")?;
                    return Ok(true);
                };
                self.send("150 Data connection accepted; transfer starting.")?;
                let mut out = String::new();
                for (is_dir, size, mtime, name) in entries {
                    match cmd.as_str() {
                        "NLST" => out.push_str(&format!("{name}\r\n")),
                        "MLSD" => out.push_str(&format!("type={};modify={};size={}; {name}\r\n", if is_dir { "dir" } else { "file" }, filetime_to_ymdhms(mtime), size)),
                        _ => out.push_str(&unix_list_line(is_dir, size, mtime, &name)),
                    }
                }
                let _ = data.write_all(out.as_bytes());
                drop(data);
                self.send("226 Transfer complete.")?;
            }
            "MLST" => {
                let target = ftp_to_index(&self.cwd, arg);
                match target.as_deref().and_then(|p| self.resolve(p)) {
                    Some((v, id)) => {
                        let g = v.read();
                        let p = index_to_ftp(&g.full_path(id));
                        self.send(&format!("250-Listing {p}\r\n type={};modify={};size={}; {p}\r\n250 End.", if g.is_dir(id) { "dir" } else { "file" }, filetime_to_ymdhms(g.mtime(id)), g.size[id as usize]))?;
                    }
                    None => self.send("550 File or Directory not found.")?,
                }
            }
            "SIZE" => {
                let target = ftp_to_index(&self.cwd, arg);
                match target.as_deref().and_then(|p| self.resolve(p)) {
                    Some((v, id)) if !v.read().is_dir(id) => {
                        let s = v.read().size[id as usize];
                        self.send(&format!("200 {s}"))?;
                    }
                    _ => self.send("550 File not found.")?,
                }
            }
            "MDTM" => {
                let target = ftp_to_index(&self.cwd, arg);
                match target.as_deref().and_then(|p| self.resolve(p)) {
                    Some((v, id)) => {
                        let t = v.read().mtime(id);
                        self.send(&format!("213 {}.000", filetime_to_ymdhms(t)))?;
                    }
                    None => self.send("550 File not found.")?,
                }
            }
            "RETR" => {
                if !self.cfg.allow_download {
                    self.send("550 File not found.")?;
                    return Ok(true);
                }
                let target = ftp_to_index(&self.cwd, arg);
                let Some(p) = target else {
                    self.send("550 File not found.")?;
                    return Ok(true);
                };
                let Ok(mut f) = std::fs::File::open(&p) else {
                    self.send("550 File not found.")?;
                    return Ok(true);
                };
                let Some(mut data) = self.open_data() else {
                    self.send("425 Can't open data connection.")?;
                    return Ok(true);
                };
                self.send("150 Data connection accepted; transfer starting.")?;
                let mut buf = vec![0u8; 1 << 20];
                let mut ok = true;
                loop {
                    let n = match f.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(_) => {
                            ok = false;
                            break;
                        }
                    };
                    if data.write_all(&buf[..n]).is_err() {
                        ok = false;
                        break;
                    }
                }
                drop(data);
                self.send(if ok { "226 Transfer complete." } else { "451 Transfer aborted: read error." })?;
            }
            "SITE" => {
                let a = arg.trim_start();
                let (sub, rest) = match a.find(|c: char| c.is_ascii_whitespace()) {
                    Some(i) => (&a[..i], &a[i + 1..]),
                    None => (a, ""),
                };
                if sub.eq_ignore_ascii_case("EVERYTHING") {
                    return self.everything(rest);
                } else if sub.is_empty() {
                    self.send("500 SITE: Missing argument.")?;
                } else {
                    self.send("500 Unknown SITE command.")?;
                }
            }
            "EVERYTHING" => return self.everything(arg),
            "QUIT" => {
                self.send("221 Goodbye.")?;
                return Ok(false);
            }
            _ => self.send("500 Syntax error, command unrecognized.")?,
        }
        Ok(true)
    }
}

fn serve(index: Arc<Index>, cfg: Arc<EtpConfig>, stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3600)));
    let reader = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut s = Session {
        index,
        cfg,
        stream,
        reader: BufReader::new(reader),
        logged_in: false,
        user: String::new(),
        utf8: true,
        cwd: "/".into(),
        opts: Options { count: u32::MAX, ..Default::default() },
        pasv: None,
    };
    if s.send("220 Welcome to accio ETP/FTP (Everything compatible)").is_err() {
        return;
    }
    let mut line = Vec::new();
    loop {
        line.clear();
        match s.reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        while line.last().map(|&b| b == b'\n' || b == b'\r').unwrap_or(false) {
            line.pop();
        }
        let text = String::from_utf8_lossy(&line).into_owned();
        match s.handle(&text) {
            Ok(true) => {}
            _ => return,
        }
    }
}

pub fn start(index: Arc<Index>, cfg: EtpConfig) {
    let addr = format!("{}:{}", cfg.bind, cfg.port);
    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            log::warn!("ETP サーバーを {addr} で開始できない: {e}");
            return;
        }
    };
    log::info!("ETP サーバー開始 ftp://{addr}/");
    let cfg = Arc::new(cfg);
    std::thread::Builder::new()
        .name("etp".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let index = index.clone();
                let cfg = cfg.clone();
                std::thread::spawn(move || serve(index, cfg, stream));
            }
        })
        .ok();
    let _ = Ordering::Relaxed;
}
