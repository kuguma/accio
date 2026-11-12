//! エンジン: ボリューム検出 → 索引の読み込みまたは MFT スキャン → USN 監視 → 保存、を統括する。
//!
//! すべて UI スレッド外で動く。UI は `Engine::index` のスナップショットで検索し、`status` を表示するだけ。
//! 1 ボリュームにつき 1 スレッド。遅い・壊れたボリュームが他に影響しない。

use accio_index::{builder::VolumeBuilder, store, update, Index, SharedVolume, VolumeMeta};
use accio_ntfs::refs::{to_mft_entries_with_root, IdMap, RefsAdapter};
use accio_ntfs::{MftEntry, RecordSource, UsnEvent};

/// USN イベントが参照するレコード（自身と親）を先に読んでおくレコード源。
/// 索引の書き込みロックの中で ioctl を待たないための前段。読めなかったものは中で再試行する。
struct PrefetchedRecords<'a> {
    inner: &'a UsnJournal,
    cache: std::collections::HashMap<u64, Option<MftEntry>>,
}

impl<'a> PrefetchedRecords<'a> {
    /// 親は索引に無いもの（新しいフォルダ）だけ読む。既知の親まで読むと C: の変更量では ioctl が毎秒数万回になり追従が遅れる
    fn new(inner: &'a UsnJournal, events: &[UsnEvent], vol: &SharedVolume) -> Self {
        use accio_ntfs::usn::reason;
        const RECNO_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
        let mut cache = std::collections::HashMap::new();
        let known_parents: std::collections::HashSet<u64> = {
            let g = vol.read();
            events.iter().map(|e| e.parent_frn).filter(|p| g.entry_by_recno(p & RECNO_MASK).map(|i| g.frn(i) == *p).unwrap_or(false)).collect()
        };
        for e in events {
            if e.reason & reason::CLOSE == 0 {
                continue;
            }
            if e.reason & (reason::FILE_CREATE | reason::RENAME_NEW_NAME | reason::DATA_OVERWRITE | reason::DATA_EXTEND | reason::DATA_TRUNCATION | reason::BASIC_INFO_CHANGE | reason::HARD_LINK_CHANGE) != 0 {
                cache.entry(e.frn).or_insert_with(|| RecordSource::read_file_record(inner, e.frn));
            }
            if !known_parents.contains(&e.parent_frn) {
                cache.entry(e.parent_frn).or_insert_with(|| RecordSource::read_file_record(inner, e.parent_frn));
            }
        }
        Self { inner, cache }
    }
}

impl RecordSource for PrefetchedRecords<'_> {
    fn read_file_record(&self, frn: u64) -> Option<MftEntry> {
        match self.cache.get(&frn) {
            Some(r) => r.clone(),
            None => RecordSource::read_file_record(self.inner, frn),
        }
    }
}
use accio_ntfs::{discover_volumes, scan_mft, FileIdReader, FileReader, PositionalRead, ScanOptions, UsnJournal, VolumeInfo};
use parking_lot::{Mutex, RwLock};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub enum VolState {
    Pending,
    Loading,
    Scanning { done: u64, total: u64 },
    Ready,
    Error(String),
}

#[derive(Debug, Clone)]
pub struct VolStatus {
    pub root: String,
    pub state: VolState,
    pub entries: usize,
    pub usn_events: u64,
    pub last_change: Option<Instant>,
}

#[derive(Default)]
pub struct EngineStatus {
    pub volumes: Vec<VolStatus>,
    pub discovering: bool,
    pub message: String,
    pub startup_ready_at: Option<Duration>,
}

pub struct Engine {
    pub index: Arc<Index>,
    pub status: Arc<Mutex<EngineStatus>>,
    /// 索引が変化するたびに増える。UI はこれを見て再検索する。
    pub generation: Arc<AtomicU64>,
    pub shutdown: Arc<AtomicBool>,
    /// 少なくとも 1 ボリュームが検索可能になった
    pub db_loaded: Arc<AtomicBool>,
    /// 外部からの要求（IPC の REBUILD_DB / SAVE_DB、-reindex）。要求ごとに増える世代番号で、各ボリュームが自分の既読値と比べる
    pub reindex_requested: Arc<AtomicU64>,
    pub save_requested: Arc<AtomicU64>,
    /// フォルダ索引の対象（accio.ini の folders=）
    pub folders: parking_lot::Mutex<Vec<String>>,
    /// フォルダ索引の定期再走査の間隔（分、0 で無効。accio.ini の folder_rescan_minutes=）
    pub folder_rescan_minutes: AtomicU64,
    /// ReFS ボリューム（guid_path）ごとの ID 変換層。保存時に表を索引へ写す
    refs: Mutex<std::collections::HashMap<String, Arc<RefsAdapter>>>,
    /// 別プロセス（昇格インデクサ）が保存する索引ファイルを読むだけのモード。ファイル更新を監視して再読込する
    pub follow_indexer: bool,
    /// 変更が落ち着いてから保存するまでの時間
    pub save_settle: Duration,
    pub journal: Arc<Mutex<Journal>>,
    data_dir: PathBuf,
    pub elevated: bool,
    started: Instant,
    repaint: Arc<dyn Fn() + Send + Sync>,
    workers: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

/// 索引ジャーナル（Everything 1.5 の Index Journal 相当）。change_id は 0 起点で単調増加。
pub struct Journal {
    pub id: u64,
    pub first: u64,
    pub next: u64,
    pub entries: std::collections::VecDeque<(u64, i64, accio_index::update::Change)>,
    pub max_entries: usize,
}

impl Journal {
    pub fn new() -> Self {
        Self { id: ft_now() as u64, first: 0, next: 0, entries: Default::default(), max_entries: 100_000 }
    }
    pub fn push(&mut self, c: accio_index::update::Change) {
        let id = self.next;
        self.next += 1;
        self.entries.push_back((id, ft_now(), c));
        while self.entries.len() > self.max_entries {
            self.entries.pop_front();
            self.first += 1;
        }
    }
}

pub const USN_MAX_SIZE: u64 = 128 << 20;
pub const USN_ALLOC_DELTA: u64 = 8 << 20;
/// インデクサモード（UI が別プロセス）では早めに保存して UI へ反映する
pub const SAVE_INTERVAL_INDEXER: Duration = Duration::from_secs(8);

fn ft_now() -> i64 {
    let mut ft = windows_sys::Win32::Foundation::FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    unsafe { windows_sys::Win32::System::SystemInformation::GetSystemTimeAsFileTime(&mut ft) };
    ((ft.dwHighDateTime as i64) << 32) | ft.dwLowDateTime as i64
}

impl Engine {
    pub fn new(data_dir: PathBuf, repaint: Arc<dyn Fn() + Send + Sync>, elevated: bool, follow_indexer: bool, save_settle: Duration) -> Arc<Self> {
        std::fs::create_dir_all(&data_dir).ok();
        Arc::new(Self {
            index: Arc::new(Index::default()),
            status: Arc::new(Mutex::new(EngineStatus::default())),
            generation: Arc::new(AtomicU64::new(0)),
            shutdown: Arc::new(AtomicBool::new(false)),
            db_loaded: Arc::new(AtomicBool::new(false)),
            reindex_requested: Arc::new(AtomicU64::new(0)),
            save_requested: Arc::new(AtomicU64::new(0)),
            folders: parking_lot::Mutex::new(Vec::new()),
            folder_rescan_minutes: AtomicU64::new(0),
            refs: Mutex::new(std::collections::HashMap::new()),
            follow_indexer,
            save_settle,
            journal: Arc::new(Mutex::new(Journal::new())),
            data_dir,
            elevated,
            started: Instant::now(),
            repaint,
            workers: Mutex::new(Vec::new()),
        })
    }

    fn bump(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        (self.repaint)();
    }

    fn set_state(&self, slot: usize, f: impl FnOnce(&mut VolStatus)) {
        let mut st = self.status.lock();
        if let Some(v) = st.volumes.get_mut(slot) {
            f(v);
        }
        drop(st);
        (self.repaint)();
    }

    pub fn start(self: &Arc<Self>) {
        let me = self.clone();
        let h = std::thread::Builder::new()
            .name("discover".into())
            .spawn(move || {
                me.status.lock().discovering = true;
                let vols = discover_volumes(Duration::from_secs(5));
                {
                    let mut st = me.status.lock();
                    st.discovering = false;
                    st.volumes = vols
                        .iter()
                        .map(|v| VolStatus { root: v.display_root(), state: VolState::Pending, entries: 0, usn_events: 0, last_change: None })
                        .collect();
                    if vols.is_empty() {
                        st.message = crate::i18n::tr("固定ディスクの NTFS ボリュームが見つかりません").into();
                    } else if me.follow_indexer {
                        st.message = crate::i18n::tr("インデクサ（別プロセス）の索引を参照中").into();
                    } else if !me.elevated {
                        st.message = crate::i18n::tr("読み取り専用（非昇格）: 保存済み索引のみ。更新は反映されません").into();
                    }
                }
                (me.repaint)();
                let mut handles = Vec::new();
                for (i, v) in vols.into_iter().enumerate() {
                    let me2 = me.clone();
                    handles.push(
                        std::thread::Builder::new()
                            .name(format!("vol {}", v.display_root()))
                            .spawn(move || me2.run_volume(i, v))
                            .unwrap(),
                    );
                }
                me.workers.lock().extend(handles);
                if me.follow_indexer {
                    let me3 = me.clone();
                    std::thread::Builder::new().name("watch-data-dir".into()).spawn(move || me3.watch_data_dir()).ok();
                }
                // フォルダ索引
                let folders: Vec<String> = me.folders.lock().clone();
                let base = me.status.lock().volumes.len();
                for (k, f) in folders.into_iter().enumerate() {
                    me.status.lock().volumes.push(VolStatus { root: f.clone(), state: VolState::Pending, entries: 0, usn_events: 0, last_change: None });
                    let me2 = me.clone();
                    // ReadDirectoryChangesW で永久にブロックするので join はしない（終了時はプロセスごと終わる）
                    std::thread::Builder::new().name(format!("folder {f}")).spawn(move || me2.run_folder(base + k, f)).unwrap();
                }
            })
            .unwrap();
        self.workers.lock().push(h);
    }

    /// フォルダ索引のライフサイクル: 走査 → 変更監視（ReadDirectoryChangesW）→ 変更が落ち着いたら再走査して差し替え。
    fn run_folder(self: Arc<Self>, slot: usize, root: String) {
        use windows_sys::Win32::Foundation::{GENERIC_READ, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, ReadDirectoryChangesW, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_ATTRIBUTES,
            FILE_NOTIFY_CHANGE_CREATION, FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
            FILE_NOTIFY_CHANGE_SIZE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        };
        let current: Arc<Mutex<Option<SharedVolume>>> = Arc::new(Mutex::new(None));
        if self.follow_indexer {
            // インデクサが保存したフォルダ索引を読むだけ（更新は watch_data_dir が拾う）
            let path = self.folder_store_path(&root);
            self.set_state(slot, |s| s.state = VolState::Loading);
            match store::load(&path) {
                Ok(v) => {
                    let n = v.live_count();
                    let shared = Arc::new(RwLock::new(v));
                    self.index.push(shared.clone());
                    self.set_state(slot, |s| {
                        s.state = VolState::Ready;
                        s.entries = n;
                    });
                    self.mark_ready();
                    self.bump();
                    self.build_name_order(&shared);
                }
                Err(e) => self.set_state(slot, |s| s.state = VolState::Error(crate::i18n::trf("索引なし: {}", &[&e]))),
            }
            return;
        }
        let scan = |me: &Arc<Self>, current: &Mutex<Option<SharedVolume>>| {
            me.set_state(slot, |s| s.state = VolState::Scanning { done: 0, total: 1 });
            let t = Instant::now();
            let progress = |n: usize| {
                me.set_state(slot, |s| {
                    s.state = VolState::Scanning { done: 0, total: 1 };
                    s.entries = n;
                });
            };
            match accio_index::folder::scan_folder(&root, &me.shutdown, &progress) {
                Ok(mut v) => {
                    v.order_name = v.compute_name_order();
                    v.order_covers = v.len();
                    let n = v.live_count();
                    let shared = Arc::new(RwLock::new(v));
                    let mut cur = current.lock();
                    match cur.as_ref() {
                        Some(old) => me.index.replace(old, shared.clone()),
                        None => me.index.push(shared.clone()),
                    }
                    *cur = Some(shared.clone());
                    drop(cur);
                    log::info!("{root}: フォルダ走査 {:.2}s, {n} entries", t.elapsed().as_secs_f64());
                    // 別プロセスの UI が読めるように保存する
                    let path = me.folder_store_path(&root);
                    let g = shared.read();
                    if let Err(e) = store::save(&g, &path) {
                        log::warn!("{root}: フォルダ索引の保存失敗: {e}");
                    }
                    me.set_state(slot, |s| {
                        s.state = VolState::Ready;
                        s.entries = n;
                    });
                    me.mark_ready();
                    me.bump();
                }
                Err(e) => {
                    log::warn!("{root}: フォルダ走査失敗: {e}");
                    me.set_state(slot, |s| s.state = VolState::Error(format!("{e}")));
                }
            }
        };
        scan(&self, &current);
        if current.lock().is_none() {
            return;
        }
        // 定期再走査（ReadDirectoryChangesW が拾えない変更＝ネットワーク共有などのため）
        {
            let me = self.clone();
            let current = current.clone();
            let root2 = root.clone();
            std::thread::Builder::new()
                .name(format!("folder-rescan {root2}"))
                .spawn(move || {
                    let scan = |me: &Arc<Self>, current: &Mutex<Option<SharedVolume>>| {
                        let t = Instant::now();
                        if let Ok(mut v) = accio_index::folder::scan_folder(&root2, &me.shutdown, &|_| {}) {
                            v.order_name = v.compute_name_order();
                            v.order_covers = v.len();
                            let n = v.live_count();
                            let shared = Arc::new(RwLock::new(v));
                            let mut cur = current.lock();
                            if let Some(old) = cur.as_ref() {
                                me.index.replace(old, shared.clone());
                            }
                            *cur = Some(shared.clone());
                            drop(cur);
                            let path = me.folder_store_path(&root2);
                            let g = shared.read();
                            let _ = store::save(&g, &path);
                            me.set_state(slot, |s| s.entries = n);
                            me.bump();
                            log::info!("{root2}: 定期再走査 {:.2}s, {n} entries", t.elapsed().as_secs_f64());
                        }
                    };
                    let mut last = Instant::now();
                    while !me.shutdown.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_secs(5));
                        let mins = me.folder_rescan_minutes.load(Ordering::Relaxed);
                        if mins > 0 && last.elapsed() >= Duration::from_secs(mins * 60) {
                            scan(&me, &current);
                            last = Instant::now();
                        }
                    }
                })
                .ok();
        }
        // 変更監視
        let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: NUL 終端パス。ディレクトリハンドルは監視にだけ使う。
        let h = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_LIST_DIRECTORY | GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            log::warn!("{root}: 変更監視を開始できない");
            return;
        }
        let mut buf = vec![0u8; 64 << 10];
        while !self.shutdown.load(Ordering::Relaxed) {
            let mut returned = 0u32;
            // 同期呼び出し: 変更があるまでブロックする（終了時はプロセスごと終わる）
            let ok = unsafe {
                ReadDirectoryChangesW(
                    h,
                    buf.as_mut_ptr() as *mut _,
                    buf.len() as u32,
                    1,
                    FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_DIR_NAME | FILE_NOTIFY_CHANGE_ATTRIBUTES | FILE_NOTIFY_CHANGE_SIZE | FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_CREATION,
                    &mut returned,
                    std::ptr::null_mut(),
                    None,
                )
            };
            if ok == 0 {
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
            self.set_state(slot, |s| {
                s.usn_events += 1;
                s.last_change = Some(Instant::now());
            });
            // 落ち着くまで待ってから再走査（連続する変更をまとめる）
            std::thread::sleep(Duration::from_secs(2));
            scan(&self, &current);
        }
    }

    fn store_path(&self, info: &VolumeInfo) -> PathBuf {
        self.data_dir.join(format!("{:08x}.accio", info.serial))
    }

    fn folder_store_path(&self, root: &str) -> PathBuf {
        let h = accio_query::eval::hash64(accio_index::volume::lowercase(root.trim_end_matches('\\')).as_bytes());
        self.data_dir.join(format!("folder-{h:016x}.accio"))
    }

    /// 索引ファイルを読み込み、同じ guid_path のボリュームがあれば差し替え、無ければ追加する（追従モード）。
    fn reload_file(self: &Arc<Self>, path: &std::path::Path) {
        let t = Instant::now();
        let v = match store::load(path) {
            Ok(v) => v,
            Err(e) => {
                log::warn!("{}: 再読込失敗: {e}", path.display());
                return;
            }
        };
        let guid = v.meta.guid_path.clone();
        let root = v.meta.display_root.clone();
        let n = v.live_count();
        let shared = Arc::new(RwLock::new(v));
        let existing = self.index.snapshot().into_iter().find(|x| x.read().meta.guid_path == guid);
        match existing {
            Some(old) => self.index.replace(&old, shared.clone()),
            None => self.index.push(shared.clone()),
        }
        {
            let mut st = self.status.lock();
            match st.volumes.iter_mut().find(|s| s.root == root) {
                Some(s) => {
                    s.state = VolState::Ready;
                    s.entries = n;
                    s.last_change = Some(Instant::now());
                    s.usn_events += 1;
                }
                None => st.volumes.push(VolStatus { root: root.clone(), state: VolState::Ready, entries: n, usn_events: 0, last_change: Some(Instant::now()) }),
            }
        }
        log::info!("{root}: 索引ファイルを再読込 {:.2}s, {n} entries", t.elapsed().as_secs_f64());
        self.mark_ready();
        self.bump();
        self.build_name_order(&shared);
    }

    /// 追従モード: データディレクトリの *.accio の更新を監視して再読込する。
    fn watch_data_dir(self: Arc<Self>) {
        use std::collections::HashMap;
        let mut seen: HashMap<PathBuf, std::time::SystemTime> = HashMap::new();
        let list = |dir: &PathBuf| -> Vec<(PathBuf, std::time::SystemTime)> {
            std::fs::read_dir(dir)
                .map(|rd| {
                    rd.flatten()
                        .filter(|e| e.path().extension().map(|x| x == "accio").unwrap_or(false))
                        .filter_map(|e| e.metadata().ok().and_then(|m| m.modified().ok()).map(|t| (e.path(), t)))
                        .collect()
                })
                .unwrap_or_default()
        };
        for (p, t) in list(&self.data_dir) {
            seen.insert(p, t);
        }
        loop {
            std::thread::sleep(Duration::from_secs(2));
            if self.shutdown.load(Ordering::Relaxed) {
                return;
            }
            let now = list(&self.data_dir);
            for (p, t) in now {
                let changed = seen.get(&p).map(|old| *old != t).unwrap_or(true);
                if changed {
                    // 書き込み途中を避ける: 更新から 1 秒以上経っていること
                    if t.elapsed().map(|d| d < Duration::from_secs(1)).unwrap_or(false) {
                        continue;
                    }
                    seen.insert(p.clone(), t);
                    self.reload_file(&p);
                }
            }
        }
    }

    /// 1 ボリュームのライフサイクル。
    fn run_volume(self: Arc<Self>, slot: usize, info: VolumeInfo) {
        let device = info.device_path();
        if !self.elevated {
            let path = self.store_path(&info);
            self.set_state(slot, |s| s.state = VolState::Loading);
            match store::load(&path) {
                Ok(v) if v.meta.guid_path == info.guid_path => {
                    let n = v.live_count();
                    let shared = Arc::new(RwLock::new(v));
                    self.index.push(shared.clone());
                    self.set_state(slot, |s| {
                        s.state = VolState::Ready;
                        s.entries = n;
                    });
                    self.mark_ready();
                    self.bump();
                    self.build_name_order(&shared);
                }
                Ok(_) => self.set_state(slot, |s| s.state = VolState::Error(crate::i18n::tr("索引ファイルのボリューム不一致").into())),
                Err(e) => self.set_state(slot, |s| s.state = VolState::Error(crate::i18n::trf("索引なし: {}", &[&e]))),
            }
            return;
        }
        let is_refs = info.fs_name == "ReFS";
        let mut journal = match if is_refs { UsnJournal::open_refs(&device) } else { UsnJournal::open(&device) } {
            Ok(j) => j,
            Err(e) => {
                log::error!("{}: {e}", info.display_root());
                self.set_state(slot, |s| s.state = VolState::Error(format!("{e}")));
                return;
            }
        };
        let jstate = match journal.ensure(USN_MAX_SIZE, USN_ALLOC_DELTA) {
            Ok(s) => s,
            Err(e) => {
                log::error!("{}: USN: {e}", info.display_root());
                self.set_state(slot, |s| s.state = VolState::Error(format!("USN: {e}")));
                return;
            }
        };

        // 1. 保存済み索引の読み込み
        let path = self.store_path(&info);
        let mut current: Option<SharedVolume> = None;
        if path.exists() {
            self.set_state(slot, |s| s.state = VolState::Loading);
            let t = Instant::now();
            match store::load(&path) {
                Ok(v) if v.meta.guid_path == info.guid_path => {
                    let usable = v.meta.journal_id == jstate.journal_id && v.meta.next_usn >= jstate.first_usn && v.meta.next_usn <= jstate.next_usn;
                    log::info!("{}: 索引読み込み {:.2}s, {} entries, usable={}", info.display_root(), t.elapsed().as_secs_f64(), v.len(), usable);
                    if usable {
                        let shared = Arc::new(RwLock::new(v));
                        self.index.push(shared.clone());
                        let n = shared.read().live_count();
                        self.set_state(slot, |s| {
                            s.state = VolState::Ready;
                            s.entries = n;
                        });
                        self.mark_ready();
                        self.bump();
                        self.build_name_order(&shared);
                        current = Some(shared);
                    } else {
                        log::warn!("{}: USN ジャーナルが巻き戻った/変わったため再スキャン", info.display_root());
                    }
                }
                Ok(_) => log::warn!("{}: 索引ファイルのボリューム不一致", info.display_root()),
                Err(e) => log::warn!("{}: 索引読み込み失敗: {e}", info.display_root()),
            }
        }

        // ReFS: ID 変換層を用意する（保存済み索引があればその表から復元）
        if is_refs {
            let ids = current.as_ref().map(|v| IdMap::from_saved(&v.read().refs_ids)).unwrap_or_default();
            match FileIdReader::open(&info.guid_path) {
                Ok(reader) => {
                    let adapter = Arc::new(RefsAdapter::new(reader, ids));
                    journal.set_refs(adapter.clone());
                    self.refs.lock().insert(info.guid_path.clone(), adapter);
                }
                Err(e) => {
                    log::error!("{}: {e}", info.display_root());
                    self.set_state(slot, |s| s.state = VolState::Error(format!("{e}")));
                    return;
                }
            }
        }

        // 2. 無ければ（または使えなければ）フルスキャン
        if current.is_none() {
            match self.full_scan(slot, &info, &journal, None) {
                Ok(v) => {
                    // 初回スキャン直後に保存しておく（次回起動を速くし、異常終了にも備える）
                    self.save_volume(&v, &path);
                    self.build_name_order(&v);
                    current = Some(v);
                }
                Err(e) => {
                    log::error!("{}: スキャン失敗: {e}", info.display_root());
                    self.set_state(slot, |s| s.state = VolState::Error(format!("{e}")));
                    return;
                }
            }
        }

        // 3. USN 監視ループ
        let mut vol = current.unwrap();
        let mut buf = vec![0u8; 4 << 20];
        let mut last_save = Instant::now();
        let mut dirty = false;
        let mut idle_ticks = 0u32;
        let mut seen_save = self.save_requested.load(Ordering::Relaxed);
        let mut seen_reindex = self.reindex_requested.load(Ordering::Relaxed);
        while !self.shutdown.load(Ordering::Relaxed) {
            let (jid, start) = {
                let g = vol.read();
                (g.meta.journal_id, g.meta.next_usn)
            };
            let mut applied_any = false;
            loop {
                let cur = vol.read().meta.next_usn;
                match journal.read(jid, start.max(cur), &mut buf) {
                    Ok((next, events)) => {
                        if events.is_empty() {
                            vol.write().meta.next_usn = next;
                            break;
                        }
                        // レコード読み（ioctl）をロックの外で先に済ませ、書き込みロックの時間を短くする
                        let t_batch = Instant::now();
                        let prefetched = PrefetchedRecords::new(&journal, &events, &vol);
                        let t_prefetch = t_batch.elapsed();
                        let st = {
                            let mut jr = self.journal.lock();
                            update::apply_with_journal(&vol, &prefetched, &events, &mut |c| jr.push(c))
                        };
                        if t_batch.elapsed() > Duration::from_millis(500) {
                            log::info!("{}: USN バッチ {} 件: 先読み {:.2}s, 反映 {:.2}s", info.display_root(), events.len(), t_prefetch.as_secs_f64(), (t_batch.elapsed() - t_prefetch).as_secs_f64());
                        }
                        {
                            let mut g = vol.write();
                            g.meta.next_usn = next;
                        }
                        let n = (st.created + st.deleted + st.renamed + st.updated) as u64;
                        if st.record_miss > 0 {
                            log::warn!("{}: MFT レコード再読み取り失敗 {} 件 (USN 反映 {:?})", info.display_root(), st.record_miss, st);
                        }
                        if n > 0 {
                            applied_any = true;
                            dirty = true;
                            let live = vol.read().live_count();
                            self.set_state(slot, |s| {
                                s.usn_events += n;
                                s.entries = live;
                                s.last_change = Some(Instant::now());
                            });
                        }
                        if events.len() < 1000 {
                            break;
                        }
                    }
                    Err(accio_ntfs::usn::ERROR_JOURNAL_ENTRY_DELETED) | Err(accio_ntfs::usn::ERROR_JOURNAL_NOT_ACTIVE) => {
                        log::warn!("{}: USN ジャーナルが巻き戻った。再スキャンします", info.display_root());
                        match self.full_scan(slot, &info, &journal, Some(&vol)) {
                            Ok(v) => {
                                self.build_name_order(&v);
                                vol = v;
                                dirty = true;
                            }
                            Err(e) => {
                                self.set_state(slot, |s| s.state = VolState::Error(format!("{e}")));
                                return;
                            }
                        }
                        break;
                    }
                    Err(e) => {
                        // ジャーナルが作り直された（ID 変更）/ 削除中 / ボリュームが一時的に使えない
                        match journal.query() {
                            Ok(js) if js.journal_id != jid => {
                                log::warn!("{}: USN ジャーナルの ID が変わった。再スキャンします", info.display_root());
                                match self.full_scan(slot, &info, &journal, Some(&vol)) {
                                    Ok(v) => {
                                        self.build_name_order(&v);
                                        vol = v;
                                        dirty = true;
                                    }
                                    Err(e) => log::error!("{}: 再スキャン失敗: {e}", info.display_root()),
                                }
                            }
                            _ => {
                                log::warn!("{}: USN 読み取りエラー {e}", info.display_root());
                                std::thread::sleep(Duration::from_secs(5));
                            }
                        }
                        break;
                    }
                }
            }
            if applied_any {
                self.bump();
                idle_ticks = 0;
            } else {
                idle_ticks = idle_ticks.saturating_add(1);
            }
            let want_save = self.save_requested.load(Ordering::Relaxed);
            if want_save != seen_save {
                seen_save = want_save;
                self.save_volume(&vol, &path);
                last_save = Instant::now();
                dirty = false;
            }
            let want_reindex = self.reindex_requested.load(Ordering::Relaxed);
            if want_reindex != seen_reindex {
                seen_reindex = want_reindex;
                log::info!("{}: 再索引の要求", info.display_root());
                match self.full_scan(slot, &info, &journal, Some(&vol)) {
                    Ok(v) => {
                        self.save_volume(&v, &path);
                        self.build_name_order(&v);
                        vol = v;
                        dirty = false;
                        last_save = Instant::now();
                    }
                    Err(e) => log::error!("{}: 再索引失敗: {e}", info.display_root()),
                }
            }
            // 静かになったら保存。C: のように変更が止まらないボリュームでも 5 分に 1 回は保存する
            // （保存が無いと次回起動で USN が巻き戻ったとみなしてフルスキャンになる）
            let overdue = dirty && last_save.elapsed() > Duration::from_secs(300);
            if (dirty && last_save.elapsed() > self.save_settle && idle_ticks > 10) || overdue {
                self.save_volume(&vol, &path);
                last_save = Instant::now();
                dirty = false;
                let (n, covers) = {
                    let g = vol.read();
                    (g.len(), g.order_covers)
                };
                if n - covers > 10_000 || (n - covers) * 100 > n {
                    self.build_name_order(&vol);
                }
            }
            // 変化が続いている間は細かく、静かなら緩く
            let wait = if idle_ticks < 3 { 250 } else { 1000 };
            std::thread::sleep(Duration::from_millis(wait));
        }
        if dirty {
            self.save_volume(&vol, &path);
        }
    }

    /// 名前順の順列をバックグラウンドで計算して格納する（ソート中は読み取りロックのみ）。
    fn build_name_order(&self, vol: &SharedVolume) {
        let t = Instant::now();
        let (order, n) = {
            let g = vol.read();
            (g.compute_name_order(), g.len())
        };
        let mut g = vol.write();
        g.order_name = order;
        g.order_covers = n;
        log::info!("{}: 名前順の順列 {} 件 {:.2}s", g.meta.display_root, n, t.elapsed().as_secs_f64());
        drop(g);
        self.bump();
    }

    fn mark_ready(&self) {
        self.db_loaded.store(true, Ordering::Relaxed);
        let mut st = self.status.lock();
        if st.startup_ready_at.is_none() {
            st.startup_ready_at = Some(self.started.elapsed());
        }
    }

    fn save_volume(&self, vol: &SharedVolume, path: &std::path::Path) {
        let t = Instant::now();
        // 書き込みロックは compact（溢れた名前があるときだけ）と ID 表の書き戻しの間だけ。
        // 列の複製は読み取りロック（0.3 秒程度）、圧縮と書き出し（数秒）はロック外で行う
        let snap = {
            let mut g = vol.write();
            if !g.overflow.is_empty() {
                g.compact();
            }
            if let Some(a) = self.refs.lock().get(&g.meta.guid_path) {
                g.refs_ids = a.ids.lock().unwrap_or_else(|e| e.into_inner()).to_saved();
            }
            let g = parking_lot::RwLockWriteGuard::downgrade(g);
            store::Snapshot::take(&g)
        };
        let root = vol.read().meta.display_root.clone();
        match snap.write(path) {
            Ok(()) => log::info!("{root}: 索引保存 {:.2}s", t.elapsed().as_secs_f64()),
            Err(e) => log::error!("{root}: 索引保存失敗: {e}"),
        }
    }

    /// MFT をフルスキャンして新しいボリューム索引を作り、`replace_target` があれば差し替える。
    /// スキャン中も新索引は検索対象に入る（到着済み分だけヒットする）。
    fn full_scan(&self, slot: usize, info: &VolumeInfo, journal: &UsnJournal, replace_target: Option<&SharedVolume>) -> anyhow::Result<SharedVolume> {
        if info.fs_name == "ReFS" {
            return self.refs_scan(slot, info, journal, replace_target);
        }
        let reader = FileReader::open_volume(&info.device_path())?;
        let jstate = journal.query().map_err(|e| anyhow::anyhow!("USN query failed: {e}"))?;
        // スキャン開始時点の USN を記録し、スキャン中の変更はあとで再生する
        let start_usn = jstate.next_usn;
        let mft_records = {
            let mut sector = [0u8; 512];
            reader.read_at(0, &mut sector)?;
            let boot = accio_ntfs::boot::BootInfo::parse(&sector)?;
            let mut rec0 = vec![0u8; boot.bytes_per_record as usize];
            reader.read_at(boot.mft_lcn * boot.bytes_per_cluster as u64, &mut rec0)?;
            let mut size = 0u64;
            if accio_ntfs::mft::fixup(&mut rec0) {
                accio_ntfs::mft::for_each_attribute(&rec0, |ty, a| {
                    if ty == accio_ntfs::mft::ATTR_DATA && a[9] == 0 && a[8] != 0 {
                        size = u64::from_le_bytes(a[48..56].try_into().unwrap());
                    }
                });
            }
            (size / boot.bytes_per_record as u64).max(16) + 1024
        };
        let meta = VolumeMeta {
            guid_path: info.guid_path.clone(),
            display_root: info.display_root(),
            serial: info.serial,
            journal_id: jstate.journal_id,
            next_usn: start_usn,
            mft_records,
        };
        let builder = VolumeBuilder::new(meta, mft_records);
        let shared = builder.vol.clone();
        match replace_target {
            Some(old) => self.index.replace(old, shared.clone()),
            None => self.index.push(shared.clone()),
        }
        self.mark_ready();
        self.set_state(slot, |s| s.state = VolState::Scanning { done: 0, total: 1 });
        let t = Instant::now();
        log::info!("{}: MFT スキャン開始 ({} records)", info.display_root(), mft_records);
        let last_report = Mutex::new(Instant::now());
        let last_log = Mutex::new(Instant::now());
        let cancel = self.shutdown.clone();
        let progress = |done: u64, total: u64| {
            let mut lr = last_report.lock();
            if lr.elapsed() > Duration::from_millis(200) || done == total {
                *lr = Instant::now();
                drop(lr);
                let mut ll = last_log.lock();
                if ll.elapsed() > Duration::from_secs(10) {
                    *ll = Instant::now();
                    log::info!("{}: スキャン {:.0}%", info.display_root(), done as f64 / total.max(1) as f64 * 100.0);
                }
                drop(ll);
                let n = shared.read().live_count();
                self.set_state(slot, |s| {
                    s.state = VolState::Scanning { done, total };
                    s.entries = n;
                });
                self.bump();
            }
        };
        let stats = scan_mft(&reader, &ScanOptions::default(), &cancel, &progress, &|batch| builder.push_batch(batch))?;
        let vol = builder.finish();
        let n = vol.read().live_count();
        log::info!("{}: MFT スキャン {:.2}s, {} records → {} entries", info.display_root(), t.elapsed().as_secs_f64(), stats.total_records, n);
        self.set_state(slot, |s| {
            s.state = VolState::Ready;
            s.entries = n;
        });
        self.bump();
        let _ = ft_now();
        Ok(vol)
    }

    /// ReFS: ディレクトリ列挙（ファイル ID・サイズ・日時込み）で索引を組む。更新は USN ジャーナル（V3）で追う。
    /// `FSCTL_ENUM_USN_DATA` は ReFS では使えない（error 1）。
    fn refs_scan(&self, slot: usize, info: &VolumeInfo, journal: &UsnJournal, replace_target: Option<&SharedVolume>) -> anyhow::Result<SharedVolume> {
        let adapter = journal.refs().cloned().ok_or_else(|| anyhow::anyhow!(crate::i18n::tr("ReFS の変換層が無い")))?;
        let jstate = journal.query().map_err(|e| anyhow::anyhow!("USN query failed: {e}"))?;
        let start_usn = jstate.next_usn;
        self.set_state(slot, |s| s.state = VolState::Scanning { done: 0, total: 1 });
        let t = Instant::now();
        let mut records = Vec::new();
        let cancel = || self.shutdown.load(Ordering::Relaxed);
        let progress = |n: usize| {
            self.set_state(slot, |s| {
                s.state = VolState::Scanning { done: 0, total: 1 };
                s.entries = n;
            });
        };
        let root_id = accio_ntfs::enumerate::walk_with_ids(&info.guid_path, &cancel, &progress, |r| records.push(r))?;
        log::info!("{}: ディレクトリ列挙 {} 件 {:.2}s", info.display_root(), records.len(), t.elapsed().as_secs_f64());
        let mut ids = IdMap::default();
        let (entries, _) = to_mft_entries_with_root(&records, &mut ids, Some(root_id));
        drop(records);
        let mft_records = ids.len() as u64 + 1024;
        let meta = VolumeMeta {
            guid_path: info.guid_path.clone(),
            display_root: info.display_root(),
            serial: info.serial,
            journal_id: jstate.journal_id,
            next_usn: start_usn,
            mft_records,
        };
        let builder = VolumeBuilder::new(meta, mft_records);
        let shared = builder.vol.clone();
        match replace_target {
            Some(old) => self.index.replace(old, shared.clone()),
            None => self.index.push(shared.clone()),
        }
        *adapter.ids.lock().unwrap_or_else(|e| e.into_inner()) = ids;
        let total = entries.len() as u64;
        let mut done = 0u64;
        for chunk in entries.chunks(100_000) {
            if cancel() {
                anyhow::bail!(crate::i18n::tr("中断"));
            }
            builder.push_batch(chunk.to_vec());
            done += chunk.len() as u64;
            let live = shared.read().live_count();
            self.set_state(slot, |s| {
                s.state = VolState::Scanning { done, total: total.max(1) };
                s.entries = live;
            });
            self.bump();
        }
        let vol = builder.finish();
        self.mark_ready();
        let live = vol.read().live_count();
        self.set_state(slot, |s| {
            s.state = VolState::Ready;
            s.entries = live;
        });
        self.bump();
        log::info!("{}: ReFS 索引 {} entries {:.2}s", info.display_root(), live, t.elapsed().as_secs_f64());
        Ok(vol)
    }

    pub fn stop_and_save(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
        let handles: Vec<_> = self.workers.lock().drain(..).collect();
        for h in handles {
            let _ = h.join();
        }
    }
}
