//! accio GUI。UI スレッドは索引スナップショットと検索結果を描画するだけで、I/O も検索も行わない。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod dragdrop;
mod elevation;
mod engine;
mod etp;
mod history;
mod http;
mod ipc;
mod ipc3;
mod preview;
mod searcher;
mod settings;
mod shell;
mod shellmenu;
mod tray;
mod winshow;
mod ui;

use std::sync::Arc;

/// Everything 互換のコマンドラインオプション（対応分のみ）。
#[derive(Default, Clone)]
pub struct CliOptions {
    pub search: Option<String>,
    pub case: bool,
    pub path: bool,
    pub whole_word: bool,
    pub regex: bool,
    /// `-a` / `-diacritics`: ダイアクリティカルマークを区別
    pub diacritics: bool,
    pub sort: Option<accio_query::Sort>,
    /// `-instance <name>`: IPC ウィンドウクラスに `_(<name>)` を付ける（Everything 互換）
    pub instance: Option<String>,
    /// `-startup`: ウィンドウを出さずにトレイ常駐で開始
    pub startup: bool,
    /// `-new-window`: 起動中のインスタンスがあっても転送せず新しく起動する
    pub new_window: bool,
    /// `-filter <name>`: 起動時に選ぶフィルタ名
    pub filter: Option<String>,
    /// `-filelist <path>`: 起動時にファイルリスト（EFU/テキスト）を開く
    pub filelist: Option<String>,
    /// `-indexer`: ウィンドウなしでインデクサ（索引構築・USN 追従・IPC/HTTP/ETP）だけを動かす
    pub indexer: bool,
}

fn parse_cli() -> CliOptions {
    use accio_query::{Sort, SortKey};
    let mut o = CliOptions::default();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.to_ascii_lowercase().as_str() {
            "-search" | "-s" => o.search = it.next(),
            "-case" | "-i" => o.case = true,
            "-match-path" | "-p" => o.path = true,
            "-ww" | "-whole-word" => o.whole_word = true,
            "-regex" | "-r" => o.regex = true,
            "-diacritics" | "-a" => o.diacritics = true,
            "-instance" => o.instance = it.next(),
            "-help" | "-h" | "-?" | "--help" => {
                println!(
                    "accio [options]\n  -s, -search <text>   検索語\n  -i, -case            大小区別\n  -p, -match-path      パス一致\n  -ww                  単語一致\n  -r, -regex           正規表現\n  -a, -diacritics      ダイアクリティカルマークを区別\n  -sort <name|path|size|dm|dc|da|ext|runcount|dr>[-descending]\n  -filter <name>       フィルタ\n  -filelist <path>     ファイルリスト（EFU/テキスト）を開いて検索\n  -instance <name>     インスタンス名（IPC の識別）\n  -startup             トレイ常駐で開始（ウィンドウを出さない）\n  -new-window          起動中のインスタンスへ転送せず新しく起動\n  -new-tab             起動中のインスタンスに新しいタブで検索\n  -toggle-window / -exit / -reindex / -save-db   起動中のインスタンスへ転送\n環境変数: ACCIO_NO_ELEVATE=1（昇格せず読み取り専用）, ACCIO_DATA_DIR=<dir>"
                );
                std::process::exit(0);
            }
            "-startup" => o.startup = true,
            "-indexer" => o.indexer = true,
            "-new-window" | "-newwindow" => o.new_window = true,
            "-filter" => o.filter = it.next(),
            "-filelist" | "-file-list" => o.filelist = it.next(),
            "-url" => {
                if let Some(u) = it.next() {
                    let q = u.strip_prefix("es:").or_else(|| u.strip_prefix("ES:")).unwrap_or(&u);
                    let q = q.trim_start_matches("//");
                    o.search = Some(url_decode(q));
                }
            }
            "-sort" => {
                let v = it.next().unwrap_or_default().to_ascii_lowercase();
                let (key, desc) = (v.trim_end_matches("-ascending").trim_end_matches("-descending"), v.ends_with("-descending"));
                let key = match key {
                    "name" => SortKey::Name,
                    "path" => SortKey::Path,
                    "size" => SortKey::Size,
                    "dm" | "date-modified" => SortKey::DateModified,
                    "dc" | "date-created" => SortKey::DateCreated,
                    "da" | "date-accessed" => SortKey::DateAccessed,
                    "dr" | "date-run" => SortKey::DateRun,
                    "ext" | "extension" => SortKey::Extension,
                    "run-count" | "runcount" => SortKey::RunCount,
                    _ => continue,
                };
                o.sort = Some(Sort { key, ascending: !desc });
            }
            _ => {}
        }
    }
    o
}

/// HTTP / ETP サーバーを設定に従って開始する（UI 単体のときとインデクサのとき）。
fn start_servers(settings: &settings::Settings, engine: &Arc<engine::Engine>) {
    if settings.bool("http_server", false) {
        let port: u16 = settings.get("http_port").and_then(|v| v.parse().ok()).unwrap_or(80);
        let bind = settings.get("http_bind").unwrap_or("127.0.0.1").to_string();
        let auth = match (settings.get("http_user"), settings.get("http_password")) {
            (Some(u), Some(p)) if !u.is_empty() => Some(format!("{u}:{p}")),
            _ => None,
        };
        http::start(engine.index.clone(), &bind, port, auth);
    }
    if settings.bool("etp_server", false) {
        etp::start(
            engine.index.clone(),
            etp::EtpConfig {
                bind: settings.get("etp_bind").unwrap_or("0.0.0.0").to_string(),
                port: settings.get("etp_port").and_then(|v| v.parse().ok()).unwrap_or(21),
                user: settings.get("etp_user").map(|s| s.to_string()),
                password: settings.get("etp_password").map(|s| s.to_string()),
                allow_download: settings.bool("etp_allow_download", false),
            },
        );
    }
}

fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    out.push(v);
                    i += 2;
                } else {
                    out.push(b'%');
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn data_dir() -> std::path::PathBuf {
    if let Some(d) = std::env::var_os("ACCIO_DATA_DIR") {
        return std::path::PathBuf::from(d);
    }
    let base = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("accio")
}

fn main() -> eframe::Result<()> {
    // 生ボリューム読み取りと USN には管理者権限が必要。非昇格なら自分を昇格起動して終了する。
    // ACCIO_NO_ELEVATE=1 なら保存済み索引だけで動く読み取り専用モード。
    let elevated = elevation::is_elevated();
    let pre = parse_cli();
    let pre_settings = settings::Settings::load(&data_dir().join("accio.ini"));
    // UI が別プロセスのインデクサを参照する構成では UI は昇格しない
    let follow_indexer = !pre.indexer && pre_settings.bool("use_indexer", false);
    if !elevated && !follow_indexer && std::env::var_os("ACCIO_NO_ELEVATE").is_none()
        && elevation::relaunch_elevated() {
            return Ok(());
        }
    if let Some(p) = std::env::var_os("ACCIO_SHELLMENU_PROBE") {
        // 検証用: シェルメニューを構築できるか確かめて終了
        match shellmenu::probe(&p.to_string_lossy()) {
            Ok(n) => println!("shellmenu items: {n}"),
            Err(e) => println!("shellmenu error: {e}"),
        }
        return Ok(());
    }
    if let Some(v) = std::env::var_os("ACCIO_URLPROTO_PROBE") {
        let on = v.to_string_lossy() == "on";
        println!("set_url_protocol({on}) = {}, now registered = {}", shell::set_url_protocol(on), shell::is_url_protocol());
        return Ok(());
    }
    if let Some(v) = std::env::var_os("ACCIO_CTXMENU_PROBE") {
        // 検証用: フォルダ右クリックメニューの登録 (on) / 削除 (off)
        let on = v.to_string_lossy() == "on";
        println!("set_folder_context_menu({on}) = {}, now registered = {}", shell::set_folder_context_menu(on), shell::is_folder_context_menu());
        return Ok(());
    }
    if let Some(spec) = std::env::var_os("ACCIO_DND_PROBE") {
        // 検証用: CF_HDROP データオブジェクトの往復（";" 区切りのパス）
        let paths: Vec<String> = spec.to_string_lossy().split(';').map(|s| s.to_string()).collect();
        match dragdrop::probe(&paths) {
            Ok(v) => println!("hdrop: {v:?}"),
            Err(e) => println!("hdrop error: {e}"),
        }
        return Ok(());
    }
    if let Some(p) = std::env::var_os("ACCIO_PREVIEW_PROBE") {
        // 検証用: プレビュー読み込みの結果種別を表示して終了
        let p = p.to_string_lossy().to_string();
        match preview::load_for_probe(&p) {
            preview::Content::Image { size, rgba } => println!("image {}x{} ({} bytes)", size[0], size[1], rgba.len()),
            preview::Content::Text(t) => println!("text {} chars: {:?}", t.chars().count(), t.lines().next().unwrap_or("")),
            preview::Content::Info(i) => println!("info: {i}"),
        }
        return Ok(());
    }
    if let Some(spec) = std::env::var_os("ACCIO_BULK_PREVIEW") {
        // 検証用: "find|replace|regex(0/1)|case(0/1)|path;path" の置換プレビューを表示して終了
        let spec = spec.to_string_lossy().to_string();
        let parts: Vec<&str> = spec.splitn(5, '|').collect();
        if parts.len() == 5 {
            let b = ui::BulkRename {
                paths: parts[4].split(';').map(|s| s.to_string()).collect(),
                find: parts[0].to_string(),
                replace: parts[1].to_string(),
                regex: parts[2] == "1",
                match_case: parts[3] == "1",
                error: None,
            };
            let (prev, err) = b.preview();
            for (a, c) in prev {
                println!("{a} -> {c}");
            }
            if let Some(e) = err {
                println!("error: {e}");
            }
        }
        return Ok(());
    }
    accio_query::set_type_name_provider(Box::new(shell::type_name));
    let mut cli = parse_cli();
    if cli.indexer && cli.instance.is_none() {
        cli.instance = Some("accio-indexer".into());
    }
    // 二重起動: 起動中のインスタンスへコマンドラインを転送して終了する（Everything と同じ振る舞い）
    if !cli.new_window && !cli.startup && !cli.indexer {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if ipc::forward_to_running(&cli.instance, &args) {
            return Ok(());
        }
    }
    let dir = data_dir();
    std::fs::create_dir_all(&dir).ok();
    let log_path = dir.join("accio.log");
    let log_file = std::fs::OpenOptions::new().create(true).append(true).open(&log_path).ok();
    let mut builder = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"));
    if let Some(f) = log_file {
        builder.target(env_logger::Target::Pipe(Box::new(f)));
    }
    builder.init();
    // panic は場所とバックトレースをログに残す。背景スレッドの panic でプロセスを落とさない（unwind）
    std::panic::set_hook(Box::new(|info| {
        let bt = std::backtrace::Backtrace::force_capture();
        log::error!("panic: {info}
{bt}");
    }));
    log::info!("accio {} 起動 (elevated={elevated}, indexer={}, follow_indexer={follow_indexer})", env!("CARGO_PKG_VERSION"), cli.indexer);

    let settings = settings::Settings::load(&dir.join("accio.ini"));
    if cli.indexer {
        // ウィンドウなしのインデクサ。終了は IPC の EXIT（`accio -instance accio-indexer -exit`）か taskkill
        let repaint: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
        let engine = engine::Engine::new(dir.clone(), repaint, elevated, false, engine::SAVE_INTERVAL_INDEXER);
        *engine.folders.lock() = settings
            .get("folders")
            .map(|v| v.split(';').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
            .unwrap_or_default();
        engine.folder_rescan_minutes.store(settings.get("folder_rescan_minutes").and_then(|v| v.parse().ok()).unwrap_or(0), std::sync::atomic::Ordering::Relaxed);
        accio_query::set_search_prefix(crate::settings::exclude_prefix_from(&settings));
        engine.start();
        start_servers(&settings, &engine);
        let eng2 = engine.clone();
        let external = Arc::new(parking_lot::Mutex::new(Vec::new()));
        ipc3::start(cli.instance.clone(), Arc::new(ipc3::Ipc3Context { index: engine.index.clone(), db_loaded: engine.db_loaded.clone(), generation: engine.generation.clone(), journal: engine.journal.clone() }));
        ipc::start(cli.instance.clone(), Arc::new(ipc::IpcContext {
            index: engine.index.clone(),
            elevated,
            db_loaded: engine.db_loaded.clone(),
            on_exit: Box::new(move || {
                eng2.stop_and_save();
                std::process::exit(0);
            }),
            external: external.clone(),
            repaint: Arc::new(|| {}),
            reindex_requested: engine.reindex_requested.clone(),
            save_requested: engine.save_requested.clone(),
        }));
        // UI が無いので、転送されたコマンド（-exit / -reindex / -save-db）はここで処理する
        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let cmds: Vec<ipc::ExternalCommand> = std::mem::take(&mut *external.lock());
            for c in cmds {
                match c {
                    ipc::ExternalCommand::Exit => {
                        log::info!("インデクサ終了要求");
                        engine.stop_and_save();
                        std::process::exit(0);
                    }
                    ipc::ExternalCommand::Reindex => {
                        engine.reindex_requested.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    ipc::ExternalCommand::SaveDb => {
                        engine.save_requested.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    _ => {}
                }
            }
        }
    }
    let geom = |k: &str, d: f32| settings.get(k).and_then(|v| v.parse::<f32>().ok()).unwrap_or(d);
    let icon = {
        // 256px の PNG（MONOFORM 022 A-PULL）。ウィンドウ左上・タスクバー・ALT+TAB で使われる
        let img = image::load_from_memory(include_bytes!("../assets/icon/accio@256.png")).expect("icon png").to_rgba8();
        let (w, h) = img.dimensions();
        egui::IconData { rgba: img.into_raw(), width: w, height: h }
    };
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("accio")
        .with_icon(std::sync::Arc::new(icon))
        .with_inner_size([geom("window_w", 1200.0).max(600.0), geom("window_h", 760.0).max(300.0)])
        .with_min_inner_size([600.0, 300.0])
        .with_visible(!cli.startup);
    if let (Some(x), Some(y)) = (settings.get("window_x").and_then(|v| v.parse::<f32>().ok()), settings.get("window_y").and_then(|v| v.parse::<f32>().ok())) {
        viewport = viewport.with_position([x, y]);
    }
    if settings.bool("window_maximized", false) {
        viewport = viewport.with_maximized(true);
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native(
        "accio",
        options,
        Box::new(move |cc| {
            ui::install_fonts(&cc.egui_ctx, settings.get("font"), settings.get("font_size").and_then(|v| v.parse().ok()).unwrap_or(13.0));
            let ctx = cc.egui_ctx.clone();
            let repaint: Arc<dyn Fn() + Send + Sync> = Arc::new(move || ctx.request_repaint());
            let engine = engine::Engine::new(dir.clone(), repaint, elevated, follow_indexer, std::time::Duration::from_secs(60));
            *engine.folders.lock() = settings
                .get("folders")
                .map(|v| v.split(';').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
                .unwrap_or_default();
            engine.folder_rescan_minutes.store(settings.get("folder_rescan_minutes").and_then(|v| v.parse().ok()).unwrap_or(0), std::sync::atomic::Ordering::Relaxed);
            engine.start();
            if !follow_indexer {
                start_servers(&settings, &engine);
            }
            ipc3::start(cli.instance.clone(), Arc::new(ipc3::Ipc3Context {
                index: engine.index.clone(),
                db_loaded: engine.db_loaded.clone(),
                generation: engine.generation.clone(),
                journal: engine.journal.clone(),
            }));
            if std::env::var_os("ACCIO_JOURNAL_PROBE").is_some() {
                // 検証用: 合成の変更をジャーナルへ入れる（READ_JOURNAL の符号化確認）
                let mut j = engine.journal.lock();
                j.push(accio_index::update::Change { kind: accio_index::update::CH_FILE_CREATE, old_path: "C:\\probe".into(), old_name: "a.txt".into(), new_path: String::new(), new_name: String::new(), size: 123, mtime: 1, ctime: 2, atime: 0, attrs: 32 });
                j.push(accio_index::update::Change { kind: accio_index::update::CH_FILE_MOVE, old_path: "C:\\probe".into(), old_name: "a.txt".into(), new_path: "C:\\probe\\sub".into(), new_name: "b.txt".into(), size: 123, mtime: 1, ctime: 2, atime: 0, attrs: 32 });
                j.push(accio_index::update::Change { kind: accio_index::update::CH_FOLDER_DELETE, old_path: "C:\\probe".into(), old_name: "sub".into(), new_path: String::new(), new_name: String::new(), size: u64::MAX, mtime: 0, ctime: 0, atime: 0, attrs: 16 });
            }
            let eng2 = engine.clone();
            let external = Arc::new(parking_lot::Mutex::new(Vec::new()));
            let ctx2 = cc.egui_ctx.clone();
            ipc::start(cli.instance.clone(), Arc::new(ipc::IpcContext {
                index: engine.index.clone(),
                elevated,
                db_loaded: engine.db_loaded.clone(),
                on_exit: Box::new(move || {
                    eng2.stop_and_save();
                    std::process::exit(0);
                }),
                external: external.clone(),
                repaint: Arc::new(move || ctx2.request_repaint()),
                reindex_requested: engine.reindex_requested.clone(),
                save_requested: engine.save_requested.clone(),
            }));
            Ok(Box::new(ui::App::new(cc, engine, cli.clone(), dir.join("accio.ini"), external)))
        }),
    )
}
