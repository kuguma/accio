//! Everything 互換の検索構文パーサ。
//!
//! 文法（Everything 既定: OR が AND より強く結合する）:
//!   expr    := andlist
//!   andlist := orlist (SPACE orlist)*
//!   orlist  := unary ('|' unary)*
//!   unary   := '!' unary | '<' expr '>' | term
//!   term    := (modifier ':')* [function ':'] text | '"' ... '"'

use crate::datetime::parse_date_range;

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    And(Vec<Node>),
    Or(Vec<Node>),
    Not(Box<Node>),
    Term(Term),
    /// 常に真（空文字列など）
    All,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mods {
    pub case: Option<bool>,
    pub path: Option<bool>,
    pub wfn: Option<bool>,
    pub wholeword: Option<bool>,
    pub regex: Option<bool>,
    /// ダイアクリティカルマークを区別する（`diacritics:` / `nodiacritics:`）
    pub diacritics: Option<bool>,
    /// `*` `?` をワイルドカードとして扱う（`nowildcards:` で文字そのもの）
    pub wildcards: Option<bool>,
    pub file_only: bool,
    pub folder_only: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cmp {
    Lt,
    Le,
    Eq,
    Ge,
    Gt,
    Ne,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// 部分一致（`*` `?` を含むならファイル名全体へのワイルドカード一致）
    Text(String),
    Regex(String),
    Ext(Vec<String>),
    /// [lo, hi] 両端含む
    Size(u64, u64),
    DateModified(i64, i64),
    DateCreated(i64, i64),
    DateAccessed(i64, i64),
    Folder,
    File,
    Empty,
    Root,
    /// 指定フォルダ直下
    Parent(String),
    /// 指定フォルダ配下すべて
    Ancestor(String),
    Child(String),
    Attrib(u32),
    Len(u32, u32),
    Depth(u32, u32),
    StartWith(String),
    EndWith(String),
    Count(u32),
    /// 重複（全ボリューム横断）。どの属性で重複とみなすか
    Dupe(DupeKey),
    /// 直下の子の数 (lo, hi, 種別)
    ChildCount(u32, u32, ChildKind),
    /// ファイル内容の部分一致（遅い: 候補のファイルを読む）
    Content(String),
    /// シェルの種別名（"テキスト ドキュメント" など）の部分一致
    Type(String),
    /// ファイル参照番号（MFT レコード番号 | 連番<<48）の範囲
    Frn(u64, u64),
    /// ボリューム番号（索引内の並び順、0 始まり）の範囲
    Fsi(u32, u32),
    /// 実行回数（アプリ層の実行履歴）の範囲
    RunCount(u32, u32),
    /// 最終実行日時（FILETIME 閉区間）
    DateRun(i64, i64),
    /// 未対応の関数（評価時は常に偽、UI に警告）
    Unsupported(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DupeKey {
    Name,
    NamePart,
    Size,
    DateModified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildKind {
    All,
    Files,
    Folders,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Term {
    pub kind: Kind,
    pub mods: Mods,
}

const MODIFIERS: &[&str] = &[
    "case", "nocase", "path", "nopath", "wfn", "wholefilename", "nowfn", "nowholefilename", "wholeword", "ww",
    "nowholeword", "noww", "regex", "noregex", "file", "files", "folder", "folders", "nofileonly", "nofolderonly",
    "ascii", "utf8", "noascii", "diacritics", "nodiacritics", "wildcards", "nowildcards",
];

/// Everything 既定のフィルタマクロ（拡張子リスト）。
pub const MACROS: &[(&str, &str)] = &[
    ("audio", "aac;ac3;aif;aifc;aiff;au;cda;dts;fla;flac;it;m1a;m2a;m3u;m4a;mid;midi;mka;mod;mp2;mp3;mpa;ogg;opus;ra;rmi;spc;rmi;snd;umx;voc;wav;wma;xm"),
    ("zip", "7z;ace;arj;bz2;cab;gz;gzip;jar;r00;r01;r02;r03;r04;r05;r06;r07;r08;r09;r10;r11;r12;r13;r14;r15;r16;r17;r18;r19;r20;r21;r22;r23;r24;r25;r26;r27;r28;r29;rar;tar;tgz;z;zip;zst;xz;lz;lzma"),
    ("doc", "c;chm;cpp;csv;cxx;doc;docm;docx;dot;dotm;dotx;h;hpp;htm;html;hxx;ini;java;lua;mht;mhtml;odt;pdf;potx;potm;ppam;ppsm;ppsx;pps;ppt;pptm;pptx;rtf;sldm;sldx;thmx;txt;vsd;wpd;wps;wri;xlam;xls;xlsb;xlsm;xlsx;xltm;xltx;xml;md"),
    ("exe", "bat;cmd;exe;msi;msp;scr;com;ps1"),
    ("pic", "ani;bmp;gif;ico;jpe;jpeg;jpg;pcx;png;psd;tga;tif;tiff;webp;wmf;heic;avif;svg"),
    ("image", "ani;bmp;gif;ico;jpe;jpeg;jpg;pcx;png;psd;tga;tif;tiff;webp;wmf;heic;avif;svg"),
    ("video", "3g2;3gp;3gp2;3gpp;amr;amv;asf;avi;bdmv;bik;d2v;divx;drc;dsa;dsm;dss;dsv;evo;f4v;flc;fli;flic;flv;hdmov;ifo;ivf;m1v;m2p;m2t;m2ts;m2v;m4b;m4p;m4v;mkv;mp2v;mp4;mp4v;mpe;mpeg;mpg;mpls;mpv2;mpv4;mov;mts;ogm;ogv;pss;pva;qt;ram;ratdvd;rm;rmm;rmvb;roq;rpm;smil;smk;swf;tp;tpr;ts;vob;vp6;webm;wm;wmp;wmv"),
];

/// ユーザー定義フィルタのマクロ（`名前:` → 検索式）。アプリ層が設定から登録する。
static CUSTOM_MACROS: std::sync::RwLock<Vec<(String, String)>> = std::sync::RwLock::new(Vec::new());

pub fn set_custom_macros(list: Vec<(String, String)>) {
    *CUSTOM_MACROS.write().unwrap_or_else(|e| e.into_inner()) = list.into_iter().map(|(m, q)| (m.to_ascii_lowercase(), q)).collect();
}

fn custom_macro(head: &str) -> Option<String> {
    CUSTOM_MACROS.read().unwrap_or_else(|e| e.into_inner()).iter().find(|(m, _)| m == head).map(|(_, q)| q.clone())
}

thread_local! {
    /// マクロ展開の再帰深さ（マクロが自分自身を参照しても止まるように）
    static MACRO_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

const FUNCTIONS: &[&str] = &[
    "ext", "size", "dm", "datemodified", "dc", "datecreated", "da", "dateaccessed", "parent", "infolder", "nosubfolders",
    "ancestor", "child", "empty", "root", "attrib", "attributes", "len", "depth", "parents", "startwith", "endwith",
    "count", "type", "content", "dupe", "sizedupe", "namepartdupe", "dmdupe", "frn", "fsi", "childcount", "childfilecount",
    "childfoldercount", "rc", "recentchange", "runcount", "dr", "daterun", "filelist",
];

struct Lexer<'a> {
    s: &'a [u8],
    p: usize,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Text(String),
    Or,
    Not,
    Open,
    Close,
}

impl<'a> Lexer<'a> {
    fn tokens(mut self) -> Vec<Tok> {
        let mut out = Vec::new();
        while self.p < self.s.len() {
            let c = self.s[self.p];
            match c {
                b' ' | b'\t' => {
                    self.p += 1;
                }
                b'|' => {
                    self.p += 1;
                    out.push(Tok::Or);
                }
                b'!' => {
                    self.p += 1;
                    out.push(Tok::Not);
                }
                b'<' => {
                    self.p += 1;
                    out.push(Tok::Open);
                }
                b'>' => {
                    self.p += 1;
                    out.push(Tok::Close);
                }
                _ => {
                    let mut buf = Vec::new();
                    let mut in_q = false;
                    while self.p < self.s.len() {
                        let c = self.s[self.p];
                        if in_q {
                            if c == b'"' {
                                in_q = false;
                            } else {
                                buf.push(c);
                            }
                            self.p += 1;
                            continue;
                        }
                        match c {
                            b'"' => {
                                in_q = true;
                                self.p += 1;
                            }
                            b' ' | b'\t' | b'|' => break,
                            // `size:>1mb` のように関数引数の比較演算子として現れる `<` `>` は本文の一部
                            b'<' | b'>' => {
                                if matches!(buf.last(), Some(b':' | b'<' | b'>' | b'=' | b'!')) {
                                    buf.push(c);
                                    self.p += 1;
                                } else {
                                    break;
                                }
                            }
                            _ => {
                                buf.push(c);
                                self.p += 1;
                            }
                        }
                    }
                    out.push(Tok::Text(String::from_utf8_lossy(&buf).into_owned()));
                }
            }
        }
        out
    }
}

struct Parser {
    toks: Vec<Tok>,
    p: usize,
    pub warnings: Vec<String>,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.p)
    }
    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.p).cloned();
        self.p += 1;
        t
    }

    fn expr(&mut self) -> Node {
        let mut items = Vec::new();
        while let Some(t) = self.peek() {
            if *t == Tok::Close {
                break;
            }
            items.push(self.orlist());
        }
        match items.len() {
            0 => Node::All,
            1 => items.pop().unwrap(),
            _ => Node::And(items),
        }
    }

    fn orlist(&mut self) -> Node {
        let mut items = vec![self.unary()];
        while self.peek() == Some(&Tok::Or) {
            self.p += 1;
            if matches!(self.peek(), None | Some(Tok::Close)) {
                break;
            }
            items.push(self.unary());
        }
        if items.len() == 1 {
            items.pop().unwrap()
        } else {
            Node::Or(items)
        }
    }

    fn unary(&mut self) -> Node {
        match self.next() {
            Some(Tok::Not) => {
                if matches!(self.peek(), None | Some(Tok::Close) | Some(Tok::Or)) {
                    return Node::All;
                }
                Node::Not(Box::new(self.unary()))
            }
            Some(Tok::Open) => {
                let e = self.expr();
                if self.peek() == Some(&Tok::Close) {
                    self.p += 1;
                }
                e
            }
            Some(Tok::Close) | Some(Tok::Or) => Node::All,
            Some(Tok::Text(t)) => self.term(&t),
            None => Node::All,
        }
    }

    fn term(&mut self, text: &str) -> Node {
        let mut mods = Mods::default();
        let mut rest = text;
        while let Some(colon) = rest.find(':') {
            let head = rest[..colon].to_ascii_lowercase();
            if head.is_empty() {
                break;
            }
            if MODIFIERS.contains(&head.as_str()) {
                apply_modifier(&mut mods, &head);
                rest = &rest[colon + 1..];
                continue;
            }
            if FUNCTIONS.contains(&head.as_str()) {
                let arg = &rest[colon + 1..];
                let kind = self.function(&head, arg);
                return Node::Term(Term { kind, mods });
            }
            if let Some((_, exts)) = MACROS.iter().find(|(m, _)| *m == head) {
                // `audio:` はフィルタマクロ。`audio:foo` は「音声ファイルかつ foo」
                let ext = Node::Term(Term { kind: Kind::Ext(exts.split(';').map(|e| e.to_string()).collect()), mods: mods.clone() });
                let arg = &rest[colon + 1..];
                if arg.is_empty() {
                    return ext;
                }
                let text = Node::Term(Term { kind: Kind::Text(arg.to_string()), mods });
                return Node::And(vec![ext, text]);
            }
            if let Some(q) = custom_macro(&head) {
                // ユーザー定義マクロ: `name:` は検索式そのもの、`name:foo` は「検索式 かつ foo」
                let depth = MACRO_DEPTH.with(|d| d.get());
                if depth >= 8 {
                    self.warnings.push(format!("マクロの展開が深すぎる: {head}:"));
                    return Node::All;
                }
                MACRO_DEPTH.with(|d| d.set(depth + 1));
                let inner = parse(&q);
                MACRO_DEPTH.with(|d| d.set(depth));
                self.warnings.extend(inner.warnings);
                let arg = &rest[colon + 1..];
                if arg.is_empty() {
                    return inner.root;
                }
                let text = Node::Term(Term { kind: Kind::Text(arg.to_string()), mods });
                return Node::And(vec![inner.root, text]);
            }
            break;
        }
        if rest.is_empty() {
            // `folder:` `file:` 単独は種別フィルタとして機能する
            if mods.folder_only {
                return Node::Term(Term { kind: Kind::Folder, mods });
            }
            if mods.file_only {
                return Node::Term(Term { kind: Kind::File, mods });
            }
            return Node::All;
        }
        if mods.regex == Some(true) {
            return Node::Term(Term { kind: Kind::Regex(rest.to_string()), mods });
        }
        Node::Term(Term { kind: Kind::Text(rest.to_string()), mods })
    }

    fn function(&mut self, name: &str, arg: &str) -> Kind {
        match name {
            "ext" => Kind::Ext(arg.split(';').filter(|s| !s.is_empty()).map(|s| s.trim_start_matches('.').to_ascii_lowercase()).collect()),
            "size" => match parse_size_range(arg) {
                Some((lo, hi)) => Kind::Size(lo, hi),
                None => {
                    self.warnings.push(format!("size: の値を解釈できない: {arg}"));
                    Kind::Unsupported(format!("size:{arg}"))
                }
            },
            // rc:/recentchange: は Everything が変更を観測した日時。accio は索引に観測時刻を持たないので更新日時で代用する
            "dm" | "datemodified" | "dc" | "datecreated" | "da" | "dateaccessed" | "rc" | "recentchange" | "dr" | "daterun" => match parse_date_expr(arg) {
                Some((lo, hi)) => match name {
                    "dc" | "datecreated" => Kind::DateCreated(lo, hi),
                    "da" | "dateaccessed" => Kind::DateAccessed(lo, hi),
                    "dr" | "daterun" => Kind::DateRun(lo, hi),
                    _ => Kind::DateModified(lo, hi),
                },
                None => {
                    self.warnings.push(format!("{name}: の日付を解釈できない: {arg}"));
                    Kind::Unsupported(format!("{name}:{arg}"))
                }
            },
            "parent" | "infolder" | "nosubfolders" => Kind::Parent(normalize_path(arg)),
            "ancestor" => Kind::Ancestor(normalize_path(arg)),
            "child" => Kind::Child(arg.to_string()),
            "empty" => Kind::Empty,
            "root" => Kind::Root,
            "attrib" | "attributes" => Kind::Attrib(parse_attrib(arg)),
            "len" => {
                let (lo, hi) = parse_u32_range(arg).unwrap_or((0, u32::MAX));
                Kind::Len(lo, hi)
            }
            "depth" | "parents" => {
                let (lo, hi) = parse_u32_range(arg).unwrap_or((0, u32::MAX));
                Kind::Depth(lo, hi)
            }
            "startwith" => Kind::StartWith(arg.to_string()),
            "endwith" => Kind::EndWith(arg.to_string()),
            "count" => Kind::Count(arg.parse().unwrap_or(u32::MAX)),
            "content" | "utf8content" | "ansicontent" | "utf16content" => Kind::Content(arg.to_string()),
            "type" => Kind::Type(arg.to_string()),
            "frn" => match parse_u64_range(arg) {
                Some((lo, hi)) => Kind::Frn(lo, hi),
                None => {
                    self.warnings.push(format!("frn: の値を解釈できない: {arg}"));
                    Kind::Unsupported(format!("frn:{arg}"))
                }
            },
            "fsi" => {
                let (lo, hi) = parse_u32_range(arg).unwrap_or((0, u32::MAX));
                Kind::Fsi(lo, hi)
            }
            "runcount" => {
                let (lo, hi) = parse_u32_range(arg).unwrap_or((0, u32::MAX));
                Kind::RunCount(lo, hi)
            }
            "dupe" => Kind::Dupe(DupeKey::Name),
            "namepartdupe" => Kind::Dupe(DupeKey::NamePart),
            "sizedupe" => Kind::Dupe(DupeKey::Size),
            "dmdupe" => Kind::Dupe(DupeKey::DateModified),
            "childcount" | "childfilecount" | "childfoldercount" => {
                let (lo, hi) = parse_u32_range(arg).unwrap_or((0, u32::MAX));
                let kind = match name {
                    "childfilecount" => ChildKind::Files,
                    "childfoldercount" => ChildKind::Folders,
                    _ => ChildKind::All,
                };
                Kind::ChildCount(lo, hi, kind)
            }
            other => {
                self.warnings.push(format!("未対応の関数: {other}:"));
                Kind::Unsupported(format!("{other}:{arg}"))
            }
        }
    }
}

fn apply_modifier(m: &mut Mods, head: &str) {
    match head {
        "case" => m.case = Some(true),
        "nocase" => m.case = Some(false),
        "path" => m.path = Some(true),
        "nopath" => m.path = Some(false),
        "wfn" | "wholefilename" => m.wfn = Some(true),
        "nowfn" | "nowholefilename" => m.wfn = Some(false),
        "wholeword" | "ww" => m.wholeword = Some(true),
        "nowholeword" | "noww" => m.wholeword = Some(false),
        "regex" => m.regex = Some(true),
        "noregex" => m.regex = Some(false),
        "file" | "files" => m.file_only = true,
        "folder" | "folders" => m.folder_only = true,
        "nofileonly" => m.file_only = false,
        "nofolderonly" => m.folder_only = false,
        "diacritics" => m.diacritics = Some(true),
        "nodiacritics" => m.diacritics = Some(false),
        "wildcards" => m.wildcards = Some(true),
        "nowildcards" => m.wildcards = Some(false),
        // ascii:/utf8:/noascii: は照合経路の指定（accio は常に同じ経路）
        _ => {}
    }
}

pub fn normalize_path(p: &str) -> String {
    let p = p.trim().trim_matches('"').replace('/', "\\");
    let p = p.trim_end_matches('\\');
    accio_index::volume::lowercase(p)
}

fn split_cmp(s: &str) -> (Cmp, &str) {
    if let Some(r) = s.strip_prefix("<=") {
        (Cmp::Le, r)
    } else if let Some(r) = s.strip_prefix(">=") {
        (Cmp::Ge, r)
    } else if let Some(r) = s.strip_prefix("!=") {
        (Cmp::Ne, r)
    } else if let Some(r) = s.strip_prefix("==") {
        (Cmp::Eq, r)
    } else if let Some(r) = s.strip_prefix('<') {
        (Cmp::Lt, r)
    } else if let Some(r) = s.strip_prefix('>') {
        (Cmp::Gt, r)
    } else if let Some(r) = s.strip_prefix('=') {
        (Cmp::Eq, r)
    } else {
        (Cmp::Eq, s)
    }
}

/// `size:` の値 → [lo, hi]（両端含む、バイト）。
pub fn parse_size_range(arg: &str) -> Option<(u64, u64)> {
    let a = arg.trim().to_ascii_lowercase();
    let consts: &[(&str, u64, u64)] = &[
        ("empty", 0, 0),
        ("tiny", 1, 10 << 10),
        ("small", (10 << 10) + 1, 100 << 10),
        ("medium", (100 << 10) + 1, 1 << 20),
        ("large", (1 << 20) + 1, 16 << 20),
        ("huge", (16 << 20) + 1, 128 << 20),
        ("gigantic", (128 << 20) + 1, u64::MAX),
        ("unknown", 0, 0),
    ];
    if let Some(&(_, lo, hi)) = consts.iter().find(|c| c.0 == a) {
        return Some((lo, hi));
    }
    if let Some((l, r)) = a.split_once("..").or_else(|| a.split_once('-')) {
        let (lo, _) = parse_size_value(l)?;
        let (_, hi) = parse_size_value(r)?;
        return Some((lo, hi));
    }
    let (cmp, v) = split_cmp(&a);
    let (lo, hi) = parse_size_value(v)?;
    Some(match cmp {
        Cmp::Eq => (lo, hi),
        Cmp::Lt => (0, lo.saturating_sub(1)),
        Cmp::Le => (0, hi),
        Cmp::Gt => (hi.saturating_add(1), u64::MAX),
        Cmp::Ge => (lo, u64::MAX),
        Cmp::Ne => (lo, hi), // 評価側で反転できないため = と同じ扱い（稀用）
    })
}

/// 数値＋単位 → その粒度での [lo, hi]。`1kb` → [1024, 2047]。
fn parse_size_value(v: &str) -> Option<(u64, u64)> {
    let v = v.trim();
    let num_end = v.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(v.len());
    let num: f64 = v[..num_end].parse().ok()?;
    let unit = v[num_end..].trim();
    let mult: u64 = match unit {
        "" | "b" | "bytes" => 1,
        "kb" | "k" => 1 << 10,
        "mb" | "m" => 1 << 20,
        "gb" | "g" => 1 << 30,
        "tb" | "t" => 1 << 40,
        "pb" => 1 << 50,
        _ => return None,
    };
    let lo = (num * mult as f64) as u64;
    let hi = if num.fract() == 0.0 && mult > 1 { lo + mult - 1 } else { lo };
    Some((lo, hi))
}

/// 日付式 → [lo, hi]（両端含む FILETIME）。
pub fn parse_date_expr(arg: &str) -> Option<(i64, i64)> {
    let a = arg.trim();
    if let Some((l, r)) = a.split_once("..") {
        let (lo, _) = parse_date_range(l)?;
        let (_, hi) = parse_date_range(r)?;
        return Some((lo, hi - 1));
    }
    let (cmp, v) = split_cmp(a);
    let (lo, hi) = parse_date_range(v)?;
    Some(match cmp {
        Cmp::Eq | Cmp::Ne => (lo, hi - 1),
        Cmp::Lt => (i64::MIN, lo - 1),
        Cmp::Le => (i64::MIN, hi - 1),
        Cmp::Gt => (hi, i64::MAX),
        Cmp::Ge => (lo, i64::MAX),
    })
}

fn parse_u32_range(arg: &str) -> Option<(u32, u32)> {
    let (lo, hi) = parse_u64_range(arg)?;
    Some((lo.min(u32::MAX as u64) as u32, hi.min(u32::MAX as u64) as u32))
}

fn parse_u64_range(arg: &str) -> Option<(u64, u64)> {
    let a = arg.trim();
    if let Some((l, r)) = a.split_once("..").or_else(|| a.split_once('-')) {
        return Some((l.parse().ok()?, r.parse().ok()?));
    }
    let (cmp, v) = split_cmp(a);
    let n: u64 = v.parse().ok()?;
    Some(match cmp {
        Cmp::Eq | Cmp::Ne => (n, n),
        Cmp::Lt => (0, n.saturating_sub(1)),
        Cmp::Le => (0, n),
        Cmp::Gt => (n.saturating_add(1), u64::MAX),
        Cmp::Ge => (n, u64::MAX),
    })
}

/// 属性文字（RHSDAC...）または数値 → FILE_ATTRIBUTE マスク。
pub fn parse_attrib(arg: &str) -> u32 {
    let a = arg.trim();
    if let Some(h) = a.strip_prefix("0x") {
        return u32::from_str_radix(h, 16).unwrap_or(0);
    }
    if let Ok(n) = a.parse::<u32>() {
        return n;
    }
    let mut m = 0;
    for c in a.chars() {
        m |= match c.to_ascii_uppercase() {
            'R' => 0x1,
            'H' => 0x2,
            'S' => 0x4,
            'D' => 0x10,
            'A' => 0x20,
            'N' => 0x80,
            'T' => 0x100,
            'P' => 0x200,
            'L' => 0x400,
            'C' => 0x800,
            'O' => 0x1000,
            'I' => 0x2000,
            'E' => 0x4000,
            'M' => 0x40_0000,
            _ => 0,
        };
    }
    m
}

pub struct Parsed {
    pub root: Node,
    pub warnings: Vec<String>,
}

pub fn parse(query: &str) -> Parsed {
    let toks = Lexer { s: query.as_bytes(), p: 0 }.tokens();
    let mut p = Parser { toks, p: 0, warnings: Vec::new() };
    let mut root = p.expr();
    // 余った `>` などを読み飛ばして残りも AND で結ぶ
    while p.peek().is_some() {
        p.p += 1;
        let more = p.expr();
        if more != Node::All {
            root = Node::And(vec![root, more]);
        }
    }
    Parsed { root, warnings: p.warnings }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Node {
        Node::Term(Term { kind: Kind::Text(s.into()), mods: Mods::default() })
    }

    #[test]
    fn precedence_or_binds_tighter() {
        let p = parse("a b|c");
        assert_eq!(p.root, Node::And(vec![text("a"), Node::Or(vec![text("b"), text("c")])]));
    }

    #[test]
    fn not_and_group() {
        let p = parse("!<a b> c");
        assert_eq!(p.root, Node::And(vec![Node::Not(Box::new(Node::And(vec![text("a"), text("b")]))), text("c")]));
    }

    #[test]
    fn quotes_keep_spaces() {
        let p = parse(r#""program files" ext:exe;dll"#);
        assert_eq!(
            p.root,
            Node::And(vec![
                text("program files"),
                Node::Term(Term { kind: Kind::Ext(vec!["exe".into(), "dll".into()]), mods: Mods::default() })
            ])
        );
    }

    #[test]
    fn modifiers_then_function() {
        let p = parse("folder:case:Foo");
        match p.root {
            Node::Term(t) => {
                assert_eq!(t.kind, Kind::Text("Foo".into()));
                assert!(t.mods.folder_only);
                assert_eq!(t.mods.case, Some(true));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size_range("1kb"), Some((1024, 2047)));
        assert_eq!(parse_size_range(">10mb"), Some((10 * (1 << 20) + (1 << 20) - 1 + 1, u64::MAX)));
        assert_eq!(parse_size_range("1mb..2mb").unwrap().0, 1 << 20);
        assert_eq!(parse_size_range("empty"), Some((0, 0)));
        assert_eq!(parse_size_range("gigantic").unwrap().0, (128 << 20) + 1);
    }

    #[test]
    fn comparison_inside_function_arg() {
        let p = parse("size:>400kb <a b>");
        match &p.root {
            Node::And(v) => {
                assert!(matches!(v[0], Node::Term(Term { kind: Kind::Size(_, u64::MAX), .. })));
                assert!(matches!(v[1], Node::And(_)));
            }
            _ => panic!("{:?}", p.root),
        }
    }

    #[test]
    fn macros() {
        match parse("audio:").root {
            Node::Term(Term { kind: Kind::Ext(v), .. }) => assert!(v.contains(&"mp3".to_string())),
            o => panic!("{o:?}"),
        }
        assert!(matches!(parse("video:foo").root, Node::And(_)));
    }

    #[test]
    fn drive_letter_is_text_not_function() {
        let p = parse(r"c:\users");
        assert_eq!(p.root, text(r"c:\users"));
    }

    #[test]
    fn dates() {
        assert!(parse_date_expr("today").is_some());
        assert!(parse_date_expr("monday").is_some());
        assert!(parse_date_expr("nextweek").is_some());
        assert!(parse_date_expr("coming3days").is_some());
        assert!(parse_date_expr("2024-05-06T13:45").is_some());
        let (lo, hi) = parse_date_expr("2024-05-06T13:45").unwrap();
        assert_eq!(hi - lo + 1, 60 * crate::datetime::TICKS_PER_SEC);
        match parse("frn:1234").root {
            Node::Term(Term { kind: Kind::Frn(1234, 1234), .. }) => {}
            other => panic!("{other:?}"),
        }
        match parse("runcount:>0").root {
            Node::Term(Term { kind: Kind::RunCount(1, u32::MAX), .. }) => {}
            other => panic!("{other:?}"),
        }
        set_custom_macros(vec![("src".into(), "ext:rs;c".into()), ("loop".into(), "loop:".into())]);
        match parse("src:").root {
            Node::Term(Term { kind: Kind::Ext(e), .. }) => assert_eq!(e, vec!["rs", "c"]),
            other => panic!("{other:?}"),
        }
        match parse("SRC:main").root {
            Node::And(v) => assert_eq!(v.len(), 2),
            other => panic!("{other:?}"),
        }
        assert!(!parse("loop:").warnings.is_empty());
        set_custom_macros(Vec::new());
        match parse("rc:today").root {
            Node::Term(Term { kind: Kind::DateModified(..), .. }) => {}
            other => panic!("{other:?}"),
        }
        assert!(parse_date_expr("2024").is_some());
        assert!(parse_date_expr("2024-03").is_some());
        assert!(parse_date_expr("2024-03-15").is_some());
        assert!(parse_date_expr("last7days").is_some());
        let (lo, hi) = parse_date_expr("2024-01-01..2024-12-31").unwrap();
        assert!(hi > lo);
    }
}
