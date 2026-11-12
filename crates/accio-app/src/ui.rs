//! メインウィンドウ。
//!
//! 2 つの形態がある。ピン留め（既定）は通常のウィンドウで、ファイルブラウザとして使える。
//! ピン留めを外すと Spotlight 風の浮動パネルになり、他の場所を触る（フォーカスを失う）と隠れる。
//! 検索の設定はすべて検索バー左端のハンバーガーに入っている。
//!
//! 操作モデル:
//! - 検索窓が常にキーボードフォーカスを持つ。文字を打てば即検索。
//! - ↑↓ / PgUp / PgDn / Ctrl+Home / Ctrl+End は結果の選択行を動かす（検索窓にフォーカスがあっても）。
//! - Enter は「選択行があるとき」だけ開く。Ctrl+Enter はパスを開く。何も選択していなければ何も起きない。
//! - Esc は検索語をクリア。
//! - 行クリックで選択、ダブルクリックで開く、右クリックでメニュー。
//!
//! 選択は行番号ではなく (ボリューム, エントリ) で覚えるので、索引更新で結果が入れ替わっても追従する。

use crate::engine::{Engine, VolState};
use crate::history::RunHistory;
use crate::searcher::Searcher;
use crate::settings::Settings;
use crate::shell;
use accio_query::{datetime::format_filetime, highlight_needles, Hit, Request, Response, SearchOpts, Sort, SortKey};
use egui::text::LayoutJob;
use egui::{Color32, FontTweak, Key, Modifiers, RichText, TextFormat};

/// 結果列。Everything の既定列＋追加可能な列。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Col {
    Name,
    Path,
    Size,
    DateModified,
    DateCreated,
    Extension,
    Attributes,
    Type,
    RunCount,
    DateAccessed,
    DateRun,
}

impl Col {
    pub const ALL: [Col; 11] = [Col::Name, Col::Path, Col::Size, Col::DateModified, Col::DateCreated, Col::DateAccessed, Col::Extension, Col::Attributes, Col::Type, Col::RunCount, Col::DateRun];
    fn label(self) -> &'static str {
        match self {
            Col::Name => crate::i18n::tr("名前"),
            Col::Path => crate::i18n::tr("パス"),
            Col::Size => crate::i18n::tr("サイズ"),
            Col::DateModified => crate::i18n::tr("更新日時"),
            Col::DateCreated => crate::i18n::tr("作成日時"),
            Col::Extension => crate::i18n::tr("拡張子"),
            Col::Attributes => crate::i18n::tr("属性"),
            Col::Type => crate::i18n::tr("種類"),
            Col::RunCount => crate::i18n::tr("実行回数"),
            Col::DateAccessed => crate::i18n::tr("アクセス日時"),
            Col::DateRun => crate::i18n::tr("実行日時"),
        }
    }
    fn key(self) -> &'static str {
        match self {
            Col::Name => "name",
            Col::Path => "path",
            Col::Size => "size",
            Col::DateModified => "dm",
            Col::DateCreated => "dc",
            Col::Extension => "ext",
            Col::Attributes => "attrib",
            Col::Type => "type",
            Col::RunCount => "runcount",
            Col::DateAccessed => "da",
            Col::DateRun => "daterun",
        }
    }
    fn from_key(k: &str) -> Option<Col> {
        Col::ALL.iter().copied().find(|c| c.key() == k)
    }
    fn sort_key(self) -> Option<SortKey> {
        match self {
            Col::Name => Some(SortKey::Name),
            Col::Path => Some(SortKey::Path),
            Col::Size => Some(SortKey::Size),
            Col::DateModified => Some(SortKey::DateModified),
            Col::DateCreated => Some(SortKey::DateCreated),
            Col::Extension => Some(SortKey::Extension),
            Col::Attributes => None,
            Col::Type => None,
            Col::RunCount => Some(SortKey::RunCount),
            Col::DateAccessed => Some(SortKey::DateAccessed),
            Col::DateRun => Some(SortKey::DateRun),
        }
    }
}

/// Everything 互換の属性文字列（R H S D A N T P L C O I E）。
fn attrib_string(a: u32) -> String {
    let mut s = String::new();
    for (bit, c) in [(0x1, 'R'), (0x2, 'H'), (0x4, 'S'), (0x10, 'D'), (0x20, 'A'), (0x80, 'N'), (0x100, 'T'), (0x200, 'P'), (0x400, 'L'), (0x800, 'C'), (0x1000, 'O'), (0x2000, 'I'), (0x4000, 'E')] {
        if a & bit != 0 {
            s.push(c);
        }
    }
    s
}

/// 名前中の検索語を太字＋色で強調したレイアウト。
/// 実行回数/実行日時の表（ヒット → (回数, 最終実行)）
type RunInfoMap = Arc<std::collections::HashMap<Hit, (u32, i64)>>;

/// フォルダ/ファイルの種別アイコンを矩形に描く（フォントに頼らない）。
fn paint_kind_icon_at(p: &egui::Painter, r: egui::Rect, is_dir: bool) {
    let w = r.width();
    let h = r.height();
    if is_dir {
        // フォルダ: 本体＋タブ
        let body = egui::Rect::from_min_size(egui::pos2(r.min.x, r.min.y + h * 0.22), egui::vec2(w, h * 0.68));
        let tab = egui::Rect::from_min_size(egui::pos2(r.min.x, r.min.y + h * 0.08), egui::vec2(w * 0.45, h * 0.22));
        let fill = Color32::from_rgb(245, 190, 70);
        let dark = Color32::from_rgb(214, 155, 40);
        p.rect_filled(tab, 1.5, dark);
        p.rect_filled(body, 1.5, fill);
        p.rect_filled(egui::Rect::from_min_size(body.min, egui::vec2(w, h * 0.12)), 0.0, dark);
    } else {
        // ファイル: 右上を折った紙
        let page = egui::Rect::from_min_size(egui::pos2(r.min.x + w * 0.15, r.min.y + h * 0.05), egui::vec2(w * 0.7, h * 0.9));
        let fold = w * 0.25;
        let stroke = egui::Stroke::new(1.0_f32, Color32::from_gray(140));
        let pts = vec![
            page.min,
            egui::pos2(page.max.x - fold, page.min.y),
            egui::pos2(page.max.x, page.min.y + fold),
            egui::pos2(page.max.x, page.max.y),
            egui::pos2(page.min.x, page.max.y),
        ];
        p.add(egui::Shape::convex_polygon(pts, Color32::from_gray(250), stroke));
        p.line_segment([egui::pos2(page.max.x - fold, page.min.y), egui::pos2(page.max.x - fold, page.min.y + fold)], stroke);
        p.line_segment([egui::pos2(page.max.x - fold, page.min.y + fold), egui::pos2(page.max.x, page.min.y + fold)], stroke);
        for k in 0..3 {
            let y = page.min.y + h * (0.42 + 0.16 * k as f32);
            p.line_segment([egui::pos2(page.min.x + w * 0.15, y), egui::pos2(page.max.x - w * 0.15, y)], egui::Stroke::new(1.0_f32, Color32::from_gray(200)));
        }
    }
}

/// 行の先頭に 1 行の高さのアイコンを置く。
fn paint_kind_icon(ui: &mut egui::Ui, is_dir: bool) {
    let side = ui.text_style_height(&egui::TextStyle::Body).min(16.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(side + 2.0, side), egui::Sense::hover());
    paint_kind_icon_at(ui.painter(), egui::Rect::from_min_size(rect.min, egui::vec2(side, side)), is_dir);
}

/// 結果 1 行の表示に要る値。索引のロックが取れなかったフレームの再利用にも使う。
struct RowData {
    is_dir: bool,
    name: String,
    path: String,
    size: u64,
    mtime: i64,
    ctime: i64,
    atime: i64,
    attrs: u32,
    ext: String,
}

impl RowData {
    fn read(g: &accio_index::VolumeIndex, id: u32) -> Self {
        Self {
            is_dir: g.is_dir(id),
            name: g.name(id).to_string(),
            path: g.parent_path(id),
            size: g.size[id as usize],
            mtime: g.mtime(id),
            ctime: g.ctime(id),
            atime: g.atime(id),
            attrs: g.attrs[id as usize],
            ext: String::from_utf8_lossy(g.ext(id)).into_owned(),
        }
    }
}

/// 短い待ちで読み取りロックを試す。索引の書き込み（USN 反映、保存前の圧縮）で待たされるなら諦める。
fn try_read_vol(v: &accio_index::SharedVolume) -> Option<parking_lot::RwLockReadGuard<'_, accio_index::VolumeIndex>> {
    v.try_read_for(Duration::from_millis(2))
}

/// `RRGGBB` / `#RRGGBB` → 色。
fn parse_color(s: &str) -> Option<Color32> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

/// 利用者が指定できる色（accio.ini の color_*=RRGGBB）。
#[derive(Clone, Copy)]
struct Colors {
    highlight: Color32,
    selection: Option<Color32>,
    text: Option<Color32>,
    background: Option<Color32>,
}

impl Colors {
    fn load(settings: &crate::settings::Settings) -> Self {
        let get = |k: &str| settings.get(k).and_then(parse_color);
        Self {
            highlight: get("color_highlight").unwrap_or(Color32::from_rgb(200, 60, 0)),
            selection: get("color_selection"),
            text: get("color_text"),
            background: get("color_background"),
        }
    }
    fn apply(&self, ctx: &egui::Context) {
        let c = *self;
        ctx.all_styles_mut(|style| {
            if let Some(s) = c.selection {
                style.visuals.selection.bg_fill = s;
            }
            style.visuals.override_text_color = c.text;
            if let Some(b) = c.background {
                style.visuals.panel_fill = b;
                style.visuals.window_fill = b;
                style.visuals.extreme_bg_color = b;
            }
        });
    }
}

fn highlighted(name: &str, needles: &[String], color: Color32, font: egui::FontId, highlight: Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    let normal = TextFormat { font_id: font.clone(), color, ..Default::default() };
    let strong = TextFormat { font_id: font, color: highlight, ..Default::default() };
    if needles.is_empty() {
        job.append(name, 0.0, normal);
        return job;
    }
    // 折り畳み後の文字列と、その各バイトが元の文字列のどのバイト位置（文字の先頭）に対応するか
    let mut lower = String::with_capacity(name.len());
    let mut origin: Vec<usize> = Vec::with_capacity(name.len());
    for (off, c) in name.char_indices() {
        let before = lower.len();
        for l in c.to_lowercase() {
            accio_index::volume::fold_into(l, &mut lower);
        }
        origin.resize(lower.len(), off);
        debug_assert!(lower.len() >= before);
    }
    let mut marks = vec![false; name.len()];
    for n in needles {
        let mut start = 0;
        while let Some(p) = lower[start..].find(n.as_str()) {
            let (a, b) = (start + p, start + p + n.len());
            let oa = origin[a];
            let mut ob = if b < origin.len() { origin[b] } else { name.len() };
            if ob <= oa {
                // 1 文字が複数バイトに折り畳まれた途中で終わる（ß の "s" など）: その文字全体を強調
                ob = oa + name[oa..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
            }
            for m in &mut marks[oa..ob] {
                *m = true;
            }
            start += p + n.len().max(1);
        }
    }
    let mut i = 0;
    while i < name.len() {
        let on = marks[i];
        let mut j = i + 1;
        while j < name.len() && (marks[j] == on || !name.is_char_boundary(j)) {
            j += 1;
        }
        while !name.is_char_boundary(j) {
            j += 1;
        }
        job.append(&name[i..j], 0.0, if on { strong.clone() } else { normal.clone() });
        i = j;
    }
    job
}
use egui_extras::{Column, TableBuilder};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Latin は Segoe UI、CJK は Yu Gothic にフォールバックする。両者のベースラインが揃うよう
/// Yu Gothic 側を少し下げる（実画面で調整した値）。
/// `font` は追加のフォントファイル（最優先）、`font_size` は基準の文字サイズ（px、既定 13）。
pub fn install_fonts(ctx: &egui::Context, font: Option<&str>, font_size: f32) {
    let mut fonts = egui::FontDefinitions::default();
    let mut order: Vec<String> = Vec::new();
    if let Some(path) = font.map(str::trim).filter(|p| !p.is_empty()) {
        match std::fs::read(path) {
            Ok(b) => {
                fonts.font_data.insert("user".into(), Arc::new(egui::FontData::from_owned(b)));
                order.push("user".into());
            }
            Err(e) => log::warn!("フォント {path} を読めない: {e}"),
        }
    }
    // ラテン文字と日本語を別フォントで混ぜるとベースラインがずれる。Yu Gothic UI（YuGothM.ttc の 2 番目のフェイス、
    // ラテン文字は Segoe UI と同じ設計）を 1 つだけ使う。無ければ Meiryo UI（meiryo.ttc の 3 番目）
    for (name, path, index) in [("yugothic-ui", r"C:\Windows\Fonts\YuGothM.ttc", 1u32), ("meiryo-ui", r"C:\Windows\Fonts\meiryo.ttc", 2u32)] {
        if let Ok(b) = std::fs::read(path) {
            let data = egui::FontData { font: std::borrow::Cow::Owned(b), index, tweak: FontTweak::default() };
            fonts.font_data.insert(name.into(), Arc::new(data));
            order.push(name.into());
            break;
        }
    }
    for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        let list = fonts.families.entry(fam).or_default();
        for (i, n) in order.iter().enumerate() {
            list.insert(i, n.clone());
        }
    }
    ctx.set_fonts(fonts);
    // set_style は現在のテーマの style にしか効かない（ライト/ダークで別々）ので両方に適用する
    let scale = (font_size / 13.0).clamp(0.5, 3.0);
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing.y = 2.0;
        for (k, f) in egui::Style::default().text_styles {
            style.text_styles.insert(k, egui::FontId::new(f.size * scale, f.family));
        }
        // ラベルをテキスト選択可能にすると、結果行のセル上のクリック/ドラッグをラベルが奪い
        // 行が選択できなくなる（Everything の一覧もテキスト選択はしない）
        style.interaction.selectable_labels = false;
        // egui はどのマウスボタンでも「押下中」の見た目になる（右クリックでも凹む）。実際に反応するのは左だけなので
        // 押下中の見た目を無くし、ホバーと同じにする（反応したように見えて何も起きない混乱を避ける）
        style.visuals.widgets.active = style.visuals.widgets.hovered;
        // 入力カーソルは点滅させない（点滅と再描画が重なると「ちらつき」に見える）
        style.visuals.text_cursor.blink = false;
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Search,
    DiskUsage,
}

#[derive(Clone, Copy)]
enum TabCmd {
    New,
    Close,
    Next,
    Prev,
    Go(usize),
}

pub struct App {
    engine: Arc<Engine>,
    searcher: Arc<Searcher>,
    query: String,
    opts: SearchOpts,
    sort: Sort,
    mode: Mode,
    du_path: Option<String>,
    last_submitted: Option<(String, SearchOpts, Sort)>,
    last_submit_at: Instant,
    submitted_index_gen: u64,
    /// 選択中のエントリ。行番号は結果が変わるたびに引き直す。
    selected: Option<Hit>,
    selected_row: Option<usize>,
    seen_result: Option<Arc<Response>>,
    scroll_to: Option<usize>,
    search_id: egui::Id,
    columns: Vec<Col>,
    needles: Vec<String>,
    settings: Settings,
    settings_path: std::path::PathBuf,
    tray: Option<crate::tray::Tray>,
    hidden: bool,
    allow_close: bool,
    run_in_background: bool,
    /// 複数選択（`selected` はフォーカス/アンカー）
    multi: Vec<Hit>,
    /// インラインリネーム中: (対象, 編集中の名前)
    renaming: Option<(Hit, String)>,
    /// ごみ箱確認ダイアログ: 対象パス
    confirm_delete: Option<Vec<String>>,
    show_settings: bool,
    /// 開いているファイルリストの表示名（検索対象が索引ではなくリストになっている）
    file_list: Option<String>,
    /// 直近に描いた行の内容。索引が書き込み中でロックが取れないフレームはこれを描く
    row_cache: std::collections::HashMap<Hit, Arc<RowData>>,
    /// 実行回数/実行日時の表（結果が変わるまで再計算しない）
    run_info_cache: Option<(Arc<Response>, RunInfoMap)>,
    file_list_rx: Option<std::sync::mpsc::Receiver<FileListResult>>,
    settings_hotkey: String,
    settings_http_port: String,
    start_on_login: bool,
    notice: Option<(String, Instant)>,
    /// 組み込みフィルタ（Everything のフィルタバー相当）
    filter: usize,
    bookmarks: Vec<(String, String)>,
    show_bookmarks: bool,
    run_history: RunHistory,
    /// 外部（二重起動や IPC）から受け取ったコマンド
    pub external: Arc<parking_lot::Mutex<Vec<crate::ipc::ExternalCommand>>>,
    /// メインウィンドウの HWND（シェルメニューの親に使う）
    hwnd: isize,
    exclude_hidden: bool,
    exclude_system: bool,
    exclude_folders: Vec<String>,
    settings_folders: String,
    settings_exclude_folders: String,
    settings_folder_rescan: String,
    /// ワイルドカード除外（`*.tmp;*.log`）
    exclude_files: Vec<String>,
    settings_exclude_files: String,
    /// 対象フォルダの限定（空なら全体）
    include_only: Vec<String>,
    settings_include_only: String,
    /// ユーザー定義フィルタ (名前, 検索式)。組み込みフィルタの後ろに並ぶ
    /// (名前, 検索式, マクロ名)。マクロ名があれば検索語の中で `マクロ名:` として使える
    custom_filters: Vec<(String, String, String)>,
    settings_custom_filters: String,
    /// 0 = システム, 1 = ライト, 2 = ダーク
    theme: u8,
    /// 表示倍率（%）
    zoom: u8,
    /// 検索対象から外したドライブ（表示名 `C:` など）。空なら全部
    drives_off: std::collections::BTreeSet<String>,
    colors: Colors,
    settings_colors: [String; 4],
    settings_font: String,
    font_size: u8,
    theme_applied: bool,
    last_rect: Option<(f32, f32, f32, f32)>,
    last_maximized: bool,
    /// 実行回数ソートの結果キャッシュ: (元, 並べ替え後)
    runcount_sorted: Option<(Arc<Response>, Arc<Response>)>,
    startup_hide_pending: bool,
    startup_frames: u32,
    /// 一括リネームダイアログ
    bulk: Option<BulkRename>,
    show_preview: bool,
    previewer: crate::preview::Previewer,
    preview_for: Option<Hit>,
    preview_tex: Option<(String, egui::TextureHandle)>,
    /// タブ（Everything 1.5 相当）。アクティブなタブの状態は App のフィールドに展開されている
    tabs: Vec<TabState>,
    active_tab: usize,
    /// ドラッグ開始候補の行（押下位置から一定以上動いたら OLE ドラッグを始める）
    drag_candidate: Option<(usize, egui::Pos2)>,
    /// 表示モード: 0 = 詳細, それ以外 = サムネイルの一辺（px）
    thumb_size: u32,
    thumbs: crate::preview::Thumbs,
    /// パス → テクスチャ（None は画像でない/読めない）
    thumb_cache: std::collections::HashMap<String, Option<egui::TextureHandle>>,
    thumb_pending: std::collections::HashSet<String>,
    /// 検証用スクリプト（ACCIO_UI_SCRIPT）: egui のイベントを注入して UI の振る舞いを確かめる
    ui_script: Vec<String>,
    ui_script_wait: u32,
    /// 直前のフレームで描いた行の矩形（スクリプトの click:<row> に使う）
    row_rects: Vec<(usize, egui::Rect)>,
    last_copied: String,
    /// ピン留め（通常のウィンドウ）。false なら Spotlight 風の浮動パネルで、フォーカスを失うと隠れる
    pinned: bool,
    /// OS のウィンドウに適用済みの形態（None なら未適用）
    mode_applied: Option<bool>,
    /// 直近に表示した時刻。表示直後はフォーカスが確定していないので、その間は隠さない
    shown_at: Instant,
    /// 浮動パネルの直近の高さ
    float_h: f32,
    /// 浮動パネルのカードの実測の高さ（直前のフレーム。0 なら未計測）
    float_content_h: f32,
}

/// タブごとに保持する検索状態。
#[derive(Clone, Default)]
struct TabState {
    query: String,
    opts: SearchOpts,
    sort: Sort,
    filter: usize,
    du: bool,
    du_path: Option<String>,
    selected: Option<Hit>,
}

/// 複数選択の一括リネーム（Everything の「Advanced Rename」相当の最小版）。
/// 検索/置換（通常文字列または正規表現、`$1` 参照可）で新しい名前を作り、プレビューしてから適用する。
pub struct BulkRename {
    pub paths: Vec<String>,
    pub find: String,
    pub replace: String,
    pub regex: bool,
    pub match_case: bool,
    pub error: Option<String>,
}

impl BulkRename {
    pub fn preview(&self) -> (Vec<(String, String)>, Option<String>) {
        let mut out = Vec::new();
        let re = if self.regex && !self.find.is_empty() {
            match regex::RegexBuilder::new(&self.find).case_insensitive(!self.match_case).build() {
                Ok(r) => Some(r),
                Err(e) => return (out, Some(crate::i18n::trf("正規表現エラー: {}", &[&e]))),
            }
        } else {
            None
        };
        for p in &self.paths {
            let name = p.rsplit('\\').next().unwrap_or(p).to_string();
            let new = if self.find.is_empty() {
                name.clone()
            } else if let Some(re) = &re {
                // `$1_$2` のような参照は Rust regex では `${1}_${2}` と書く必要があるので変換する
                let rep = dollar_ref_re().replace_all(&self.replace, "$${$1}").into_owned();
                re.replace_all(&name, rep.as_str()).into_owned()
            } else if self.match_case {
                name.replace(&self.find, &self.replace)
            } else {
                // 大文字小文字を無視した置換
                let lower = name.to_lowercase();
                let f = self.find.to_lowercase();
                if lower.len() != name.len() {
                    name.replace(&self.find, &self.replace)
                } else {
                    let mut out = String::new();
                    let mut i = 0;
                    while i < name.len() {
                        if lower[i..].starts_with(&f) {
                            out.push_str(&self.replace);
                            i += f.len();
                        } else {
                            let ch = name[i..].chars().next().unwrap();
                            out.push(ch);
                            i += ch.len_utf8();
                        }
                    }
                    out
                }
            };
            out.push((name, new));
        }
        (out, None)
    }
}

/// Everything 既定のフィルタ。(表示名, クエリに前置する語)
/// `名前|検索式[|マクロ]` → (名前, 検索式, マクロ)。名前か検索式が空なら None。
fn parse_custom_filter(line: &str) -> Option<(String, String, String)> {
    let mut it = line.split('|').map(|s| s.trim());
    let name = it.next()?.to_string();
    let query = it.next()?.to_string();
    let macro_name = it.next().unwrap_or("").trim_end_matches(':').to_string();
    if name.is_empty() || query.is_empty() {
        return None;
    }
    Some((name, query, macro_name))
}

fn format_custom_filter((n, q, m): &(String, String, String)) -> String {
    if m.is_empty() {
        format!("{n}|{q}")
    } else {
        format!("{n}|{q}|{m}")
    }
}

fn custom_macros(filters: &[(String, String, String)]) -> Vec<(String, String)> {
    filters.iter().filter(|(_, _, m)| !m.is_empty()).map(|(_, q, m)| (m.clone(), q.clone())).collect()
}

/// ファイルリスト読み込みの結果: (表示名, ルートごとの索引) か エラー文
type FileListResult = Result<(String, Vec<accio_index::VolumeIndex>), String>;

/// 置換文字列中の `$1` 参照（Rust regex では `${1}` と書く必要がある）
fn dollar_ref_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"\$(\d+)").unwrap())
}

/// 浮動パネル（ピン留めを外した状態）の幅と余白（pt）。余白は影のぶん
const FLOAT_W: f32 = 760.0;
const FLOAT_MARGIN: f32 = 24.0;
const FLOAT_ROWS_MAX: usize = 10;
/// 検索バーの高さと、コンパクト表示の行の高さ（pt）
const BAR_H: f32 = 52.0;
const ROW_H: f32 = 46.0;

/// 検索バー/ハンバーガーから update() へ返す操作
#[derive(Default)]
struct BarActions {
    export: bool,
    open_file_list: bool,
    close_file_list: bool,
    toggle_pin: bool,
    /// 表示の都合でピン留めが必要（サムネイル/プレビュー/設定）
    need_pin: bool,
}

/// 結果行から update() へ返す操作
#[derive(Default)]
struct RowActions {
    clicked: Option<(usize, (bool, bool))>,
    double: Option<usize>,
    rename_req: Option<Hit>,
    delete_req: bool,
    shell_menu_req: bool,
    copy_files_req: bool,
    du_into: Option<String>,
    drag_start: Option<(usize, egui::Pos2)>,
}

/// 結果行の右クリックメニュー（コンパクト表示と表で共通）
fn row_context_menu(ui: &mut egui::Ui, full: &str, name: &str, is_dir: bool, multi_len: usize, hit: Hit, out: &mut RowActions) {
    let t = crate::i18n::tr;
    if ui.button(t("開く (Enter)")).clicked() {
        shell::open(full);
        ui.close();
    }
    if ui.button(t("パスを開く (Ctrl+Enter)")).clicked() {
        shell::open_path(full);
        ui.close();
    }
    if ui.button(t("コピー (Ctrl+C)")).clicked() {
        out.copy_files_req = true;
        ui.close();
    }
    if ui.button(t("フルパスをコピー (Ctrl+Shift+C)")).clicked() {
        ui.ctx().copy_text(full.to_string());
        ui.close();
    }
    if ui.button(if multi_len > 1 { t("一括リネーム (F2)") } else { t("名前の変更 (F2)") }).clicked() {
        out.rename_req = Some(hit);
        ui.close();
    }
    if ui.button(t("Explorer のメニュー…")).clicked() {
        out.shell_menu_req = true;
        ui.close();
    }
    if ui.button(t("ごみ箱へ移動 (Del)")).clicked() {
        out.delete_req = true;
        ui.close();
    }
    if ui.button(t("名前をコピー")).clicked() {
        ui.ctx().copy_text(name.to_string());
        ui.close();
    }
    if is_dir && ui.button(t("このフォルダの容量内訳")).clicked() {
        out.du_into = Some(full.to_string());
        ui.close();
    }
    if !is_dir && ui.button(t("管理者として実行 (Ctrl+Shift+Enter)")).clicked() {
        shell::open_as_admin(full);
        ui.close();
    }
    if ui.button(t("プロパティ (Alt+Enter)")).clicked() {
        shell::properties(full);
        ui.close();
    }
}

pub const FILTERS: [(&str, &str); 8] = [
    ("すべて", ""),
    ("音声", "audio:"),
    ("圧縮", "zip:"),
    ("文書", "doc:"),
    ("実行", "exe:"),
    ("フォルダ", "folder:"),
    ("画像", "pic:"),
    ("動画", "video:"),
];

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        engine: Arc<Engine>,
        initial: crate::CliOptions,
        settings_path: std::path::PathBuf,
        external: Arc<parking_lot::Mutex<Vec<crate::ipc::ExternalCommand>>>,
    ) -> Self {
        let ctx = cc.egui_ctx.clone();
        let repaint: Arc<dyn Fn() + Send + Sync> = Arc::new(move || ctx.request_repaint());
        let searcher = Searcher::spawn(engine.index.clone(), engine.generation.clone(), repaint.clone());
        let settings = Settings::load(&settings_path);
        let hotkey = settings.get("hotkey").unwrap_or("ctrl+alt+a").to_string();
        let run_in_background = settings.bool("run_in_background", true);
        let http_port = settings.get("http_port").unwrap_or("80").to_string();
        let data_dir = settings_path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        if let Some(p) = initial.filelist.as_deref() {
            if let Ok(text) = std::fs::read_to_string(p) {
                let vols = accio_index::filelist::build(&accio_index::filelist::parse(&text));
                engine.index.set_overlay(Some(vols.into_iter().map(|v| Arc::new(parking_lot::RwLock::new(v))).collect()));
                engine.generation.fetch_add(1, Ordering::Relaxed);
            }
        }
        let run_history = RunHistory::load(&data_dir.join("Run History.csv"));
        accio_query::set_run_history(run_history.snapshot());
        let exclude_hidden = settings.bool("exclude_hidden", false);
        let exclude_system = settings.bool("exclude_system", false);
        let exclude_folders: Vec<String> = settings.get("exclude_folders").map(|v| v.split(';').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()).unwrap_or_default();
        let settings_folders = settings.get("folders").unwrap_or("").replace(';', "\n");
        let settings_exclude_folders = settings.get("exclude_folders").unwrap_or("").replace(';', "\n");
        let settings_folder_rescan = settings.get("folder_rescan_minutes").unwrap_or("0").to_string();
        let theme: u8 = settings.get("theme").and_then(|v| v.parse().ok()).unwrap_or(0);
        let zoom: u8 = settings.get("zoom").and_then(|v| v.parse().ok()).filter(|z| (50..=250).contains(z)).unwrap_or(100);
        let drives_off: std::collections::BTreeSet<String> = settings.list("drives_off").into_iter().collect();
        let colors = Colors::load(&settings);
        let settings_colors = ["color_highlight", "color_selection", "color_text", "color_background"].map(|k| settings.get(k).unwrap_or("").to_string());
        let settings_font = settings.get("font").unwrap_or("").to_string();
        let font_size: u8 = settings.get("font_size").and_then(|v| v.parse().ok()).filter(|z| (8..=40).contains(z)).unwrap_or(13);
        let show_preview = settings.bool("preview", false);
        let thumb_size: u32 = settings.get("thumb_size").and_then(|v| v.parse().ok()).unwrap_or(0);
        // セッション復元（タブと検索語）。-s 指定時はそのタブを先頭に追加する
        let restore_session = settings.bool("restore_session", true);
        let mut tabs: Vec<TabState> = if restore_session {
            (0..32)
                .filter_map(|i| settings.get(&format!("session.tab.{i}")))
                .filter(|q| !q.is_empty())
                .map(|q| TabState { query: q.to_string(), ..Default::default() })
                .collect()
        } else {
            Vec::new()
        };
        let mut active_tab: usize = settings.get("session.active").and_then(|v| v.parse().ok()).unwrap_or(0);
        if tabs.is_empty() {
            tabs.push(TabState::default());
            active_tab = 0;
        }
        active_tab = active_tab.min(tabs.len() - 1);
        let restored_query = tabs[active_tab].query.clone();
        let exclude_files: Vec<String> = settings.list("exclude_files");
        let settings_exclude_files = exclude_files.join(";");
        let include_only: Vec<String> = settings.list("include_only");
        let settings_include_only = include_only.join("\n");
        accio_query::set_search_prefix(crate::settings::exclude_prefix(exclude_hidden, exclude_system, &exclude_folders, &exclude_files, &include_only));
        let custom_filters: Vec<(String, String, String)> =
            (0..64).filter_map(|i| settings.get(&format!("filter.{i}")).and_then(parse_custom_filter)).collect();
        let settings_custom_filters = custom_filters.iter().map(format_custom_filter).collect::<Vec<_>>().join("\n");
        accio_query::parser::set_custom_macros(custom_macros(&custom_filters));
        let bookmarks: Vec<(String, String)> = (0..64)
            .filter_map(|i| settings.get(&format!("bookmark.{i}")).and_then(|v| v.split_once('|')).map(|(n, q)| (n.to_string(), q.to_string())))
            .collect();
        let pinned = settings.bool("pinned", true);
        let tray = match crate::tray::Tray::new(&hotkey, repaint.clone()) {
            Ok(t) => Some(t),
            Err(e) => {
                log::warn!("トレイアイコンを作成できない: {e}");
                None
            }
        };
        let columns: Vec<Col> = settings
            .get("columns")
            .map(|v| v.split(',').filter_map(Col::from_key).collect::<Vec<_>>())
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| vec![Col::Name, Col::Path, Col::Size, Col::DateModified]);
        let sort = initial.sort.unwrap_or_else(|| Sort {
            key: match settings.get("sort") {
                Some("path") => SortKey::Path,
                Some("size") => SortKey::Size,
                Some("dm") => SortKey::DateModified,
                Some("dc") => SortKey::DateCreated,
                Some("ext") => SortKey::Extension,
                Some("runcount") => SortKey::RunCount,
                Some("da") => SortKey::DateAccessed,
                Some("daterun") => SortKey::DateRun,
                _ => SortKey::Name,
            },
            ascending: settings.bool("sort_ascending", true),
        });
        Self {
            engine,
            searcher,
            query: initial.search.unwrap_or(restored_query),
            opts: SearchOpts {
                match_case: initial.case || settings.bool("match_case", false),
                match_path: initial.path || settings.bool("match_path", false),
                whole_word: initial.whole_word || settings.bool("whole_word", false),
                regex: initial.regex || settings.bool("regex", false),
                match_diacritics: initial.diacritics || settings.bool("match_diacritics", false),
            },
            sort,
            columns,
            needles: Vec::new(),
            settings,
            settings_path,
            tray,
            hidden: initial.startup,
            allow_close: false,
            run_in_background,
            multi: Vec::new(),
            renaming: None,
            confirm_delete: None,
            show_settings: false,
            row_cache: Default::default(),
            run_info_cache: None,
            file_list: initial.filelist.as_deref().filter(|p| std::path::Path::new(p).is_file()).map(|p| std::path::Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_string())),
            file_list_rx: None,
            settings_hotkey: hotkey.clone(),
            settings_http_port: http_port,
            start_on_login: shell::is_start_on_login(),
            notice: None,
            filter: initial
                .filter
                .as_deref()
                .and_then(|f| {
                    FILTERS.iter().position(|(n, _)| n.eq_ignore_ascii_case(f)).or_else(|| custom_filters.iter().position(|(n, _, _)| n.eq_ignore_ascii_case(f)).map(|i| FILTERS.len() + i))
                })
                .unwrap_or(0),
            bookmarks,
            show_bookmarks: false,
            run_history,
            external,
            hwnd: 0,
            exclude_hidden,
            exclude_system,
            exclude_folders,
            settings_folders,
            settings_exclude_folders,
            settings_folder_rescan,
            exclude_files,
            settings_exclude_files,
            include_only,
            settings_include_only,
            custom_filters,
            settings_custom_filters,
            theme,
            zoom,
            drives_off,
            colors,
            settings_colors,
            settings_font,
            font_size,
            theme_applied: false,
            last_rect: None,
            last_maximized: false,
            runcount_sorted: None,
            startup_hide_pending: initial.startup,
            startup_frames: 0,
            bulk: None,
            show_preview,
            previewer: crate::preview::Previewer::new(repaint.clone()),
            preview_for: None,
            preview_tex: None,
            tabs,
            active_tab,
            drag_candidate: None,
            thumb_size,
            thumbs: crate::preview::Thumbs::new(repaint.clone()),
            thumb_cache: Default::default(),
            thumb_pending: Default::default(),
            ui_script: std::env::var("ACCIO_UI_SCRIPT").ok().map(|s| s.split(';').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()).unwrap_or_default(),
            ui_script_wait: 30,
            row_rects: Vec::new(),
            last_copied: String::new(),
            pinned,
            mode_applied: None,
            shown_at: Instant::now(),
            float_h: 0.0,
            float_content_h: 0.0,
            mode: Mode::Search,
            du_path: None,
            last_submitted: None,
            last_submit_at: Instant::now(),
            submitted_index_gen: u64::MAX,
            selected: None,
            selected_row: None,
            seen_result: None,
            scroll_to: None,
            search_id: egui::Id::new("search_box"),
        }
    }

    fn effective_request(&self) -> Request {
        let prefix = if self.filter < FILTERS.len() {
            FILTERS[self.filter].1.to_string()
        } else {
            self.custom_filters.get(self.filter - FILTERS.len()).map(|(_, q, _)| format!("<{q}>")).unwrap_or_default()
        };
        // 除外設定は accio_query::set_search_prefix で全検索経路に効かせている（ここでは付けない）
        // ドライブの絞り込み: 外したドライブがあれば、残りのボリューム番号を fsi: で列挙する
        let mut prefix = prefix;
        if !self.drives_off.is_empty() {
            let vols = self.engine.index.snapshot();
            let keep: Vec<String> = vols
                .iter()
                .enumerate()
                .filter_map(|(i, v)| {
                    let root = v.try_read_for(Duration::from_millis(5)).map(|g| g.meta.display_root.clone())?;
                    (!self.drives_off.contains(&root)).then(|| format!("fsi:{i}"))
                })
                .collect();
            if !keep.is_empty() {
                prefix.push_str(&format!(" <{}>", keep.join("|")));
            }
        }
        let query = match self.mode {
            Mode::Search => format!("{prefix} {}", self.query),
            Mode::DiskUsage => match &self.du_path {
                Some(p) => format!("parent:\"{p}\" {}", self.query),
                None => format!("folder: depth:1 {}", self.query),
            },
        };
        Request { query, opts: self.opts, sort: self.sort, ..Default::default() }
    }

    /// 検索語もフィルタも無いなら検索しない（何も表示しない）。
    fn nothing_to_search(&self) -> bool {
        self.mode == Mode::Search && self.query.trim().is_empty() && self.filter == 0 && self.file_list.is_none()
    }

    fn maybe_submit(&mut self) {
        if self.nothing_to_search() {
            if self.last_submitted.is_some() || self.searcher.result.load().is_none() {
                self.searcher.result.store(Some(Arc::new(Response::default())));
                self.last_submitted = None;
            }
            return;
        }
        let idx_gen = self.engine.generation.load(Ordering::Relaxed);
        let req = self.effective_request();
        let key = (req.query.clone(), req.opts, req.sort);
        let changed = self.last_submitted.as_ref() != Some(&key);
        let index_changed = idx_gen != self.submitted_index_gen;
        if !changed && !index_changed {
            return;
        }
        // 索引の更新に伴う再検索は 2 秒に 1 回まで（入力の変更は即時）。結果の入れ替わりで操作を邪魔しない。
        // 結果が巨大（100 万件超）なら並べ替えに時間がかかり、その読み取りの間 USN 反映の書き込みが待つので間隔を伸ばす
        let big = self.searcher.result.load_full().map(|r| r.total > 1_000_000).unwrap_or(false);
        let interval = if big { Duration::from_secs(10) } else { Duration::from_secs(2) };
        if !changed && self.last_submit_at.elapsed() < interval {
            return;
        }
        if changed {
            self.needles = highlight_needles(&self.query);
        }
        self.searcher.submit(req);
        self.last_submitted = Some(key);
        self.last_submit_at = Instant::now();
        self.submitted_index_gen = idx_gen;
    }

    fn save_settings(&mut self) {
        self.settings.set("match_case", self.opts.match_case as u8);
        self.settings.set("match_path", self.opts.match_path as u8);
        self.settings.set("whole_word", self.opts.whole_word as u8);
        self.settings.set("regex", self.opts.regex as u8);
        self.settings.set("match_diacritics", self.opts.match_diacritics as u8);
        self.settings.set(
            "sort",
            match self.sort.key {
                SortKey::Name => "name",
                SortKey::Path => "path",
                SortKey::Size => "size",
                SortKey::DateModified => "dm",
                SortKey::DateCreated => "dc",
                SortKey::Extension => "ext",
                SortKey::RunCount => "runcount",
                SortKey::DateAccessed => "da",
                SortKey::DateRun => "daterun",
            },
        );
        self.settings.set("sort_ascending", self.sort.ascending as u8);
        self.settings.set("columns", self.columns.iter().map(|c| c.key()).collect::<Vec<_>>().join(","));
        self.settings.save(&self.settings_path);
    }

    /// 結果が差し替わったら選択行を引き直す。
    fn sync_selection(&mut self, res: &Option<Arc<Response>>) {
        let same = match (&self.seen_result, res) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        self.seen_result = res.clone();
        self.selected_row = match (&self.selected, res) {
            (Some(sel), Some(r)) => r.hits.iter().position(|h| h == sel),
            _ => None,
        };
        if self.selected_row.is_none() {
            self.selected = None;
        }
    }

    fn select_row(&mut self, res: &Response, row: usize) {
        if let Some(h) = res.hits.get(row) {
            self.selected = Some(*h);
            self.selected_row = Some(row);
        }
    }

    fn hit_path(&self, hit: Hit) -> Option<(String, bool)> {
        if let Some(d) = self.row_cache.get(&hit) {
            let full = if d.path.is_empty() { d.name.clone() } else { format!("{}\\{}", d.path, d.name) };
            return Some((full, d.is_dir));
        }
        let vols = self.engine.index.snapshot();
        let g = vols.get(hit.vol as usize)?.try_read_for(Duration::from_millis(50))?;
        if hit.id as usize >= g.len() {
            return None;
        }
        Some((g.full_path(hit.id), g.is_dir(hit.id)))
    }

    fn open_selected(&mut self, open_path: bool) {
        let Some(hit) = self.selected else { return };
        let Some((path, is_dir)) = self.hit_path(hit) else { return };
        if open_path {
            shell::open_path(&path);
        } else if self.mode == Mode::DiskUsage && is_dir {
            self.du_path = Some(path);
            self.selected = None;
            self.selected_row = None;
        } else {
            self.run_history.record(&path);
            accio_query::set_run_history(self.run_history.snapshot());
            shell::open(&path);
        }
    }

    /// 結果に含まれるエントリの実行回数（履歴が空なら空）。
    /// 結果に含まれるエントリの (実行回数, 最終実行日時)。
    fn run_info_for(&self, res: &Response, vols: &[accio_index::SharedVolume]) -> std::collections::HashMap<Hit, (u32, i64)> {
        let mut out = std::collections::HashMap::new();
        if self.run_history.is_empty() {
            return out;
        }
        for h in res.hits.iter().take(200_000) {
            let Some(v) = vols.get(h.vol as usize) else { continue };
            let g = v.read_recursive();
            if h.id as usize >= g.len() {
                continue;
            }
            if let Some(info) = self.run_history.info(&g.full_path(h.id)) {
                out.insert(*h, info);
            }
        }
        out
    }

    fn snapshot_tab(&self) -> TabState {
        TabState {
            query: self.query.clone(),
            opts: self.opts,
            sort: self.sort,
            filter: self.filter,
            du: self.mode == Mode::DiskUsage,
            du_path: self.du_path.clone(),
            selected: self.selected,
        }
    }

    fn restore_tab(&mut self, t: &TabState) {
        self.query = t.query.clone();
        self.opts = t.opts;
        self.sort = t.sort;
        self.filter = t.filter;
        self.mode = if t.du { Mode::DiskUsage } else { Mode::Search };
        self.du_path = t.du_path.clone();
        self.selected = t.selected;
        self.selected_row = None;
        self.multi.clear();
        self.seen_result = None;
        self.last_submitted = None;
    }

    fn switch_tab(&mut self, to: usize) {
        if to >= self.tabs.len() || to == self.active_tab {
            return;
        }
        self.tabs[self.active_tab] = self.snapshot_tab();
        self.active_tab = to;
        let t = self.tabs[to].clone();
        self.restore_tab(&t);
    }

    fn new_tab(&mut self) {
        self.tabs[self.active_tab] = self.snapshot_tab();
        self.tabs.push(TabState { opts: self.opts, sort: self.sort, ..Default::default() });
        let to = self.tabs.len() - 1;
        self.active_tab = to;
        let t = self.tabs[to].clone();
        self.restore_tab(&t);
    }

    fn close_tab(&mut self) {
        if self.tabs.len() <= 1 {
            return;
        }
        self.tabs.remove(self.active_tab);
        let to = self.active_tab.min(self.tabs.len() - 1);
        self.active_tab = to;
        let t = self.tabs[to].clone();
        self.restore_tab(&t);
    }

    /// ACCIO_UI_SCRIPT の 1 ステップを実行する。`wait:N`（フレーム）, `text:<s>`, `key:<name>[+ctrl][+shift]`,
    /// `click:<row>`, `dump:<file>`（検索語/件数/選択行/選択パスを書き出す）, `exit`。
    fn run_ui_script(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        if self.ui_script.is_empty() {
            return;
        }
        // スクリプト実行中は OS のフォーカス状態に左右されないようにする（egui はフォーカス喪失で入力を落とす）
        raw.focused = true;
        raw.events.retain(|e| !matches!(e, egui::Event::WindowFocused(false)));
        // shot:<file> の結果（フレームの画像）を PNG に保存する
        for e in &raw.events {
            if let egui::Event::Screenshot { image, user_data, .. } = e {
                if let Some(path) = user_data.data.as_ref().and_then(|d| d.downcast_ref::<String>()) {
                    let [w, h] = image.size;
                    let rgba: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
                    match image::RgbaImage::from_raw(w as u32, h as u32, rgba) {
                        Some(img) => {
                            let _ = img.save(path);
                        }
                        None => log::warn!("ui script: shot の画像を組めない"),
                    }
                }
            }
        }
        if self.ui_script_wait > 0 {
            self.ui_script_wait -= 1;
            ctx.request_repaint();
            return;
        }
        let step = self.ui_script.remove(0);
        log::info!("ui script: {step}");
        ctx.request_repaint();
        let (cmd, arg) = step.split_once(':').unwrap_or((step.as_str(), ""));
        match cmd {
            "wait" => self.ui_script_wait = arg.parse().unwrap_or(10),
            "text" => {
                raw.events.push(egui::Event::Text(arg.to_string()));
                self.ui_script_wait = 5;
            }
            "key" => {
                let mut mods = Modifiers::NONE;
                let mut name = arg.to_string();
                for m in ["+ctrl", "+shift", "+alt"] {
                    if name.contains(m) {
                        name = name.replace(m, "");
                        match m {
                            "+ctrl" => mods.ctrl = true,
                            "+shift" => mods.shift = true,
                            _ => mods.alt = true,
                        }
                    }
                }
                if let Some(key) = Key::from_name(&name) {
                    raw.modifiers = mods;
                    raw.events.push(egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: mods });
                    raw.events.push(egui::Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers: mods });
                }
                self.ui_script_wait = 5;
            }
            "click" => {
                let row: usize = arg.parse().unwrap_or(0);
                if let Some((_, rect)) = self.row_rects.iter().find(|(i, _)| *i == row) {
                    let pos = rect.center();
                    // フレームを分けて 移動 → 押下 → 離す（実際のマウスと同じ順序）
                    raw.events.push(egui::Event::PointerMoved(pos));
                    self.ui_script.insert(0, format!("release:{}:{}", pos.x, pos.y));
                    self.ui_script.insert(0, format!("press:{}:{}", pos.x, pos.y));
                } else {
                    log::warn!("ui script: row {row} not visible ({} rows)", self.row_rects.len());
                }
                self.ui_script_wait = 3;
            }
            "shot" => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(arg.to_string())));
                self.ui_script_wait = 2;
            }
            "rclick" => {
                let row: usize = arg.parse().unwrap_or(0);
                if let Some((_, rect)) = self.row_rects.iter().find(|(i, _)| *i == row) {
                    let pos = rect.center();
                    raw.events.push(egui::Event::PointerMoved(pos));
                    self.ui_script.insert(0, format!("rrelease:{}:{}", pos.x, pos.y));
                    self.ui_script.insert(0, format!("rpress:{}:{}", pos.x, pos.y));
                }
                self.ui_script_wait = 3;
            }
            "rpress" | "rrelease" => {
                let mut it = arg.split(':');
                let x: f32 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
                let y: f32 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
                let pos = egui::pos2(x, y);
                raw.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed: cmd == "rpress", modifiers: Modifiers::NONE });
                self.ui_script_wait = 3;
            }
            "press" => {
                let mut it = arg.split(':');
                let x: f32 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
                let y: f32 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
                let pos = egui::pos2(x, y);
                raw.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
                self.ui_script_wait = 3;
            }
            "release" => {
                let mut it = arg.split(':');
                let x: f32 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
                let y: f32 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
                let pos = egui::pos2(x, y);
                raw.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
                self.ui_script_wait = 5;
            }
            "dump" => {
                let res = self.searcher.result.load_full();
                let sel = self.selected.and_then(|h| self.hit_path(h)).map(|(p, _)| p).unwrap_or_default();
                let text = format!(
                    "query={}\nhits={}\nselected_row={:?}\nselected={}\nmulti={}\ntabs={}\nrows={}\ncopied={}\n",
                    self.query,
                    res.as_ref().map(|r| r.hits.len()).unwrap_or(0),
                    self.selected_row,
                    sel,
                    self.multi.len(),
                    self.tabs.len(),
                    self.row_rects.len(),
                    self.last_copied
                );
                let focus = ctx.memory(|m| m.focused());
                let popup = ctx.is_popup_open();
                let text = format!("{text}focus={focus:?} search_id={:?} popup={popup}\n", self.search_id);
                let _ = std::fs::write(arg, text);
            }
            "exit" => {
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            _ => {}
        }
    }

    fn save_bookmarks(&mut self) {
        for i in 0..64 {
            let k = format!("bookmark.{i}");
            match self.bookmarks.get(i) {
                Some((n, q)) => self.settings.set(&k, format!("{n}|{q}")),
                None => {
                    if self.settings.get(&k).is_some() {
                        self.settings.set(&k, "");
                    }
                }
            }
        }
        self.save_settings();
    }

    /// 選択中（複数選択があればそれ、なければフォーカス行）のフルパス一覧。
    fn selected_paths(&self) -> Vec<String> {
        let hits: Vec<Hit> = if self.multi.len() > 1 { self.multi.clone() } else { self.selected.into_iter().collect() };
        let vols = self.engine.index.snapshot();
        hits.iter()
            .filter_map(|h| {
                let g = vols.get(h.vol as usize)?.read_recursive();
                ((h.id as usize) < g.len()).then(|| g.full_path(h.id))
            })
            .collect()
    }

    fn notify(&mut self, msg: impl Into<String>) {
        self.notice = Some((msg.into(), Instant::now()));
    }

    /// ファイルリスト（EFU / テキスト）を開く。ダイアログと読み込みは別スレッドで、結果は update() で受ける。
    fn open_file_list_dialog(&mut self) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.file_list_rx = Some(rx);
        std::thread::spawn(move || {
            let Some(path) = rfd::FileDialog::new()
                .add_filter(crate::i18n::tr("ファイルリスト (*.efu, *.txt, *.csv)"), &["efu", "txt", "csv"])
                .add_filter(crate::i18n::tr("すべてのファイル"), &["*"])
                .pick_file()
            else {
                return;
            };
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let res = match std::fs::read(&path) {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    let vols = accio_index::filelist::build(&accio_index::filelist::parse(&text));
                    Ok((name, vols))
                }
                Err(e) => Err(format!("{}: {e}", path.display())),
            };
            let _ = tx.send(res);
        });
    }

    fn close_file_list(&mut self) {
        if self.file_list.take().is_some() {
            self.engine.index.set_overlay(None);
            self.engine.generation.fetch_add(1, Ordering::Relaxed);
            self.last_submitted = None;
            self.notify(crate::i18n::tr("ファイルリストを閉じました"));
        }
    }

    fn poll_file_list(&mut self) {
        let Some(rx) = self.file_list_rx.as_ref() else { return };
        match rx.try_recv() {
            Ok(Ok((name, vols))) => {
                let n: usize = vols.iter().map(|v| v.live_count()).sum();
                self.engine.index.set_overlay(Some(vols.into_iter().map(|v| Arc::new(parking_lot::RwLock::new(v))).collect()));
                self.engine.generation.fetch_add(1, Ordering::Relaxed);
                self.last_submitted = None;
                self.file_list = Some(name.clone());
                self.file_list_rx = None;
                self.notify(crate::i18n::trf("ファイルリストを開きました: {}（{} 件）", &[&name, &n]));
            }
            Ok(Err(e)) => {
                self.file_list_rx = None;
                self.notify(crate::i18n::trf("ファイルリストを開けません: {}", &[&e]));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => self.file_list_rx = None,
        }
    }

    /// 結果全体を CSV / EFU / TXT に書き出す（ダイアログと書き込みは別スレッド）。
    fn export(&mut self, res: Arc<Response>) {
        let vols = self.engine.index.snapshot();
        std::thread::spawn(move || {
            let Some(path) = rfd::FileDialog::new()
                .add_filter(crate::i18n::tr("Everything ファイルリスト (*.efu)"), &["efu"])
                .add_filter("CSV (*.csv)", &["csv"])
                .add_filter(crate::i18n::tr("テキスト (*.txt)"), &["txt"])
                .set_file_name("accio-results.efu")
                .save_file()
            else {
                return;
            };
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            let mut out = String::new();
            let csvq = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
            match ext.as_str() {
                "efu" => out.push_str("Filename,Size,Date Modified,Date Created,Attributes\r\n"),
                "csv" => out.push_str("Name,Path,Size,Date Modified,Date Created,Attributes\r\n"),
                _ => {}
            }
            for h in &res.hits {
                let Some(v) = vols.get(h.vol as usize) else { continue };
                let g = v.read_recursive();
                if h.id as usize >= g.len() {
                    continue;
                }
                let full = g.full_path(h.id);
                match ext.as_str() {
                    "efu" => out.push_str(&format!("{},{},{},{},{}\r\n", csvq(&full), g.size[h.id as usize], g.mtime(h.id), g.ctime(h.id), g.attrs[h.id as usize])),
                    "csv" => out.push_str(&format!(
                        "{},{},{},{},{},{}\r\n",
                        csvq(g.name(h.id)),
                        csvq(&g.parent_path(h.id)),
                        g.size[h.id as usize],
                        format_filetime(g.mtime(h.id)),
                        format_filetime(g.ctime(h.id)),
                        attrib_string(g.attrs[h.id as usize])
                    )),
                    _ => {
                        out.push_str(&full);
                        out.push_str("\r\n");
                    }
                }
            }
            if let Err(e) = std::fs::write(&path, out) {
                log::error!("エクスポート失敗 {}: {e}", path.display());
            }
        });
    }

    fn du_go_up(&mut self) {
        self.du_path = self.du_path.take().and_then(|p| {
            let p = p.trim_end_matches('\\');
            let (parent, _) = p.rsplit_once('\\')?;
            Some(if parent.ends_with(':') { format!("{parent}\\") } else { parent.to_string() })
        });
        self.selected = None;
        self.selected_row = None;
    }
}

fn fmt_size(n: u64) -> String {
    const K: f64 = 1024.0;
    let f = n as f64;
    if f < K {
        format!("{n} B")
    } else if f < K * K {
        format!("{:.1} KB", f / K)
    } else if f < K * K * K {
        format!("{:.1} MB", f / K / K)
    } else if f < K * K * K * K {
        format!("{:.2} GB", f / K / K / K)
    } else {
        format!("{:.2} TB", f / K / K / K / K)
    }
}

fn fmt_count(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn ui_mods(r: &egui::Response) -> (bool, bool) {
    r.ctx.input(|i| (i.modifiers.ctrl, i.modifiers.shift))
}

fn sort_label(k: SortKey) -> &'static str {
    match k {
        SortKey::Name => crate::i18n::tr("名前"),
        SortKey::Path => crate::i18n::tr("パス"),
        SortKey::Size => crate::i18n::tr("サイズ"),
        SortKey::DateModified => crate::i18n::tr("更新日時"),
        SortKey::DateCreated => crate::i18n::tr("作成日時"),
        SortKey::Extension => crate::i18n::tr("拡張子"),
        SortKey::RunCount => crate::i18n::tr("実行回数"),
        SortKey::DateAccessed => crate::i18n::tr("アクセス日時"),
        SortKey::DateRun => crate::i18n::tr("実行日時"),
    }
}

impl App {
    /// 表示/非表示は Win32 で直接行う（隠れている間は egui のフレームが回らないため）。
    fn show_window(&mut self, ctx: &egui::Context) {
        self.hidden = false;
        self.shown_at = Instant::now();
        crate::winshow::show();
        if !self.pinned {
            self.center_float(ctx);
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }
    fn hide_window(&mut self, _ctx: &egui::Context) {
        self.hidden = true;
        crate::winshow::hide();
    }

    fn popup_or_modal_open(&self, ctx: &egui::Context) -> bool {
        egui::Popup::is_any_open(ctx) || self.renaming.is_some() || self.confirm_delete.is_some() || self.show_settings || self.bulk.is_some() || self.show_bookmarks
    }

    /// 浮動パネルの高さ（内容に合わせて伸縮する）
    fn float_height(&self, ctx: &egui::Context, n_hits: usize) -> f32 {
        // メニューが開いている間は、その分の余白（透明）を下に取る。ウィンドウの外には描けない
        let popup_room = if egui::Popup::is_any_open(ctx) { FLOAT_MARGIN * 2.0 + 620.0 } else { 0.0 };
        if self.float_content_h > 0.0 {
            return (FLOAT_MARGIN * 2.0 + self.float_content_h).max(popup_room);
        }
        let rows = n_hits.min(FLOAT_ROWS_MAX);
        let list = if rows > 0 { 1.0 + rows as f32 * ROW_H + 8.0 } else { 0.0 };
        let notice = if self.notice.as_ref().map(|(_, t)| t.elapsed() < Duration::from_secs(6)).unwrap_or(false) { 22.0 } else { 0.0 };
        let du = if self.mode == Mode::DiskUsage { 24.0 } else { 0.0 };
        (FLOAT_MARGIN * 2.0 + BAR_H + list + notice + du).max(popup_room)
    }

    /// 浮動パネルを画面の上寄り中央に置く
    fn center_float(&self, ctx: &egui::Context) {
        if let Some(m) = ctx.input(|i| i.viewport().monitor_size) {
            let x = ((m.x - FLOAT_W) / 2.0).max(0.0);
            let y = (m.y * 0.18).max(0.0);
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(x, y)));
        }
    }

    /// ウィンドウ形態（ピン留め = 通常ウィンドウ / 浮動パネル）を OS のウィンドウに適用する
    fn apply_window_mode(&mut self, ctx: &egui::Context, n_hits: usize) {
        use egui::ViewportCommand as C;
        let changed = self.mode_applied != Some(self.pinned);
        if self.pinned {
            if changed {
                self.mode_applied = Some(true);
                ctx.send_viewport_cmd(C::Decorations(true));
                ctx.send_viewport_cmd(C::WindowLevel(egui::viewport::WindowLevel::Normal));
                ctx.send_viewport_cmd(C::Resizable(true));
                ctx.send_viewport_cmd(C::MinInnerSize(egui::vec2(600.0, 300.0)));
                let geom = |k: &str, d: f32| self.settings.get(k).and_then(|v| v.parse::<f32>().ok()).unwrap_or(d);
                ctx.send_viewport_cmd(C::InnerSize(egui::vec2(geom("window_w", 1200.0).max(600.0), geom("window_h", 760.0).max(300.0))));
                if let (Some(x), Some(y)) = (self.settings.get("window_x").and_then(|v| v.parse::<f32>().ok()), self.settings.get("window_y").and_then(|v| v.parse::<f32>().ok())) {
                    ctx.send_viewport_cmd(C::OuterPosition(egui::pos2(x, y)));
                }
            }
            return;
        }
        let h = self.float_height(ctx, n_hits);
        if changed {
            self.mode_applied = Some(false);
            ctx.send_viewport_cmd(C::Decorations(false));
            ctx.send_viewport_cmd(C::WindowLevel(egui::viewport::WindowLevel::AlwaysOnTop));
            ctx.send_viewport_cmd(C::Resizable(false));
            ctx.send_viewport_cmd(C::MinInnerSize(egui::vec2(FLOAT_W, FLOAT_MARGIN * 2.0 + BAR_H)));
        }
        if changed || (h - self.float_h).abs() > 0.5 {
            self.float_h = h;
            ctx.send_viewport_cmd(C::InnerSize(egui::vec2(FLOAT_W, h)));
        }
        if changed {
            self.center_float(ctx);
        }
    }

    /// 通常ウィンドウの位置と大きさを設定に覚える（ピン留め中だけ更新される last_rect から）
    fn remember_geometry(&mut self) {
        if let Some((x, y, w, h)) = self.last_rect {
            if !self.last_maximized && w > 100.0 && h > 100.0 {
                self.settings.set("window_x", x.round());
                self.settings.set("window_y", y.round());
                self.settings.set("window_w", w.round());
                self.settings.set("window_h", h.round());
            }
            self.settings.set("window_maximized", self.last_maximized as u8);
        }
    }

    fn set_pinned(&mut self, ctx: &egui::Context, pinned: bool) {
        if self.pinned == pinned {
            return;
        }
        if self.pinned {
            self.remember_geometry();
        }
        self.pinned = pinned;
        self.settings.set("pinned", pinned as u8);
        self.save_settings();
        self.shown_at = Instant::now();
        ctx.request_repaint();
    }

    /// 検索バー: ハンバーガー、大きめの検索窓、件数、ピン留め
    fn search_bar(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, result: &Option<Arc<Response>>, act: &mut BarActions) {
        let popup_open_now = self.popup_or_modal_open(ctx);
        ui.horizontal(|ui| {
            ui.set_min_height(BAR_H);
            ui.add_space(6.0);
            let menu_label = RichText::new("≡").size(24.0);
            ui.menu_button(menu_label, |ui| self.hamburger(ctx, ui, act)).response.on_hover_text(crate::i18n::tr("検索の設定と表示"));
            ui.add_space(2.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(10.0);
                let pin = ui.selectable_label(self.pinned, crate::i18n::tr("ピン留め")).on_hover_text(crate::i18n::tr("ピン留めすると通常のウィンドウになり、他を操作しても閉じない (Ctrl+Shift+P)"));
                if pin.clicked() {
                    act.toggle_pin = true;
                }
                if let Some(r) = result.as_ref() {
                    if !self.nothing_to_search() {
                        let mut s = crate::i18n::trf("{} 件", &[&(fmt_count(r.total))]);
                        if !r.sorted {
                            s.push_str(crate::i18n::tr("  [件数が多いため未ソート]"));
                        }
                        ui.label(RichText::new(s).size(12.0).color(Color32::from_gray(140)));
                    }
                }
                if self.searcher.busy.load(Ordering::Relaxed) != 0 {
                    ui.spinner();
                }
                let hint = match self.mode {
                    Mode::Search => crate::i18n::tr("検索"),
                    Mode::DiskUsage => crate::i18n::tr("このフォルダ内を絞り込み"),
                };
                let edit = egui::TextEdit::singleline(&mut self.query)
                    .id(self.search_id)
                    .hint_text(RichText::new(hint).size(22.0).color(Color32::from_gray(150)))
                    .font(egui::FontId::proportional(22.0))
                    .vertical_align(egui::Align::Center)
                    .desired_width(f32::INFINITY)
                    .frame(false)
                    .return_key(None);
                // フォーカスは描く前に戻す（描いたあとに戻すと 1 フレームだけカーソル無しで描かれ、ちらつく）
                if !popup_open_now && ctx.memory(|m| m.focused()) != Some(self.search_id) {
                    ctx.memory_mut(|m| m.request_focus(self.search_id));
                }
                ui.add_sized(egui::vec2(ui.available_width(), BAR_H - 8.0), edit);
            });
        });
    }

    /// ハンバーガーの中身: 検索オプション、ドライブ、並べ替え、フィルタ、表示、各種ウィンドウ
    fn hamburger(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, act: &mut BarActions) {
        let t = crate::i18n::tr;
        ui.set_min_width(260.0);
        ui.toggle_value(&mut self.opts.match_case, t("大文字と小文字を区別 (Ctrl+I)"));
        ui.toggle_value(&mut self.opts.match_path, t("パス全体に一致 (Ctrl+U)"));
        ui.toggle_value(&mut self.opts.whole_word, t("単語単位で一致 (Ctrl+B)"));
        ui.toggle_value(&mut self.opts.regex, t("正規表現 (Ctrl+R)"));
        ui.toggle_value(&mut self.opts.match_diacritics, t("ダイアクリティカルマークを区別"));
        ui.separator();
        // ドライブ
        let roots: Vec<String> = self.engine.status.lock().volumes.iter().map(|v| v.root.clone()).collect();
        let drives_label = if self.drives_off.is_empty() {
            t("ドライブ: すべて").to_string()
        } else {
            crate::i18n::trf("ドライブ: {}", &[&(roots.iter().filter(|r| !self.drives_off.contains(*r)).cloned().collect::<Vec<_>>().join(" "))])
        };
        let mut drives_changed = false;
        ui.menu_button(drives_label, |ui| {
            for r in &roots {
                let mut on = !self.drives_off.contains(r);
                if ui.checkbox(&mut on, r).changed() {
                    if on {
                        self.drives_off.remove(r);
                    } else {
                        self.drives_off.insert(r.clone());
                    }
                    drives_changed = true;
                }
            }
            ui.separator();
            if ui.button(t("すべて")).clicked() {
                self.drives_off.clear();
                drives_changed = true;
            }
        });
        if drives_changed {
            self.settings.set("drives_off", self.drives_off.iter().cloned().collect::<Vec<_>>().join(";"));
            self.last_submitted = None;
        }
        // 並べ替え
        let sort_text = crate::i18n::trf("並べ替え: {} {}", &[&sort_label(self.sort.key), &(if self.sort.ascending { t("昇順") } else { t("降順") })]);
        ui.menu_button(sort_text, |ui| {
            for k in [SortKey::Name, SortKey::Path, SortKey::Size, SortKey::DateModified, SortKey::DateCreated, SortKey::DateAccessed, SortKey::Extension, SortKey::RunCount, SortKey::DateRun] {
                ui.selectable_value(&mut self.sort.key, k, sort_label(k));
            }
            ui.separator();
            ui.selectable_value(&mut self.sort.ascending, true, t("昇順"));
            ui.selectable_value(&mut self.sort.ascending, false, t("降順"));
        });
        // フィルタ
        let filter_label = if self.filter < FILTERS.len() {
            t(FILTERS[self.filter].0).to_string()
        } else {
            self.custom_filters.get(self.filter - FILTERS.len()).map(|(n, _, _)| n.clone()).unwrap_or_default()
        };
        ui.menu_button(crate::i18n::trf("フィルタ: {}", &[&filter_label]), |ui| {
            for (i, (n, _)) in FILTERS.iter().enumerate() {
                ui.selectable_value(&mut self.filter, i, t(n));
            }
            if !self.custom_filters.is_empty() {
                ui.separator();
            }
            for (i, (n, _, _)) in self.custom_filters.iter().enumerate() {
                ui.selectable_value(&mut self.filter, FILTERS.len() + i, n);
            }
        });
        ui.separator();
        // 表示
        let du = self.mode == Mode::DiskUsage;
        if ui.selectable_label(du, t("容量ビュー")).on_hover_text(t("容量を占有しているフォルダ/ファイルを一覧（フォルダを Enter/ダブルクリックで掘り下げ）")).clicked() {
            if du {
                self.mode = Mode::Search;
                self.du_path = None;
            } else {
                self.mode = Mode::DiskUsage;
                self.sort = Sort { key: SortKey::Size, ascending: false };
                self.query.clear();
            }
            self.selected = None;
            self.selected_row = None;
            ui.close();
        }
        if ui.button(t("最大ファイル")).on_hover_text(t("全ボリュームで最大のファイル")).clicked() {
            self.mode = Mode::Search;
            self.du_path = None;
            self.query = "file: size:>100mb".into();
            self.sort = Sort { key: SortKey::Size, ascending: false };
            ui.close();
        }
        let thumbs_on = self.thumb_size > 0;
        if ui.selectable_label(thumbs_on, t("サムネイル")).on_hover_text(t("Ctrl+Shift+1/2/3 でサイズ、Ctrl+Shift+6 で詳細表示")).clicked() {
            self.thumb_size = if thumbs_on { 0 } else { 160 };
            self.settings.set("thumb_size", self.thumb_size);
            act.need_pin |= self.thumb_size > 0;
            ui.close();
        }
        if ui.selectable_label(self.show_preview, t("プレビュー (Alt+P)")).clicked() {
            self.show_preview = !self.show_preview;
            self.settings.set("preview", self.show_preview as u8);
            act.need_pin |= self.show_preview;
            ui.close();
        }
        ui.separator();
        if ui.button(t("ブックマーク (Ctrl+D)")).clicked() {
            self.show_bookmarks = true;
            ui.close();
        }
        if ui.button(t("新しいタブ (Ctrl+T)")).clicked() {
            self.new_tab();
            ui.close();
        }
        if ui.button(t("ファイルリストを開く (Ctrl+O)")).clicked() {
            act.open_file_list = true;
            ui.close();
        }
        if let Some(name) = self.file_list.clone() {
            if ui.button(crate::i18n::trf("ファイルリストを閉じる: {} (Ctrl+Shift+O)", &[&name])).clicked() {
                act.close_file_list = true;
                ui.close();
            }
        }
        if ui.button(t("エクスポート (Ctrl+S)")).clicked() {
            act.export = true;
            ui.close();
        }
        ui.separator();
        if ui.selectable_label(self.pinned, t("ピン留め (Ctrl+Shift+P)")).clicked() {
            act.toggle_pin = true;
            ui.close();
        }
        if ui.button(t("設定 (Ctrl+P)")).clicked() {
            self.show_settings = true;
            ui.close();
        }
        if ui.button(t("終了 (Ctrl+Q)")).clicked() {
            self.allow_close = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// コンパクト表示（浮動パネル）: 名前と、その下にパス。右端にサイズと更新日時
    fn compact_list(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, res: &Arc<Response>, out: &mut RowActions) {
        let vols = self.engine.index.snapshot();
        let guards: Vec<Option<parking_lot::RwLockReadGuard<'_, accio_index::VolumeIndex>>> = vols.iter().map(try_read_vol).collect();
        let mut row_cache = std::mem::take(&mut self.row_cache);
        if row_cache.len() > 50_000 {
            row_cache.clear();
        }
        let selected_row = self.selected_row;
        let multi = self.multi.clone();
        let needles = self.needles.clone();
        let text_color = ui.visuals().text_color();
        let gray = Color32::from_gray(130);
        let highlight = self.colors.highlight;
        let name_font = egui::FontId::proportional(16.0);
        let sub_font = egui::FontId::proportional(12.0);
        let scroll_to = self.scroll_to.take();
        let path_mode = self.opts.match_path || self.query.contains("path:");
        let mut lock_missed = false;
        let mut rects: Vec<(usize, egui::Rect)> = Vec::new();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, ROW_H, res.hits.len(), |ui, range| {
            for i in range {
                let hit = res.hits[i];
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), egui::Sense::click_and_drag());
                if scroll_to == Some(i) {
                    ui.scroll_to_rect(rect, None);
                }
                let data = match guards.get(hit.vol as usize).and_then(|g| g.as_ref()) {
                    Some(g) => {
                        if hit.id as usize >= g.len() {
                            continue;
                        }
                        let d = Arc::new(RowData::read(g, hit.id));
                        row_cache.insert(hit, d.clone());
                        d
                    }
                    None => match row_cache.get(&hit) {
                        Some(d) => d.clone(),
                        None => {
                            lock_missed = true;
                            continue;
                        }
                    },
                };
                let p = ui.painter().clone();
                let sel = selected_row == Some(i) || multi.contains(&hit);
                let bg = rect.shrink2(egui::vec2(6.0, 1.0));
                if sel {
                    p.rect_filled(bg, 8.0, ui.visuals().selection.bg_fill);
                } else if resp.hovered() {
                    p.rect_filled(bg, 8.0, ui.visuals().widgets.hovered.bg_fill);
                }
                let icon_r = egui::Rect::from_center_size(egui::pos2(rect.left() + 30.0, rect.center().y), egui::vec2(20.0, 20.0));
                paint_kind_icon_at(&p, icon_r, data.is_dir);
                let size_text = if data.is_dir && data.size == 0 { String::new() } else { fmt_size(data.size) };
                let right_text = if size_text.is_empty() { format_filetime(data.mtime) } else { format!("{size_text}    {}", format_filetime(data.mtime)) };
                let right = p.layout_no_wrap(right_text, sub_font.clone(), gray);
                let right_w = right.size().x;
                p.galley(egui::pos2(rect.right() - 16.0 - right_w, rect.center().y - right.size().y / 2.0), right, gray);
                let text_x = rect.left() + 52.0;
                let max_w = (rect.right() - 16.0 - right_w - 12.0 - text_x).max(40.0);
                let mut job = highlighted(&data.name, &needles, text_color, name_font.clone(), highlight);
                job.wrap.max_width = max_w;
                job.wrap.max_rows = 1;
                job.wrap.break_anywhere = true;
                let name_g = p.layout_job(job);
                p.galley(egui::pos2(text_x, rect.top() + 5.0), name_g, text_color);
                let mut pj = if path_mode { highlighted(&data.path, &needles, gray, sub_font.clone(), highlight) } else { LayoutJob::simple_singleline(data.path.clone(), sub_font.clone(), gray) };
                pj.wrap.max_width = max_w;
                pj.wrap.max_rows = 1;
                pj.wrap.break_anywhere = true;
                let path_g = p.layout_job(pj);
                p.galley(egui::pos2(text_x, rect.top() + 27.0), path_g, gray);
                if resp.clicked() || resp.secondary_clicked() {
                    out.clicked = Some((i, ui_mods(&resp)));
                }
                if resp.double_clicked() {
                    out.double = Some(i);
                }
                if resp.drag_started_by(egui::PointerButton::Primary) {
                    if let Some(pos) = resp.interact_pointer_pos() {
                        out.drag_start = Some((i, pos));
                    }
                }
                let full = if data.path.is_empty() { data.name.clone() } else { format!("{}\\{}", data.path, data.name) };
                resp.context_menu(|ui| row_context_menu(ui, &full, &data.name, data.is_dir, multi.len(), hit, out));
                rects.push((i, rect));
            }
        });
        self.row_rects = rects;
        self.row_cache = row_cache;
        if lock_missed {
            ctx.request_repaint_after(Duration::from_millis(30));
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if !self.theme_applied {
            self.theme_applied = true;
            match self.theme {
                1 => ctx.set_theme(egui::Theme::Light),
                2 => ctx.set_theme(egui::Theme::Dark),
                _ => ctx.set_theme(egui::ThemePreference::System),
            }
            ctx.set_zoom_factor(self.zoom as f32 / 100.0);
            self.colors.apply(ctx);
        }
        if self.hwnd == 0 {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(h) = frame.window_handle() {
                if let RawWindowHandle::Win32(w) = h.as_raw() {
                    self.hwnd = w.hwnd.get();
                    crate::winshow::set_main_hwnd(self.hwnd);
                }
            }
        }
        if self.hwnd != 0 && !self.startup_hide_pending {
            // 外部（トレイ/ホットキー/IPC）が直接表示状態を変えているので実状態に合わせる
            let vis = crate::winshow::is_visible();
            if vis && self.hidden {
                self.shown_at = Instant::now();
                if !self.pinned {
                    self.center_float(ctx);
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
            self.hidden = !vis;
        }
        self.poll_file_list();
        if self.startup_hide_pending && self.hwnd != 0 {
            // eframe は最初の描画の後にウィンドウを表示するので、数フレーム待ってから改めて隠す
            self.startup_frames += 1;
            ctx.request_repaint();
            if self.startup_frames >= 3 {
                self.startup_hide_pending = false;
                if self.hidden {
                    self.hide_window(ctx);
                }
            }
        }
        // ---- 二重起動 / IPC からのコマンド ----
        let ext: Vec<crate::ipc::ExternalCommand> = std::mem::take(&mut *self.external.lock());
        for c in ext {
            use crate::ipc::ExternalCommand::*;
            match c {
                Search(q) => {
                    self.mode = Mode::Search;
                    self.query = q;
                    self.show_window(ctx);
                }
                Show => self.show_window(ctx),
                Toggle => {
                    // 送信側で既に切り替え済み。フォーカスだけ合わせる
                    if crate::winshow::is_visible() {
                        self.hidden = false;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    } else {
                        self.hidden = true;
                    }
                }
                Exit => {
                    self.allow_close = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                Reindex => {
                    self.engine.reindex_requested.fetch_add(1, Ordering::Relaxed);
                    self.notify(crate::i18n::tr("再索引を開始します"));
                }
                SaveDb => {
                    self.engine.save_requested.fetch_add(1, Ordering::Relaxed);
                    self.notify(crate::i18n::tr("索引を保存します"));
                }
                NewTab(q) => {
                    self.new_tab();
                    self.query = q;
                    self.show_window(ctx);
                }
            }
        }

        // ---- トレイ / ホットキー ----
        let mut tray_cmds = Vec::new();
        if let Some(t) = &self.tray {
            while let Ok(c) = t.rx.try_recv() {
                tray_cmds.push(c);
            }
        }
        for c in tray_cmds {
            use crate::tray::TrayCommand::*;
            match c {
                ShowWindow => self.show_window(ctx),
                ToggleWindow => {
                    // トレイ/ホットキー側で既に切り替え済み
                    if crate::winshow::is_visible() {
                        self.hidden = false;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    } else {
                        self.hidden = true;
                    }
                }
                NewSearch => {
                    self.query.clear();
                    self.show_window(ctx);
                }
                Exit => {
                    self.allow_close = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        // 閉じるボタンはバックグラウンド常駐（トレイへ）。終了はトレイメニューから。
        if ctx.input(|i| i.viewport().close_requested()) && self.run_in_background && !self.allow_close && self.tray.is_some() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.hide_window(ctx);
        }

        let mut result = self.searcher.result.load_full();
        if matches!(self.sort.key, SortKey::RunCount | SortKey::DateRun) {
            if let Some(r) = &result {
                if !Arc::ptr_eq(r, self.runcount_sorted.as_ref().map(|(src, _)| src).unwrap_or(r)) || self.runcount_sorted.is_none() {
                    let vols = self.engine.index.snapshot();
                    let info = self.run_info_for(r, &vols);
                    let mut hits = r.hits.clone();
                    let asc = self.sort.ascending;
                    let by_date = self.sort.key == SortKey::DateRun;
                    hits.sort_by_key(|h| {
                        let (c, t) = info.get(h).copied().unwrap_or((0, 0));
                        let k = if by_date { t.max(0) as u64 } else { c as u64 };
                        if asc { k } else { u64::MAX - k }
                    });
                    let sorted = Arc::new(Response { hits, total: r.total, files: r.files, folders: r.folders, warnings: r.warnings.clone(), elapsed_ms: r.elapsed_ms, sorted: r.sorted });
                    self.runcount_sorted = Some((r.clone(), sorted));
                }
                result = self.runcount_sorted.as_ref().map(|(_, s)| s.clone());
            }
        } else {
            self.runcount_sorted = None;
        }
        self.sync_selection(&result);
        let n_hits = result.as_ref().map(|r| r.hits.len()).unwrap_or(0);
        let page = 20i64;

        // ---- キーボード（検索窓にフォーカスがあっても効く操作）----
        let mut move_sel: Option<i64> = None;
        let mut jump: Option<usize> = None;
        let mut open_sel = false;
        let mut open_path = false;
        let mut copy_path = false;
        let mut open_file_list = false;
        let mut close_file_list = false;
        let mut clear = false;
        let mut delete_sel = false;
        let mut rename_sel = false;
        let mut copy_files = false;
        let mut props_sel = false;
        let mut admin_sel = false;
        let mut tab_cmd: Option<TabCmd> = None;
        let mut export = false;
        let mut extend = false;
        let mut toggle_pin = false;
        let popup_open = self.popup_or_modal_open(ctx);
        if !popup_open {
            ctx.input_mut(|i| {
                if i.consume_key(Modifiers::NONE, Key::Escape) || i.consume_key(Modifiers::ALT, Key::Home) {
                    clear = true;
                }
                if i.consume_key(Modifiers::NONE, Key::Delete) {
                    delete_sel = true;
                }
                if i.consume_key(Modifiers::NONE, Key::F2) {
                    rename_sel = true;
                }
                if i.consume_key(Modifiers::CTRL, Key::S) {
                    export = true;
                }
                if i.consume_key(Modifiers::CTRL | Modifiers::SHIFT, Key::O) {
                    close_file_list = true;
                } else if i.consume_key(Modifiers::CTRL, Key::O) {
                    open_file_list = true;
                }
                if i.consume_key(Modifiers::CTRL | Modifiers::SHIFT, Key::P) {
                    toggle_pin = true;
                }
                if i.consume_key(Modifiers::CTRL, Key::P) {
                    self.show_settings = true;
                }
                if i.consume_key(Modifiers::CTRL, Key::D) {
                    self.show_bookmarks = true;
                }
                if i.consume_key(Modifiers::ALT, Key::P) {
                    self.show_preview = !self.show_preview;
                    self.settings.set("preview", self.show_preview as u8);
                }
                if i.consume_key(Modifiers::ALT, Key::Enter) {
                    props_sel = true;
                }
                if i.consume_key(Modifiers::CTRL | Modifiers::SHIFT, Key::Enter) {
                    admin_sel = true;
                }
                if i.consume_key(Modifiers::NONE, Key::F5) {
                    self.last_submitted = None;
                    if self.engine.follow_indexer {
                        // インデクサに保存を頼むと、ファイル更新を検知して再読込される
                        shell::send_to_indexer(407);
                    }
                }
                if i.consume_key(Modifiers::CTRL, Key::Q) {
                    self.allow_close = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if i.consume_key(Modifiers::CTRL, Key::T) {
                    tab_cmd = Some(TabCmd::New);
                }
                for (k, size) in [(Key::Num1, 96u32), (Key::Num2, 160), (Key::Num3, 256), (Key::Num6, 0)] {
                    if i.consume_key(Modifiers::CTRL | Modifiers::SHIFT, k) {
                        self.thumb_size = size;
                        self.settings.set("thumb_size", size);
                    }
                }
                if i.consume_key(Modifiers::CTRL, Key::W) {
                    tab_cmd = Some(TabCmd::Close);
                }
                if i.consume_key(Modifiers::CTRL, Key::Tab) {
                    tab_cmd = Some(TabCmd::Next);
                }
                if i.consume_key(Modifiers::CTRL | Modifiers::SHIFT, Key::Tab) {
                    tab_cmd = Some(TabCmd::Prev);
                }
                for (n, k) in [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9].iter().enumerate() {
                    if i.consume_key(Modifiers::CTRL, *k) {
                        tab_cmd = Some(TabCmd::Go(n));
                    }
                }
                if i.consume_key(Modifiers::SHIFT, Key::ArrowDown) {
                    move_sel = Some(1);
                    extend = true;
                }
                if i.consume_key(Modifiers::SHIFT, Key::ArrowUp) {
                    move_sel = Some(-1);
                    extend = true;
                }
                if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                    move_sel = Some(1);
                }
                if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                    move_sel = Some(-1);
                }
                if i.consume_key(Modifiers::NONE, Key::PageDown) {
                    move_sel = Some(page);
                }
                if i.consume_key(Modifiers::NONE, Key::PageUp) {
                    move_sel = Some(-page);
                }
                if i.consume_key(Modifiers::CTRL, Key::Home) {
                    jump = Some(0);
                }
                if i.consume_key(Modifiers::CTRL, Key::End) {
                    jump = Some(n_hits.saturating_sub(1));
                }
                if i.consume_key(Modifiers::CTRL, Key::Enter) {
                    open_path = true;
                } else if i.consume_key(Modifiers::NONE, Key::Enter) {
                    open_sel = true;
                }
                if i.consume_key(Modifiers::CTRL | Modifiers::SHIFT, Key::C) {
                    copy_path = true;
                }
                if i.consume_key(Modifiers::CTRL, Key::C) {
                    copy_files = true;
                }
                if i.consume_key(Modifiers::CTRL, Key::I) {
                    self.opts.match_case = !self.opts.match_case;
                }
                if i.consume_key(Modifiers::CTRL, Key::U) {
                    self.opts.match_path = !self.opts.match_path;
                }
                if i.consume_key(Modifiers::CTRL, Key::B) {
                    self.opts.whole_word = !self.opts.whole_word;
                }
                if i.consume_key(Modifiers::CTRL, Key::R) {
                    self.opts.regex = !self.opts.regex;
                }
            });
        }
        if clear {
            if self.query.is_empty() {
                if self.mode == Mode::DiskUsage {
                    self.mode = Mode::Search;
                    self.du_path = None;
                } else if self.run_in_background && self.tray.is_some() {
                    self.hide_window(ctx);
                }
                self.selected = None;
                self.selected_row = None;
            }
            self.query.clear();
        }
        if let Some(r) = result.as_ref() {
            if let Some(d) = move_sel {
                if n_hits > 0 {
                    let cur = self.selected_row.map(|s| s as i64).unwrap_or(-1);
                    let next = if cur < 0 { 0 } else { (cur + d).clamp(0, n_hits as i64 - 1) as usize };
                    if extend {
                        if self.multi.is_empty() {
                            self.multi.extend(self.selected);
                        }
                        if let Some(h) = r.hits.get(next) {
                            if !self.multi.contains(h) {
                                self.multi.push(*h);
                            }
                        }
                        self.selected = r.hits.get(next).copied();
                        self.selected_row = Some(next);
                    } else {
                        self.multi.clear();
                        self.select_row(r, next);
                    }
                    self.scroll_to = Some(next);
                }
            }
            if let Some(j) = jump {
                if n_hits > 0 {
                    self.select_row(r, j);
                    self.scroll_to = Some(j);
                }
            }
        }
        if open_sel {
            self.open_selected(false);
        }
        if open_path {
            self.open_selected(true);
        }
        if copy_path {
            let paths = self.selected_paths();
            if !paths.is_empty() {
                self.last_copied = paths.join("|");
                ctx.copy_text(paths.join("\r\n"));
            }
        }
        if delete_sel {
            let paths = self.selected_paths();
            if !paths.is_empty() {
                self.confirm_delete = Some(paths);
            }
        }
        if let Some(c) = tab_cmd {
            match c {
                TabCmd::New => self.new_tab(),
                TabCmd::Close => self.close_tab(),
                TabCmd::Next => self.switch_tab((self.active_tab + 1) % self.tabs.len()),
                TabCmd::Prev => self.switch_tab((self.active_tab + self.tabs.len() - 1) % self.tabs.len()),
                TabCmd::Go(n) => self.switch_tab(n),
            }
        }
        if props_sel {
            if let Some(hit) = self.selected {
                if let Some((p, _)) = self.hit_path(hit) {
                    shell::properties(&p);
                }
            }
        }
        if admin_sel {
            if let Some(hit) = self.selected {
                if let Some((p, is_dir)) = self.hit_path(hit) {
                    if !is_dir {
                        shell::open_as_admin(&p);
                    }
                }
            }
        }
        if copy_files {
            let paths = self.selected_paths();
            if !paths.is_empty() {
                let n = paths.len();
                if shell::copy_files_to_clipboard(&paths) {
                    self.notify(crate::i18n::trf("{} 件のファイルをコピーしました（Explorer に貼り付け可）", &[&n]));
                }
            }
        }
        if rename_sel {
            if self.multi.len() > 1 {
                let paths = self.selected_paths();
                self.bulk = Some(BulkRename { paths, find: String::new(), replace: String::new(), regex: false, match_case: false, error: None });
            } else if let Some(hit) = self.selected {
                if let Some(vol) = self.engine.index.snapshot().get(hit.vol as usize) {
                    let name = vol.read_recursive().name(hit.id).to_string();
                    self.renaming = Some((hit, name));
                }
            }
        }

        // ---- ウィンドウ形態: ピン留め / 浮動 ----
        if toggle_pin {
            let p = !self.pinned;
            self.set_pinned(ctx, p);
        }
        if self.show_settings && !self.pinned {
            // 設定ウィンドウは大きいので通常ウィンドウで出す
            self.set_pinned(ctx, true);
        }
        // 浮動パネルは Spotlight と同じく、他の場所を触る（フォーカスを失う）と隠れる
        if !self.pinned && !self.hidden && self.hwnd != 0 && !self.startup_hide_pending {
            let focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);
            if !focused && self.shown_at.elapsed() > Duration::from_millis(500) && !self.popup_or_modal_open(ctx) && self.drag_candidate.is_none() {
                self.hide_window(ctx);
            }
        }
        self.apply_window_mode(ctx, n_hits);

        // ---- 検索バー（ピン留め: 上部パネル / 浮動: 角丸のカード）----
        let mut bar = BarActions::default();
        let mut rows_out = RowActions::default();
        let notice_text = self.notice.as_ref().filter(|(_, t)| t.elapsed() < Duration::from_secs(6)).map(|(m, _)| m.clone());
        if self.pinned {
            egui::TopBottomPanel::top("top").frame(egui::Frame::new().fill(ctx.style().visuals.panel_fill).inner_margin(egui::Margin::symmetric(6, 4))).show(ctx, |ui| {
                if self.tabs.len() > 1 {
                    let mut go: Option<usize> = None;
                    let mut close: Option<usize> = None;
                    ui.horizontal_wrapped(|ui| {
                        for (i, t) in self.tabs.iter().enumerate() {
                            let label = if i == self.active_tab {
                                if self.query.is_empty() { crate::i18n::tr("(空)").to_string() } else { self.query.chars().take(24).collect() }
                            } else if t.query.is_empty() {
                                crate::i18n::tr("(空)").to_string()
                            } else {
                                t.query.chars().take(24).collect()
                            };
                            let r = ui.selectable_label(i == self.active_tab, format!("{}: {label}", i + 1));
                            if r.clicked() {
                                go = Some(i);
                            }
                            if r.middle_clicked() || r.secondary_clicked() {
                                close = Some(i);
                            }
                            r.on_hover_text(crate::i18n::tr("右クリック / 中クリックで閉じる"));
                        }
                        if ui.small_button("+").on_hover_text(crate::i18n::tr("新しいタブ (Ctrl+T)")).clicked() {
                            go = Some(usize::MAX);
                        }
                    });
                    if let Some(i) = go {
                        if i == usize::MAX {
                            self.new_tab();
                        } else {
                            self.switch_tab(i);
                        }
                    }
                    if let Some(i) = close {
                        self.switch_tab(i);
                        self.close_tab();
                    }
                }
                self.search_bar(ctx, ui, &result, &mut bar);
                if self.mode == Mode::DiskUsage {
                    ui.horizontal(|ui| {
                        ui.add_space(12.0);
                        ui.label(RichText::new(crate::i18n::tr("容量")).strong());
                        if ui.button(crate::i18n::tr("↑ 親へ")).clicked() {
                            self.du_go_up();
                        }
                        ui.label(self.du_path.clone().unwrap_or_else(|| crate::i18n::tr("すべてのボリューム").into()));
                    });
                }
                if let Some(msg) = &notice_text {
                    ui.horizontal(|ui| {
                        ui.add_space(12.0);
                        ui.colored_label(Color32::from_rgb(200, 80, 0), msg);
                    });
                }
            });
        } else {
            let bg = ctx.style().visuals.window_fill;
            let stroke = ctx.style().visuals.window_stroke;
            egui::CentralPanel::default().frame(egui::Frame::NONE).show(ctx, |ui| {
                let inner = ui.max_rect().shrink(FLOAT_MARGIN);
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
                let card = egui::Frame::new()
                    .fill(bg)
                    .stroke(stroke)
                    .corner_radius(14.0)
                    .shadow(egui::epaint::Shadow { offset: [0, 6], blur: 22, spread: 0, color: Color32::from_black_alpha(80) })
                    .show(&mut child, |ui| {
                        ui.set_min_width(inner.width());
                        ui.set_max_width(inner.width());
                        self.search_bar(ctx, ui, &result, &mut bar);
                        if self.mode == Mode::DiskUsage {
                            ui.horizontal(|ui| {
                                ui.add_space(12.0);
                                ui.label(RichText::new(crate::i18n::tr("容量")).strong());
                                if ui.button(crate::i18n::tr("↑ 親へ")).clicked() {
                                    self.du_go_up();
                                }
                                ui.label(self.du_path.clone().unwrap_or_else(|| crate::i18n::tr("すべてのボリューム").into()));
                            });
                        }
                        if let Some(msg) = &notice_text {
                            ui.horizontal(|ui| {
                                ui.add_space(12.0);
                                ui.colored_label(Color32::from_rgb(200, 80, 0), msg);
                            });
                        }
                        let n = n_hits.min(FLOAT_ROWS_MAX);
                        if n > 0 {
                            ui.separator();
                            if let Some(res) = result.clone() {
                                ui.allocate_ui(egui::vec2(inner.width(), n as f32 * ROW_H + 12.0), |ui| self.compact_list(ctx, ui, &res, &mut rows_out));
                            }
                        }
                    });
                // カードの実測の高さに合わせて次のフレームでウィンドウを伸縮する
                let h = card.response.rect.height().ceil();
                if (h - self.float_content_h).abs() > 0.5 {
                    self.float_content_h = h;
                    ctx.request_repaint();
                }
            });
        }
        if bar.toggle_pin {
            let p = !self.pinned;
            self.set_pinned(ctx, p);
        }
        if bar.need_pin && !self.pinned {
            self.set_pinned(ctx, true);
        }
        let open_file_list = open_file_list || bar.open_file_list;
        let close_file_list = close_file_list || bar.close_file_list;
        let export = export || bar.export;

        if open_file_list {
            self.open_file_list_dialog();
        }
        if close_file_list {
            self.close_file_list();
        }
        if export {
            if let Some(r) = result.clone() {
                self.export(r);
            }
        }

        // ---- ブックマーク ----
        if self.show_bookmarks {
            let mut open = true;
            let mut pick: Option<String> = None;
            let mut remove: Option<usize> = None;
            let mut add = false;
            egui::Window::new(crate::i18n::tr("ブックマーク")).open(&mut open).resizable(true).show(ctx, |ui| {
                if ui.button(crate::i18n::trf("現在の検索を追加: {}", &[&(if self.query.is_empty() { crate::i18n::tr("(空)") } else { &self.query })])).clicked() && !self.query.is_empty() {
                    add = true;
                }
                ui.separator();
                egui::ScrollArea::vertical().max_height(400.0).show(ui, |ui| {
                    for (i, (n, q)) in self.bookmarks.iter().enumerate() {
                        ui.horizontal(|ui| {
                            if ui.button(n).on_hover_text(q).clicked() {
                                pick = Some(q.clone());
                            }
                            if ui.small_button("×").clicked() {
                                remove = Some(i);
                            }
                        });
                    }
                });
            });
            if add {
                self.bookmarks.push((self.query.clone(), self.query.clone()));
                self.save_bookmarks();
            }
            if let Some(i) = remove {
                self.bookmarks.remove(i);
                self.save_bookmarks();
            }
            if let Some(q) = pick {
                self.mode = Mode::Search;
                self.query = q;
                self.show_bookmarks = false;
            }
            if !open {
                self.show_bookmarks = false;
            }
        }


        // ---- 設定ウィンドウ ----
        if self.show_settings {
            let mut open = true;
            egui::Window::new(crate::i18n::tr("設定")).collapsible(false).resizable(false).open(&mut open).show(ctx, |ui| {
                let mut changed = false;
                changed |= ui.checkbox(&mut self.run_in_background, crate::i18n::tr("閉じる/Esc でトレイに常駐する")).changed();
                ui.separator();
                ui.label(RichText::new(crate::i18n::tr("昇格インデクサ（UI を非昇格で動かす）")).strong());
                let mut use_idx = self.settings.bool("use_indexer", false);
                if ui.checkbox(&mut use_idx, crate::i18n::tr("別プロセスのインデクサの索引を参照する（UI は昇格しない。再起動後に有効）")).changed() {
                    self.settings.set("use_indexer", use_idx as u8);
                    changed = true;
                }
                ui.horizontal(|ui| {
                    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
                    if ui.button(crate::i18n::tr("インデクサを今すぐ起動（UAC）")).clicked() {
                        shell::run_as_admin(&exe, "-indexer");
                    }
                    if ui.button(crate::i18n::tr("ログオン時に起動するタスクを登録（UAC）")).clicked() {
                        shell::run_as_admin("schtasks.exe", &format!("/Create /TN accio-indexer /SC ONLOGON /RL HIGHEST /F /TR \"\\\"{exe}\\\" -indexer\""));
                    }
                    if ui.button(crate::i18n::tr("タスクを削除（UAC）")).clicked() {
                        shell::run_as_admin("schtasks.exe", "/Delete /TN accio-indexer /F");
                    }
                });
                let mut rs = self.settings.bool("restore_session", true);
                if ui.checkbox(&mut rs, crate::i18n::tr("終了時のタブと検索語を次回復元する")).changed() {
                    self.settings.set("restore_session", rs as u8);
                    changed = true;
                }
                let mut ctx_on = shell::is_folder_context_menu();
                if ui.checkbox(&mut ctx_on, crate::i18n::tr("Explorer のフォルダ右クリックに「accio で検索」を追加する")).changed() && !shell::set_folder_context_menu(ctx_on) {
                    self.notify(crate::i18n::tr("右クリックメニューの設定を変更できませんでした"));
                }
                let mut url_on = shell::is_url_protocol();
                if ui.checkbox(&mut url_on, crate::i18n::tr("es: URL プロトコルを accio に関連付ける（Everything と同じ es:検索語）")).changed() && !shell::set_url_protocol(url_on) {
                    self.notify(crate::i18n::tr("URL プロトコルの設定を変更できませんでした"));
                }
                let mut sol = self.start_on_login;
                if ui.checkbox(&mut sol, crate::i18n::tr("ログオン時に自動起動する（-startup、トレイ常駐）")).changed() {
                    if shell::set_start_on_login(sol) {
                        self.start_on_login = sol;
                    } else {
                        self.notify(crate::i18n::tr("自動起動の設定を変更できませんでした"));
                    }
                }
                ui.horizontal(|ui| {
                    ui.label(crate::i18n::tr("言語 / Language:"));
                    let cur = self.settings.get("lang").unwrap_or("").to_string();
                    let label = match cur.as_str() { "ja" => "日本語", "en" => "English", _ => crate::i18n::tr("自動") };
                    egui::ComboBox::from_id_salt("lang").selected_text(label).show_ui(ui, |ui| {
                        for (v, l) in [("", crate::i18n::tr("自動")), ("ja", "日本語"), ("en", "English")] {
                            if ui.selectable_label(cur == v, l).clicked() && cur != v {
                                self.settings.set("lang", v);
                                changed = true;
                            }
                        }
                    });
                    ui.label(crate::i18n::tr("（再起動後に有効）"));
                });
                ui.horizontal(|ui| {
                    ui.label(crate::i18n::tr("ホットキー:"));
                    changed |= ui.text_edit_singleline(&mut self.settings_hotkey).changed();
                    ui.label(crate::i18n::tr("（例 ctrl+alt+a。空で無効。再起動後に有効）"));
                });
                let mut http_on = self.settings.bool("http_server", false);
                ui.horizontal(|ui| {
                    if ui.checkbox(&mut http_on, crate::i18n::tr("HTTP サーバー（再起動後に有効）")).changed() {
                        self.settings.set("http_server", http_on as u8);
                        changed = true;
                    }
                    ui.label(crate::i18n::tr("ポート:"));
                    changed |= ui.text_edit_singleline(&mut self.settings_http_port).changed();
                });
                ui.horizontal(|ui| {
                    ui.label(crate::i18n::tr("フォントファイル（空なら Segoe UI + Yu Gothic）:"));
                    if ui.text_edit_singleline(&mut self.settings_font).lost_focus() {
                        self.settings.set("font", self.settings_font.clone());
                        install_fonts(ctx, Some(&self.settings_font), self.font_size as f32);
                        changed = true;
                    }
                    ui.label(crate::i18n::tr("文字サイズ:"));
                    for z in [11u8, 12, 13, 14, 16, 18, 20] {
                        if ui.selectable_label(self.font_size == z, format!("{z}")).clicked() {
                            self.font_size = z;
                            self.settings.set("font_size", z);
                            install_fonts(ctx, Some(&self.settings_font), z as f32);
                            changed = true;
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(crate::i18n::tr("色（RRGGBB、空で既定）:"));
                    let mut color_changed = false;
                    for (i, (k, label)) in [("color_highlight", crate::i18n::tr("強調")), ("color_selection", crate::i18n::tr("選択")), ("color_text", crate::i18n::tr("文字")), ("color_background", crate::i18n::tr("背景"))].iter().enumerate() {
                        ui.label(*label);
                        if ui.add(egui::TextEdit::singleline(&mut self.settings_colors[i]).desired_width(64.0)).lost_focus() {
                            self.settings.set(k, self.settings_colors[i].trim().to_string());
                            color_changed = true;
                        }
                    }
                    if color_changed {
                        self.colors = Colors::load(&self.settings);
                        self.theme_applied = false;
                        changed = true;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(crate::i18n::tr("表示倍率:"));
                    for z in [80u8, 90, 100, 110, 125, 150, 175, 200] {
                        if ui.selectable_label(self.zoom == z, format!("{z}%")).clicked() {
                            self.zoom = z;
                            self.theme_applied = false;
                            self.settings.set("zoom", z);
                            changed = true;
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(crate::i18n::tr("配色:"));
                    for (v, l) in [(0u8, crate::i18n::tr("システム")), (1, crate::i18n::tr("ライト")), (2, crate::i18n::tr("ダーク"))] {
                        if ui.selectable_label(self.theme == v, l).clicked() {
                            self.theme = v;
                            self.theme_applied = false;
                            self.settings.set("theme", v);
                            changed = true;
                        }
                    }
                });
                ui.separator();
                ui.label(crate::i18n::tr("除外:"));
                changed |= ui.checkbox(&mut self.exclude_hidden, crate::i18n::tr("隠しファイル/フォルダを除外")).changed();
                changed |= ui.checkbox(&mut self.exclude_system, crate::i18n::tr("システムファイル/フォルダを除外")).changed();
                ui.horizontal(|ui| {
                    ui.label(crate::i18n::tr("除外ファイル（ワイルドカード、; 区切り）:"));
                    if ui.text_edit_singleline(&mut self.settings_exclude_files).changed() {
                        self.exclude_files = self.settings_exclude_files.split(';').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                        changed = true;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(crate::i18n::tr("フォルダ索引の定期再走査（分、0 で無効）:"));
                    if ui.text_edit_singleline(&mut self.settings_folder_rescan).changed() {
                        let mins: u64 = self.settings_folder_rescan.trim().parse().unwrap_or(0);
                        self.engine.folder_rescan_minutes.store(mins, Ordering::Relaxed);
                        self.settings.set("folder_rescan_minutes", mins);
                        changed = true;
                    }
                });
                ui.label(crate::i18n::tr("除外フォルダ（1 行 1 パス、配下を結果から除く）:"));
                if ui.add(egui::TextEdit::multiline(&mut self.settings_exclude_folders).desired_rows(2).desired_width(f32::INFINITY)).changed() {
                    self.exclude_folders = self.settings_exclude_folders.lines().map(|l| l.trim().trim_end_matches('\\').to_string()).filter(|l| !l.is_empty()).collect();
                    changed = true;
                }
                ui.label(crate::i18n::tr("対象フォルダの限定（1 行 1 パス、空なら全体。Everything の Include only）:"));
                if ui.add(egui::TextEdit::multiline(&mut self.settings_include_only).desired_rows(2).desired_width(f32::INFINITY)).changed() {
                    self.include_only = self.settings_include_only.lines().map(|l| l.trim().trim_end_matches('\\').to_string()).filter(|l| !l.is_empty()).collect();
                    changed = true;
                }
                ui.separator();
                ui.label(crate::i18n::tr("カスタムフィルタ（1 行 1 件、`名前|検索式|マクロ`。マクロは検索語の中で `マクロ:` として使える。例: ソース|ext:rs;c;cpp;h|src）:"));
                if ui.add(egui::TextEdit::multiline(&mut self.settings_custom_filters).desired_rows(2).desired_width(f32::INFINITY)).changed() {
                    self.custom_filters = self.settings_custom_filters.lines().filter_map(parse_custom_filter).collect();
                    accio_query::parser::set_custom_macros(custom_macros(&self.custom_filters));
                    if self.filter >= FILTERS.len() + self.custom_filters.len() {
                        self.filter = 0;
                    }
                    changed = true;
                }
                ui.separator();
                ui.label(crate::i18n::tr("フォルダ索引（NTFS 以外やネットワークのフォルダ。1 行 1 パス。再起動後に有効）:"));
                changed |= ui.add(egui::TextEdit::multiline(&mut self.settings_folders).desired_rows(2).desired_width(f32::INFINITY)).changed();
                ui.separator();
                ui.label(crate::i18n::tr("列:"));
                ui.horizontal_wrapped(|ui| {
                    for c in Col::ALL {
                        let mut on = self.columns.contains(&c);
                        if ui.checkbox(&mut on, c.label()).changed() {
                            if on {
                                self.columns.push(c);
                            } else if self.columns.len() > 1 {
                                self.columns.retain(|x| *x != c);
                            }
                            changed = true;
                        }
                    }
                });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button(crate::i18n::tr("索引を今すぐ保存")).clicked() {
                        self.engine.save_requested.fetch_add(1, Ordering::Relaxed);
                        self.notify(crate::i18n::tr("索引を保存します"));
                    }
                    if ui.button(crate::i18n::tr("索引を再構築（全ボリューム再スキャン）")).clicked() {
                        self.engine.reindex_requested.fetch_add(1, Ordering::Relaxed);
                        self.notify(crate::i18n::tr("再索引を開始します（スキャン中も検索できます）"));
                    }
                });
                ui.label(crate::i18n::trf("設定ファイル: {}", &[&(self.settings_path.display())]));
                if changed {
                    self.settings.set("run_in_background", self.run_in_background as u8);
                    self.settings.set("hotkey", self.settings_hotkey.clone());
                    self.settings.set("http_port", self.settings_http_port.clone());
                    self.settings.set("exclude_hidden", self.exclude_hidden as u8);
                    self.settings.set("exclude_system", self.exclude_system as u8);
                    self.settings.set("exclude_folders", self.exclude_folders.join(";"));
                    self.settings.set("exclude_files", self.exclude_files.join(";"));
                    self.settings.set("include_only", self.include_only.join(";"));
                    accio_query::set_search_prefix(crate::settings::exclude_prefix(self.exclude_hidden, self.exclude_system, &self.exclude_folders, &self.exclude_files, &self.include_only));
                    self.last_submitted = None;
                    for i in 0..64 {
                        let k = format!("filter.{i}");
                        match self.custom_filters.get(i) {
                            Some(f) => self.settings.set(&k, format_custom_filter(f)),
                            None => {
                                if self.settings.get(&k).is_some() {
                                    self.settings.set(&k, "");
                                }
                            }
                        }
                    }
                    let folders: Vec<String> = self.settings_folders.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
                    self.settings.set("folders", folders.join(";"));
                    self.save_settings();
                }
            });
            if !open {
                self.show_settings = false;
            }
        }

        // ---- 一括リネーム ----
        if let Some(mut b) = self.bulk.take() {
            let mut keep = true;
            let mut apply = false;
            egui::Window::new(crate::i18n::tr("一括リネーム")).collapsible(false).resizable(true).default_width(700.0).show(ctx, |ui| {
                ui.label(crate::i18n::trf("{} 件", &[&(b.paths.len())]));
                ui.horizontal(|ui| {
                    ui.label(crate::i18n::tr("検索:"));
                    ui.add(egui::TextEdit::singleline(&mut b.find).desired_width(220.0));
                    ui.label(crate::i18n::tr("置換:"));
                    ui.add(egui::TextEdit::singleline(&mut b.replace).desired_width(220.0));
                    ui.checkbox(&mut b.regex, crate::i18n::tr("正規表現"));
                    ui.checkbox(&mut b.match_case, crate::i18n::tr("大小区別"));
                });
                let (prev, err) = b.preview();
                b.error = err;
                if let Some(e) = &b.error {
                    ui.colored_label(Color32::LIGHT_RED, e);
                }
                let changed = prev.iter().filter(|(a, c)| a != c).count();
                egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                    egui::Grid::new("bulk_prev").striped(true).show(ui, |ui| {
                        for (a, c) in prev.iter().take(500) {
                            ui.label(a);
                            ui.label("→");
                            if a == c {
                                ui.label(RichText::new(c).color(Color32::GRAY));
                            } else {
                                ui.label(RichText::new(c).strong());
                            }
                            ui.end_row();
                        }
                    });
                });
                ui.horizontal(|ui| {
                    if ui.add_enabled(changed > 0 && b.error.is_none(), egui::Button::new(crate::i18n::trf("{} 件を変更", &[&changed]))).clicked() {
                        apply = true;
                    }
                    if ui.button(crate::i18n::tr("キャンセル")).clicked() {
                        keep = false;
                    }
                });
                if ui.input(|i| i.key_pressed(Key::Escape)) {
                    keep = false;
                }
            });
            if apply {
                let (prev, _) = b.preview();
                let mut ok = 0;
                let mut failed = 0;
                for (p, (a, c)) in b.paths.iter().zip(prev.iter()) {
                    if a != c && !c.is_empty() {
                        match shell::rename(p, c) {
                            Ok(()) => ok += 1,
                            Err(e) => {
                                failed += 1;
                                log::warn!("リネーム失敗 {p}: {e}");
                            }
                        }
                    }
                }
                self.notify(crate::i18n::trf("{} 件を変更しました{}", &[&ok, &(if failed > 0 { crate::i18n::trf("（{} 件失敗）", &[&failed]) } else { String::new() })]));
                self.multi.clear();
                keep = false;
            }
            if keep {
                self.bulk = Some(b);
            }
        }

        // ---- ごみ箱確認 ----
        if let Some(paths) = self.confirm_delete.clone() {
            let mut decided: Option<bool> = None;
            egui::Window::new(crate::i18n::tr("ごみ箱へ移動")).collapsible(false).resizable(false).show(ctx, |ui| {
                ui.label(crate::i18n::trf("{} 件をごみ箱へ移動します。", &[&(paths.len())]));
                for p in paths.iter().take(8) {
                    ui.monospace(p);
                }
                if paths.len() > 8 {
                    ui.label(crate::i18n::trf("… 他 {} 件", &[&(paths.len() - 8)]));
                }
                ui.horizontal(|ui| {
                    if ui.button(crate::i18n::tr("移動する")).clicked() {
                        decided = Some(true);
                    }
                    if ui.button(crate::i18n::tr("キャンセル")).clicked() {
                        decided = Some(false);
                    }
                });
                if ui.input(|i| i.key_pressed(Key::Escape)) {
                    decided = Some(false);
                }
                if ui.input(|i| i.key_pressed(Key::Enter)) {
                    decided = Some(true);
                }
            });
            if let Some(d) = decided {
                self.confirm_delete = None;
                if d {
                    let n = paths.len();
                    std::thread::spawn(move || {
                        if let Err(e) = shell::recycle(&paths) {
                            log::error!("ごみ箱へ移動できない (code {e})");
                        }
                    });
                    self.notify(crate::i18n::trf("{} 件をごみ箱へ移動しました", &[&n]));
                    self.multi.clear();
                }
            }
        }

        // ---- 下部: ステータス（ピン留め時）----
        if self.pinned {
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                match &result {
                    Some(r) => {
                        let mut s = crate::i18n::trf("{} 件 ({} ファイル, {} フォルダ)  {:.0} ms", &[&(fmt_count(r.total)), &(fmt_count(r.files)), &(fmt_count(r.folders)), &(r.elapsed_ms)]);
                        if !r.sorted {
                            s.push_str(crate::i18n::tr("  [件数が多いため未ソート]"));
                        }
                        ui.label(s);
                        for w in &r.warnings {
                            ui.colored_label(Color32::from_rgb(220, 140, 0), w);
                        }
                    }
                    None => {
                        ui.label("…");
                    }
                }
                if self.searcher.busy.load(Ordering::Relaxed) != 0 {
                    ui.spinner();
                }
                ui.separator();
                let st = self.engine.status.lock();
                if st.discovering {
                    ui.label(crate::i18n::tr("ボリューム検出中…"));
                }
                if !st.message.is_empty() {
                    ui.colored_label(Color32::LIGHT_RED, &st.message);
                }
                for v in &st.volumes {
                    let (txt, col) = match &v.state {
                        VolState::Pending => (crate::i18n::trf("{} 待機", &[&(v.root)]), Color32::GRAY),
                        VolState::Loading => (crate::i18n::trf("{} 読込中", &[&(v.root)]), Color32::from_rgb(200, 160, 0)),
                        VolState::Scanning { done, total } => (
                            crate::i18n::trf("{} スキャン {:.0}% ({})", &[&(v.root), &(*done as f64 / (*total).max(1) as f64 * 100.0), &(fmt_count(v.entries))]),
                            Color32::from_rgb(60, 140, 240),
                        ),
                        VolState::Ready => (format!("{} {}", v.root, fmt_count(v.entries)), Color32::from_rgb(40, 150, 60)),
                        VolState::Error(e) => (crate::i18n::trf("{} エラー: {}", &[&(v.root), &e]), Color32::LIGHT_RED),
                    };
                    ui.colored_label(col, txt).on_hover_text(crate::i18n::trf("USN 反映 {} 件", &[&(v.usn_events)]));
                }
                if let Some(d) = st.startup_ready_at {
                    ui.separator();
                    ui.label(crate::i18n::trf("検索可能まで {:.2}s", &[&(d.as_secs_f64())]));
                }
                if let Some(hit) = self.selected {
                    if let Some((p, _)) = self.hit_path(hit) {
                        ui.separator();
                        ui.label(RichText::new(p).color(Color32::from_gray(140)));
                    }
                }
                if let Some(t) = &self.tray {
                    if !t.hotkey_label.is_empty() {
                        ui.separator();
                        ui.label(crate::i18n::trf("ホットキー {}", &[&(t.hotkey_label)])).on_hover_text(crate::i18n::tr("accio.ini の hotkey= で変更（空で無効）"));
                    }
                }
            });
        });
        }

        // ---- プレビュー（右、ピン留め時） ----
        if self.pinned && self.show_preview {
            if self.preview_for != self.selected {
                self.preview_for = self.selected;
                match self.selected.and_then(|h| self.hit_path(h)) {
                    Some((p, is_dir)) => self.previewer.request(p, is_dir),
                    None => self.previewer.clear(),
                }
            }
            egui::SidePanel::right("preview").resizable(true).default_width(360.0).min_width(160.0).show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(crate::i18n::tr("プレビュー (Alt+P)")).strong());
                    if ui.small_button("×").clicked() {
                        self.show_preview = false;
                        self.settings.set("preview", 0u8);
                    }
                });
                ui.separator();
                let loaded = self.previewer.result.load_full();
                match loaded.as_deref() {
                    None => {
                        ui.label(RichText::new(if self.selected.is_some() { crate::i18n::tr("読み込み中…") } else { crate::i18n::tr("ファイルを選択してください") }).color(Color32::GRAY));
                    }
                    Some(l) => {
                        ui.label(RichText::new(l.path.rsplit('\\').next().unwrap_or(&l.path)).strong());
                        ui.label(RichText::new(&l.path).small().color(Color32::GRAY));
                        ui.separator();
                        match &l.content {
                            crate::preview::Content::Info(s) => {
                                ui.label(s);
                            }
                            crate::preview::Content::Text(s) => {
                                egui::ScrollArea::both().show(ui, |ui| {
                                    ui.add(egui::Label::new(RichText::new(s).monospace()).wrap_mode(egui::TextWrapMode::Extend));
                                });
                            }
                            crate::preview::Content::Image { size, rgba } => {
                                let need = self.preview_tex.as_ref().map(|(p, _)| p != &l.path).unwrap_or(true);
                                if need {
                                    let img = egui::ColorImage::from_rgba_unmultiplied(*size, rgba);
                                    let tex = ctx.load_texture("preview", img, egui::TextureOptions::LINEAR);
                                    self.preview_tex = Some((l.path.clone(), tex));
                                }
                                if let Some((_, tex)) = &self.preview_tex {
                                    let avail = ui.available_size();
                                    let [w, h] = [size[0] as f32, size[1] as f32];
                                    let scale = (avail.x / w).min(avail.y / h).min(1.0);
                                    ui.label(format!("{} × {}", size[0], size[1]));
                                    ui.add(egui::Image::new((tex.id(), egui::vec2(w * scale, h * scale))));
                                }
                            }
                        }
                    }
                }
            });
        }

        // ---- 中央: 結果テーブル（ピン留め時） ----
        let mut rename_outcome: Option<(String, bool, bool)> = None;
        let mut header_sort: Option<SortKey> = None;
        let mut toggle_col: Option<Col> = None;
        // サムネイルの完成分を取り込む
        for (path, r) in self.thumbs.take() {
            self.thumb_pending.remove(&path);
            let tex = r.map(|(size, rgba)| ctx.load_texture(format!("thumb:{path}"), egui::ColorImage::from_rgba_unmultiplied(size, &rgba), egui::TextureOptions::LINEAR));
            self.thumb_cache.insert(path, tex);
        }
        if self.thumb_cache.len() > 4000 {
            self.thumb_cache.clear();
        }
        let mut thumb_clicked: Option<(usize, (bool, bool))> = None;
        let mut thumb_double: Option<usize> = None;
        if self.pinned && self.thumb_size > 0 {
            egui::CentralPanel::default().show(ctx, |ui| {
                let Some(res) = result.clone() else {
                    ui.centered_and_justified(|ui| ui.label(crate::i18n::tr("索引を準備しています…")));
                    return;
                };
                let vols = self.engine.index.snapshot();
                let guards: Vec<Option<parking_lot::RwLockReadGuard<'_, accio_index::VolumeIndex>>> = vols.iter().map(try_read_vol).collect();
                let cell = self.thumb_size as f32 + 16.0;
                let cell_h = cell + 36.0;
                let cols = ((ui.available_width() - 8.0) / (cell + 8.0)).floor().max(1.0) as usize;
                let rows = res.hits.len().div_ceil(cols);
                let size = self.thumb_size;
                let selected_row = self.selected_row;
                let multi = self.multi.clone();
                let mut requests: Vec<String> = Vec::new();
                egui::ScrollArea::vertical().show_rows(ui, cell_h, rows, |ui, range| {
                    for r in range {
                        ui.horizontal(|ui| {
                            for c in 0..cols {
                                let i = r * cols + c;
                                if i >= res.hits.len() {
                                    break;
                                }
                                let hit = res.hits[i];
                                let Some(g) = guards.get(hit.vol as usize).and_then(|g| g.as_ref()) else {
                                    ctx.request_repaint_after(Duration::from_millis(30));
                                    continue;
                                };
                                if hit.id as usize >= g.len() {
                                    continue;
                                }
                                let is_dir = g.is_dir(hit.id);
                                let name = g.name(hit.id).to_string();
                                let full = g.full_path(hit.id);
                                let ext = String::from_utf8_lossy(g.ext(hit.id)).into_owned();
                                let (rect, resp) = ui.allocate_exact_size(egui::vec2(cell, cell_h), egui::Sense::click());
                                let sel = selected_row == Some(i) || multi.contains(&hit);
                                if sel {
                                    ui.painter().rect_filled(rect, 4.0, ui.visuals().selection.bg_fill);
                                } else if resp.hovered() {
                                    ui.painter().rect_filled(rect, 4.0, ui.visuals().widgets.hovered.bg_fill);
                                }
                                let img_rect = egui::Rect::from_min_size(rect.min + egui::vec2(8.0, 8.0), egui::vec2(size as f32, size as f32));
                                let mut drawn = false;
                                if !is_dir && crate::preview::is_image_ext(&ext) {
                                    match self.thumb_cache.get(&full) {
                                        Some(Some(tex)) => {
                                            let ts = tex.size_vec2();
                                            let scale = (size as f32 / ts.x).min(size as f32 / ts.y).min(1.0);
                                            let draw = egui::vec2(ts.x * scale, ts.y * scale);
                                            let r = egui::Rect::from_center_size(img_rect.center(), draw);
                                            ui.painter().image(tex.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                                            drawn = true;
                                        }
                                        Some(None) => {}
                                        None => {
                                            if !self.thumb_pending.contains(&full) {
                                                requests.push(full.clone());
                                            }
                                        }
                                    }
                                }
                                if !drawn {
                                    let side = size as f32 * 0.5;
                                    paint_kind_icon_at(ui.painter(), egui::Rect::from_center_size(img_rect.center(), egui::vec2(side, side)), is_dir);
                                }
                                let label_rect = egui::Rect::from_min_size(egui::pos2(rect.min.x + 4.0, img_rect.max.y + 4.0), egui::vec2(cell - 8.0, 32.0));
                                let shown: String = if name.chars().count() > 22 { name.chars().take(20).collect::<String>() + "…" } else { name.clone() };
                                ui.painter().text(label_rect.center_top(), egui::Align2::CENTER_TOP, shown, egui::FontId::proportional(13.0), ui.visuals().text_color());
                                let resp = resp.on_hover_text(&full);
                                if resp.clicked() || resp.secondary_clicked() {
                                    thumb_clicked = Some((i, ui_mods(&resp)));
                                }
                                if resp.double_clicked() {
                                    thumb_double = Some(i);
                                }
                            }
                        });
                    }
                });
                for p in requests {
                    self.thumb_pending.insert(p.clone());
                    self.thumbs.request(p, size);
                }
            });
        }
        if self.pinned && self.thumb_size == 0 {
        egui::CentralPanel::default().show(ctx, |ui| {
            let Some(res) = result.clone() else {
                ui.centered_and_justified(|ui| ui.label(crate::i18n::tr("索引を準備しています…")));
                return;
            };
            let vols = self.engine.index.snapshot();
            let row_h = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
            let font = egui::TextStyle::Body.resolve(ui.style());
            let text_color = ui.visuals().text_color();
            let highlight_color = self.colors.highlight;
            let mut table = TableBuilder::new(ui)
                .striped(true)
                .resizable(true)
                .sense(egui::Sense::click_and_drag())
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
            for c in &self.columns {
                table = table.column(match c {
                    Col::Name => Column::initial(380.0).at_least(120.0).clip(true),
                    Col::Path => Column::remainder().at_least(200.0).clip(true),
                    Col::Size => Column::initial(100.0).at_least(60.0),
                    Col::DateModified | Col::DateCreated | Col::DateAccessed | Col::DateRun => Column::initial(130.0).at_least(80.0),
                    Col::Extension => Column::initial(70.0).at_least(40.0),
                    Col::Attributes => Column::initial(60.0).at_least(40.0),
                    Col::Type => Column::initial(140.0).at_least(60.0).clip(true),
                    Col::RunCount => Column::initial(70.0).at_least(40.0),
                });
            }
            if let Some(r) = self.scroll_to.take() {
                table = table.scroll_to_row(r, None);
            }
            let selected_row = self.selected_row;
            let sort = self.sort;
            let columns = self.columns.clone();
            let needles = self.needles.clone();
            let multi = self.multi.clone();
            let renaming = self.renaming.clone();
            let mut row_rects_out: Vec<(usize, egui::Rect)> = Vec::new();
            let mut lock_missed = false;
            let path_mode = self.opts.match_path || self.query.contains("path:");
            // 実行回数列: 表示範囲だけ引くのが理想だが、履歴は小さいので履歴側から逆引きする
            let run_counts: RunInfoMap = if columns.contains(&Col::RunCount) || columns.contains(&Col::DateRun) || matches!(self.sort.key, SortKey::RunCount | SortKey::DateRun) {
                match &self.run_info_cache {
                    Some((src, m)) if Arc::ptr_eq(src, &res) => m.clone(),
                    _ => {
                        let m = Arc::new(self.run_info_for(&res, &vols));
                        self.run_info_cache = Some((res.clone(), m.clone()));
                        m
                    }
                }
            } else {
                Default::default()
            };
            let mut row_cache = std::mem::take(&mut self.row_cache);
            if row_cache.len() > 50_000 {
                row_cache.clear();
            }
            // 読み取りロックはフレームに 1 回だけ試す（行ごとに待つと書き込み中は 1 フレームが何百 ms にもなる）
            let guards: Vec<Option<parking_lot::RwLockReadGuard<'_, accio_index::VolumeIndex>>> = vols.iter().map(try_read_vol).collect();
            let mut rename_edit: Option<(String, bool, bool)> = None; // (text, commit, cancel)
            table
                .header(24.0, |mut h| {
                    for c in &columns {
                        h.col(|ui| {
                            let mark = match c.sort_key() {
                                Some(k) if sort.key == k => if sort.ascending { " ▲" } else { " ▼" },
                                _ => "",
                            };
                            let r = ui.add(egui::Label::new(RichText::new(format!("{}{mark}", c.label())).strong()).sense(egui::Sense::click()));
                            if r.clicked() {
                                header_sort = c.sort_key();
                            }
                            r.context_menu(|ui| {
                                ui.label(crate::i18n::tr("列の表示"));
                                for cc in Col::ALL {
                                    let mut on = columns.contains(&cc);
                                    if ui.checkbox(&mut on, cc.label()).clicked() {
                                        toggle_col = Some(cc);
                                        ui.close();
                                    }
                                }
                            });
                        });
                    }
                })
                .body(|body| {
                    body.rows(row_h, res.hits.len(), |mut row| {
                        let i = row.index();
                        let hit = res.hits[i];
                        row.set_selected(selected_row == Some(i) || multi.contains(&hit));
                        if hit.vol as usize >= vols.len() {
                            return;
                        }
                        let data = match guards[hit.vol as usize].as_ref() {
                            Some(g) => {
                                if hit.id as usize >= g.len() {
                                    return;
                                }
                                let d = Arc::new(RowData::read(g, hit.id));
                                row_cache.insert(hit, d.clone());
                                d
                            }
                            None => match row_cache.get(&hit) {
                                Some(d) => d.clone(),
                                None => {
                                    lock_missed = true;
                                    return;
                                }
                            },
                        };
                        let RowData { is_dir, size, mtime, ctime, atime, attrs, .. } = *data;
                        let (name, path, ext) = (data.name.clone(), data.path.clone(), data.ext.clone());
                        for c in &columns {
                            match c {
                                Col::Name => row.col(|ui| {
                                    paint_kind_icon(ui, is_dir);
                                    if let Some((rh, text)) = &renaming {
                                        if *rh == hit {
                                            let mut t = text.clone();
                                            let r = ui.add(egui::TextEdit::singleline(&mut t).desired_width(f32::INFINITY));
                                            r.request_focus();
                                            let commit = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                                            let cancel = ui.input(|i| i.key_pressed(Key::Escape));
                                            rename_edit = Some((t, commit, cancel));
                                            return;
                                        }
                                    }
                                    ui.label(highlighted(&name, &needles, text_color, font.clone(), highlight_color));
                                }),
                                Col::Path => row.col(|ui| {
                                    if path_mode {
                                        ui.label(highlighted(&path, &needles, Color32::from_gray(110), font.clone(), highlight_color));
                                    } else {
                                        ui.label(RichText::new(&path).color(Color32::from_gray(110)));
                                    }
                                }),
                                Col::Size => row.col(|ui| {
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        let t = if is_dir && size == 0 { String::new() } else { fmt_size(size) };
                                        ui.label(RichText::new(t).color(if is_dir { Color32::from_rgb(160, 120, 30) } else { Color32::from_gray(90) }));
                                    });
                                }),
                                Col::DateModified => row.col(|ui| {
                                    ui.label(format_filetime(mtime));
                                }),
                                Col::DateCreated => row.col(|ui| {
                                    ui.label(format_filetime(ctime));
                                }),
                                Col::Extension => row.col(|ui| {
                                    ui.label(&ext);
                                }),
                                Col::Attributes => row.col(|ui| {
                                    ui.label(attrib_string(attrs));
                                }),
                                Col::Type => row.col(|ui| {
                                    ui.label(shell::type_name(&ext, is_dir));
                                }),
                                Col::RunCount => row.col(|ui| {
                                    let c = run_counts.get(&hit).map(|x| x.0).unwrap_or(0);
                                    ui.label(if c > 0 { c.to_string() } else { String::new() });
                                }),
                                Col::DateRun => row.col(|ui| {
                                    let t = run_counts.get(&hit).map(|x| x.1).unwrap_or(0);
                                    ui.label(if t > 0 { format_filetime(t) } else { String::new() });
                                }),
                                Col::DateAccessed => row.col(|ui| {
                                    ui.label(format_filetime(atime));
                                }),
                            };
                        }
                        let r = row.response();
                        row_rects_out.push((i, r.rect));
                        if r.clicked() || r.secondary_clicked() {
                            rows_out.clicked = Some((i, ui_mods(&r)));
                        }
                        if r.drag_started_by(egui::PointerButton::Primary) {
                            if let Some(pos) = r.interact_pointer_pos() {
                                rows_out.drag_start = Some((i, pos));
                            }
                        }
                        if r.double_clicked() {
                            rows_out.double = Some(i);
                        }
                        let full = if data.path.is_empty() { data.name.clone() } else { format!("{}\\{}", data.path, data.name) };
                        r.context_menu(|ui| row_context_menu(ui, &full, &name, is_dir, multi.len(), hit, &mut rows_out));
                    });
                });
            rename_outcome = rename_edit;
            self.row_rects = row_rects_out;
            self.row_cache = row_cache;
            if lock_missed {
                // 索引が書き込み中で描けなかった行がある: 次のフレームで描き直す
                ctx.request_repaint_after(Duration::from_millis(30));
            }
        });
        }
        if let Some((i, m)) = thumb_clicked {
            rows_out.clicked = Some((i, m));
        }
        if let Some(i) = thumb_double {
            rows_out.double = Some(i);
        }
        let RowActions { clicked, double, rename_req, delete_req, shell_menu_req, copy_files_req, du_into, drag_start } = rows_out;
        if let Some((text, commit, cancel)) = rename_outcome {
            if cancel {
                self.renaming = None;
            } else if commit {
                if let Some((hit, _)) = self.renaming.take() {
                    if let Some((path, _)) = self.hit_path(hit) {
                        let new_name = text.trim().to_string();
                        if !new_name.is_empty() {
                            match shell::rename(&path, &new_name) {
                                Ok(()) => self.notify(crate::i18n::trf("名前を変更しました: {}", &[&new_name])),
                                Err(e) => self.notify(crate::i18n::trf("名前を変更できません: {}", &[&e])),
                            }
                        }
                    }
                }
            } else if let Some((_, t)) = &mut self.renaming {
                *t = text;
            }
        }
        if let Some(hit) = rename_req {
            if self.multi.len() > 1 {
                let paths = self.selected_paths();
                self.bulk = Some(BulkRename { paths, find: String::new(), replace: String::new(), regex: false, match_case: false, error: None });
            } else if let Some(vol) = self.engine.index.snapshot().get(hit.vol as usize) {
                let name = vol.read_recursive().name(hit.id).to_string();
                self.selected = Some(hit);
                self.renaming = Some((hit, name));
            }
        }
        if delete_req {
            let paths = self.selected_paths();
            if !paths.is_empty() {
                self.confirm_delete = Some(paths);
            }
        }
        if copy_files_req {
            let paths = self.selected_paths();
            if !paths.is_empty() && shell::copy_files_to_clipboard(&paths) {
                self.notify(crate::i18n::trf("{} 件のファイルをコピーしました", &[&(paths.len())]));
            }
        }
        if let Some((i, pos)) = drag_start {
            if let Some(r) = result.as_ref() {
                let hit = r.hits[i];
                if !self.multi.contains(&hit) {
                    self.multi.clear();
                    self.select_row(r, i);
                }
            }
            self.drag_candidate = Some((i, pos));
        }
        if let Some((_, origin)) = self.drag_candidate {
            let (down, cur) = ctx.input(|i| (i.pointer.primary_down(), i.pointer.latest_pos()));
            if !down {
                self.drag_candidate = None;
            } else if let Some(cur) = cur {
                if (cur - origin).length() > 8.0 {
                    self.drag_candidate = None;
                    let paths = self.selected_paths();
                    if !paths.is_empty() {
                        if let Err(e) = crate::dragdrop::start_drag(&paths) {
                            log::warn!("ドラッグ開始に失敗: {e}");
                        }
                    }
                }
            }
        }
        if shell_menu_req {
            // 右クリックした行が選択に含まれていなければその行だけを対象にする
            let paths = self.selected_paths();
            if !paths.is_empty() {
                if let Err(e) = crate::shellmenu::show(self.hwnd, &paths) {
                    self.notify(crate::i18n::trf("シェルメニューを表示できません: {}", &[&e]));
                }
            }
        }
        if let Some(c) = toggle_col {
            if let Some(pos) = self.columns.iter().position(|x| *x == c) {
                if self.columns.len() > 1 {
                    self.columns.remove(pos);
                }
            } else {
                self.columns.push(c);
            }
            self.save_settings();
        }
        if let Some(k) = header_sort {
            if self.sort.key == k {
                self.sort.ascending = !self.sort.ascending;
            } else {
                self.sort = Sort { key: k, ascending: !matches!(k, SortKey::Size | SortKey::DateModified | SortKey::DateCreated) };
            }
        }
        if let Some(r) = result.as_ref() {
            if let Some((i, (ctrl, shift))) = clicked {
                let hit = r.hits[i];
                if ctrl {
                    if self.multi.is_empty() {
                        self.multi.extend(self.selected);
                    }
                    if let Some(p) = self.multi.iter().position(|h| *h == hit) {
                        self.multi.remove(p);
                    } else {
                        self.multi.push(hit);
                    }
                    self.selected = Some(hit);
                    self.selected_row = Some(i);
                } else if shift {
                    let anchor = self.selected_row.unwrap_or(i);
                    let (a, b) = if anchor <= i { (anchor, i) } else { (i, anchor) };
                    self.multi = r.hits[a..=b].to_vec();
                    self.selected = Some(hit);
                    self.selected_row = Some(i);
                } else {
                    if !self.multi.contains(&hit) || self.multi.len() <= 1 {
                        self.multi.clear();
                    }
                    self.select_row(r, i);
                }
            }
            if let Some(i) = double {
                self.select_row(r, i);
                self.open_selected(false);
            }
        }
        if let Some(p) = du_into {
            self.mode = Mode::DiskUsage;
            self.du_path = Some(p);
            self.sort = Sort { key: SortKey::Size, ascending: false };
            self.query.clear();
            self.selected = None;
            self.selected_row = None;
        }

        self.maybe_submit();
        if self.pinned && self.mode_applied == Some(true) {
            if let Some(rect) = ctx.input(|i| i.viewport().outer_rect) {
                self.last_rect = Some((rect.min.x, rect.min.y, rect.width(), rect.height()));
            }
            self.last_maximized = ctx.input(|i| i.viewport().maximized).unwrap_or(false);
        }
        let busy = self.engine.status.lock().volumes.iter().any(|v| matches!(v.state, VolState::Scanning { .. } | VolState::Loading));
        if busy {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.run_ui_script(ctx, raw_input);
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        if self.pinned {
            visuals.panel_fill.to_normalized_gamma_f32()
        } else {
            [0.0, 0.0, 0.0, 0.0]
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // セッション保存（各タブの検索語）
        self.tabs[self.active_tab] = self.snapshot_tab();
        for i in 0..32 {
            let k = format!("session.tab.{i}");
            match self.tabs.get(i) {
                Some(t) => self.settings.set(&k, t.query.clone()),
                None => {
                    if self.settings.get(&k).is_some() {
                        self.settings.set(&k, "");
                    }
                }
            }
        }
        self.settings.set("session.active", self.active_tab);
        self.remember_geometry();
        self.run_history.save();
        self.save_settings();
        self.engine.stop_and_save();
    }
}
