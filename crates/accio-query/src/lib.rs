//! 検索層。クエリ文字列 → 構文木 → 各ボリュームでの並列評価 → ソート済みヒット列。
//!
//! 責務境界: 索引の内部表現には読み取りでしか触れない。UI やインデックス更新は関知しない。

pub mod bitset;
pub mod datetime;
pub mod eval;
pub mod parser;

pub use eval::{run_history_get, set_run_history, set_type_name_provider, RunHistorySnapshot, SearchOpts};

/// すべての検索に前置される式（除外設定: `!attrib:H !wfn:"*.tmp" …`）。Everything の除外は索引段階で
/// 効くので、UI だけでなく IPC/HTTP/ETP の検索にも同じように効かせる。
static SEARCH_PREFIX: std::sync::RwLock<String> = std::sync::RwLock::new(String::new());

pub fn set_search_prefix(prefix: String) {
    *SEARCH_PREFIX.write().unwrap_or_else(|e| e.into_inner()) = prefix;
}

pub fn search_prefix() -> String {
    SEARCH_PREFIX.read().unwrap_or_else(|e| e.into_inner()).clone()
}

use accio_index::SharedVolume;
use rayon::prelude::*;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    #[default]
    Name,
    Path,
    Size,
    DateModified,
    DateCreated,
    DateAccessed,
    Extension,
    /// 実行回数（UI 側の実行履歴で並べ替える。検索層では名前順）
    RunCount,
    /// 最終実行日時（実行履歴）。検索層では名前順にし、並べ替えは UI 側（RunCount と同じ）
    DateRun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sort {
    pub key: SortKey,
    pub ascending: bool,
}

impl Default for Sort {
    fn default() -> Self {
        Self { key: SortKey::Name, ascending: true }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Request {
    pub query: String,
    pub opts: SearchOpts,
    pub sort: Sort,
    /// 索引の世代。同じ世代なら重複検出の前計算をキャッシュから使う（0 はキャッシュしない）
    pub index_generation: u64,
}

/// 重複検出の前計算キャッシュ: (世代, キー) → ソート済みハッシュ列
type DupeCacheEntry = ((u64, parser::DupeKey), std::sync::Arc<Vec<u64>>);
static DUPE_CACHE: parking_lot::Mutex<Vec<DupeCacheEntry>> = parking_lot::Mutex::new(Vec::new());

/// 並べ替え用の順位表（ボリュームごと）。鍵は (ボリュームの Arc アドレス, 版)。
/// - 名前順位: `order_name` の逆順列。`order_covers` 以降（追記分）は u32::MAX で、名前比較に落とす
/// - パス順位: フォルダ木を名前順で先行順走査した番号。フォルダ構造が変わった（dir_epoch）ときだけ作り直す
type RankCacheEntry = (usize, u64, std::sync::Arc<Vec<u32>>);
static NAME_RANK_CACHE: parking_lot::Mutex<Vec<RankCacheEntry>> = parking_lot::Mutex::new(Vec::new());
static DIR_RANK_CACHE: parking_lot::Mutex<Vec<RankCacheEntry>> = parking_lot::Mutex::new(Vec::new());

fn cached_rank(cache: &parking_lot::Mutex<Vec<RankCacheEntry>>, key: usize, ver: u64, build: impl FnOnce() -> Vec<u32>) -> std::sync::Arc<Vec<u32>> {
    if let Some((_, _, r)) = cache.lock().iter().find(|(k, v, _)| *k == key && *v == ver) {
        return r.clone();
    }
    let r = std::sync::Arc::new(build());
    let mut c = cache.lock();
    c.retain(|(k, _, _)| *k != key);
    c.push((key, ver, r.clone()));
    if c.len() > 16 {
        c.remove(0);
    }
    r
}

fn name_rank(g: &accio_index::VolumeIndex, key: usize) -> std::sync::Arc<Vec<u32>> {
    let ver = (g.order_name.len() as u64) << 32 | g.order_covers as u64;
    cached_rank(&NAME_RANK_CACHE, key, ver, || {
        let mut r = vec![u32::MAX; g.len()];
        for (i, &e) in g.order_name.iter().enumerate() {
            if (e as usize) < r.len() {
                r[e as usize] = i as u32;
            }
        }
        r
    })
}

/// フォルダの先行順（名前順）の順位。ファイルは u32::MAX。
fn dir_rank(g: &accio_index::VolumeIndex, key: usize) -> std::sync::Arc<Vec<u32>> {
    // 同じアドレスに別のボリュームが載った場合に備えて長さも鍵に混ぜる
    cached_rank(&DIR_RANK_CACHE, key, g.dir_epoch ^ ((g.len() as u64) << 40), || {
        let n = g.len();
        // 生きているフォルダを (親, 小文字名, 自分) で並べる → 同じ親の子が名前順に連続する
        let mut dirs: Vec<u32> = (1..n as u32).filter(|&i| g.is_dir(i) && !g.is_deleted(i) && g.parent.get(i as usize).copied().filter(|p| (*p as usize) < g.len()).unwrap_or(accio_index::NONE) != accio_index::NONE).collect();
        dirs.par_sort_unstable_by(|&a, &b| g.parent.get(a as usize).copied().filter(|p| (*p as usize) < g.len()).unwrap_or(accio_index::NONE).cmp(&g.parent.get(b as usize).copied().filter(|p| (*p as usize) < g.len()).unwrap_or(accio_index::NONE)).then_with(|| g.lower_name(a).cmp(g.lower_name(b))).then(a.cmp(&b)));
        // 親ごとの子の範囲
        let mut first = vec![u32::MAX; n];
        let mut count = vec![0u32; n];
        for (pos, &d) in dirs.iter().enumerate() {
            let p = g.parent.get(d as usize).copied().filter(|p| (*p as usize) < g.len()).unwrap_or(accio_index::NONE) as usize;
            if first[p] == u32::MAX {
                first[p] = pos as u32;
            }
            count[p] += 1;
        }
        let mut rank = vec![u32::MAX; n];
        if n == 0 {
            return rank;
        }
        let mut next = 0u32;
        let mut stack: Vec<(u32, u32)> = vec![(0, 0)]; // (フォルダ, 次に訪ねる子の位置)
        rank[0] = next;
        next += 1;
        while let Some(&mut (d, ref mut k)) = stack.last_mut() {
            let du = d as usize;
            if first[du] == u32::MAX || *k >= count[du] {
                stack.pop();
                continue;
            }
            let child = dirs[(first[du] + *k) as usize];
            *k += 1;
            if rank[child as usize] == u32::MAX {
                rank[child as usize] = next;
                next += 1;
                stack.push((child, 0));
            }
        }
        rank
    })
}

/// 全ボリューム横断の名前順（(ボリューム番号 << 32 | エントリ) の列）。鍵は各ボリュームの (Arc アドレス, 順列の版)。
type GlobalOrderKey = Vec<(usize, u64)>;
static GLOBAL_NAME_ORDER: parking_lot::Mutex<Option<(GlobalOrderKey, std::sync::Arc<Vec<u64>>)>> = parking_lot::Mutex::new(None);

fn global_name_order(keys: &[(usize, u64)], vols: &[&accio_index::VolumeIndex]) -> std::sync::Arc<Vec<u64>> {
    if let Some((k, v)) = GLOBAL_NAME_ORDER.lock().as_ref() {
        if k.as_slice() == keys {
            return v.clone();
        }
    }
    let t = Instant::now();
    let total: usize = vols.iter().map(|g| g.order_name.len()).sum();
    let mut merged: Vec<u64> = Vec::with_capacity(total);
    let nonempty: Vec<usize> = (0..vols.len()).filter(|&vi| !vols[vi].order_name.is_empty()).collect();
    if nonempty.len() == 1 {
        let vi = nonempty[0];
        merged.extend(vols[vi].order_name.iter().map(|&e| ((vi as u64) << 32) | e as u64));
    } else {
        struct Head<'a> {
            name: &'a [u8],
            vi: usize,
            pos: usize,
        }
        impl PartialEq for Head<'_> {
            fn eq(&self, o: &Self) -> bool {
                self.name == o.name && self.vi == o.vi
            }
        }
        impl Eq for Head<'_> {}
        impl PartialOrd for Head<'_> {
            fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
                Some(self.cmp(o))
            }
        }
        impl Ord for Head<'_> {
            fn cmp(&self, o: &Self) -> std::cmp::Ordering {
                // BinaryHeap は最大ヒープなので逆順にして最小を取り出す
                o.name.cmp(self.name).then_with(|| o.vi.cmp(&self.vi))
            }
        }
        let mut heap = std::collections::BinaryHeap::with_capacity(nonempty.len());
        for &vi in &nonempty {
            heap.push(Head { name: vols[vi].lower_name(vols[vi].order_name[0]), vi, pos: 0 });
        }
        while let Some(Head { vi, pos, .. }) = heap.pop() {
            merged.push(((vi as u64) << 32) | vols[vi].order_name[pos] as u64);
            let next = pos + 1;
            if next < vols[vi].order_name.len() {
                heap.push(Head { name: vols[vi].lower_name(vols[vi].order_name[next]), vi, pos: next });
            }
        }
    }
    log::info!("名前順の全体マージ {} 件 {:.2}s", merged.len(), t.elapsed().as_secs_f64());
    let arc = std::sync::Arc::new(merged);
    *GLOBAL_NAME_ORDER.lock() = Some((keys.to_vec(), arc.clone()));
    arc
}

/// 拡張子の先頭 8 バイトを整数化（比較を速くする）。
fn ext_key(e: &[u8]) -> u64 {
    let mut b = [0u8; 8];
    let n = e.len().min(8);
    b[..n].copy_from_slice(&e[..n]);
    u64::from_be_bytes(b)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Hit {
    pub vol: u16,
    pub id: u32,
}

#[derive(Debug, Default)]
pub struct Response {
    pub hits: Vec<Hit>,
    pub total: usize,
    pub files: usize,
    pub folders: usize,
    pub warnings: Vec<String>,
    pub elapsed_ms: f64,
    pub sorted: bool,
}

/// 結果のハイライト用に、クエリ中の肯定的な文字列項（小文字化済み、ワイルドカード/正規表現を除く）を返す。
pub fn highlight_needles(query: &str) -> Vec<String> {
    use parser::{Kind, Node};
    fn walk(n: &Node, out: &mut Vec<String>) {
        match n {
            Node::And(v) | Node::Or(v) => v.iter().for_each(|x| walk(x, out)),
            Node::Not(_) | Node::All => {}
            Node::Term(t) => {
                if let Kind::Text(s) = &t.kind {
                    if t.mods.regex != Some(true) && !s.contains('*') && !s.contains('?') && !s.is_empty() {
                        let tail = s.rsplit('\\').next().unwrap_or(s);
                        if !tail.is_empty() {
                            out.push(accio_index::volume::lowercase(tail));
                        }
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(&parser::parse(query).root, &mut out);
    out.sort();
    out.dedup();
    out
}


pub fn search(volumes: &[SharedVolume], req: &Request, cancel: eval::Cancel) -> Option<Response> {
    let t0 = Instant::now();
    let prefix = search_prefix();
    let parsed = if prefix.is_empty() { parser::parse(&req.query) } else { parser::parse(&format!("{prefix} {}", req.query)) };
    let limit = eval::count_limit(&parsed.root).map(|c| c as usize);

    let guards: Vec<_> = volumes.iter().map(|v| v.read()).collect();
    let t_lock = t0.elapsed();
    // 重複検出は全ボリューム横断なので、評価の前にキーごとのハッシュ集合を作る
    let mut ctx = eval::EvalCtx::default();
    let mut keys = Vec::new();
    eval::dupe_keys(&parsed.root, &mut keys);
    for key in keys {
        if req.index_generation != 0 {
            if let Some((_, set)) = DUPE_CACHE.lock().iter().find(|(k, _)| *k == (req.index_generation, key)) {
                ctx.dupes.insert(key, set.clone());
                continue;
            }
        }
        let per: Vec<Vec<u64>> = guards
            .par_iter()
            .map(|g| {
                (0..g.len())
                    .filter(|&i| g.flags[i] & accio_index::FLAG_DELETED == 0 && (g.attrs[i] & 0x10 == 0 || key == parser::DupeKey::Name || key == parser::DupeKey::NamePart))
                    .map(|i| eval::dupe_hash(g, i, key))
                    .collect()
            })
            .collect();
        if cancel() {
            return None;
        }
        let mut all: Vec<u64> = per.into_iter().flatten().collect();
        all.par_sort_unstable();
        // 2 回以上現れる値だけを残す（隣接比較）
        let mut dup: Vec<u64> = Vec::new();
        for w in all.windows(2) {
            if w[0] == w[1] && dup.last() != Some(&w[0]) {
                dup.push(w[0]);
            }
        }
        let set = std::sync::Arc::new(dup);
        if req.index_generation != 0 {
            let mut c = DUPE_CACHE.lock();
            c.retain(|(k, _)| k.0 == req.index_generation);
            c.push(((req.index_generation, key), set.clone()));
        }
        ctx.dupes.insert(key, set);
    }
    let t_eval0 = Instant::now();
    let per_vol: Vec<Option<bitset::Bitset>> = guards
        .par_iter()
        .enumerate()
        .map(|(vi, g)| {
            let mut c = ctx.clone();
            c.fsi = vi as u32;
            eval::eval(&parsed.root, g, &req.opts, &c, cancel)
        })
        .collect();
    if per_vol.iter().any(|b| b.is_none()) {
        return None;
    }
    let mut hits: Vec<Hit> = Vec::new();
    let mut files = 0;
    let mut folders = 0;
    for (vi, b) in per_vol.iter().enumerate() {
        let b = b.as_ref().unwrap();
        let g = &guards[vi];
        hits.reserve(b.count());
        for i in b.iter_ones() {
            if g.is_dir(i as u32) {
                folders += 1;
            } else {
                files += 1;
            }
            hits.push(Hit { vol: vi as u16, id: i as u32 });
        }
    }
    if cancel() {
        return None;
    }
    let total = hits.len();
    let t_sort0 = Instant::now();
    // 件数に関わらず常に並べ替える（名前順は順列で O(N)、他は並列ソート、パス順は親フォルダの順位付け）
    let sorted = true;
    {
        let asc = req.sort.ascending;
        match req.sort.key {
            SortKey::Name if hits.len() > 200_000 && guards.iter().all(|g| !g.order_name.is_empty()) => {
                // 全ボリューム横断の名前順（各ボリュームの順列のマージ）はキャッシュし、ヒット集合で絞るだけにする。
                // マージ自体は 1960 万件で約 4 秒かかるので検索ごとにはやらない（順列が変わったときだけ）
                let keys: Vec<(usize, u64)> = volumes
                    .iter()
                    .zip(&guards)
                    .map(|(v, g)| (std::sync::Arc::as_ptr(v) as usize, ((g.order_name.len() as u64) << 32) | g.order_covers as u64))
                    .collect();
                let refs: Vec<&accio_index::VolumeIndex> = guards.iter().map(|g| &**g).collect();
                let global = global_name_order(&keys, &refs);
                let mut out: Vec<Hit> = Vec::with_capacity(total);
                for &packed in global.iter() {
                    let vi = (packed >> 32) as usize;
                    let id = packed as u32;
                    if let Some(Some(b)) = per_vol.get(vi) {
                        if (id as usize) < b.len && b.get(id as usize) {
                            out.push(Hit { vol: vi as u16, id });
                        }
                    }
                }
                // 順列に含まれない追記分（少数）: 名前で並べ、二分探索で挿入位置を求めて 1 回で合流させる
                let mut tail: Vec<Hit> = Vec::new();
                for (vi, g) in guards.iter().enumerate() {
                    if let Some(b) = per_vol[vi].as_ref() {
                        for i in g.order_covers..g.len() {
                            if b.get(i) {
                                tail.push(Hit { vol: vi as u16, id: i as u32 });
                            }
                        }
                    }
                }
                if !tail.is_empty() {
                    let name = |h: &Hit| guards[h.vol as usize].lower_name(h.id);
                    tail.par_sort_unstable_by(|a, c| name(a).cmp(name(c)));
                    let positions: Vec<usize> = tail.iter().map(|t| out.partition_point(|o| name(o) <= name(t))).collect();
                    let mut merged = Vec::with_capacity(out.len() + tail.len());
                    let mut last = 0usize;
                    for (t, &pos) in tail.iter().zip(&positions) {
                        merged.extend_from_slice(&out[last..pos]);
                        merged.push(*t);
                        last = pos;
                    }
                    merged.extend_from_slice(&out[last..]);
                    out = merged;
                }
                hits = out;
                if !asc {
                    hits.reverse();
                }
            }
            SortKey::Name | SortKey::RunCount | SortKey::DateRun => hits.par_sort_unstable_by(|a, b| {
                let o = guards[a.vol as usize].lower_name(a.id).cmp(guards[b.vol as usize].lower_name(b.id));
                if asc { o } else { o.reverse() }
            }),
            SortKey::Size | SortKey::DateModified | SortKey::DateCreated | SortKey::DateAccessed | SortKey::Extension | SortKey::Path => {
                // 主キーを整数にし、同値は名前順位（順列の逆写像）で決める。順位が無い追記分だけ名前を比較する
                let keys: Vec<usize> = volumes.iter().map(|v| std::sync::Arc::as_ptr(v) as usize).collect();
                let nr: Vec<std::sync::Arc<Vec<u32>>> = guards.iter().zip(&keys).map(|(g, k)| name_rank(g, *k)).collect();
                let dr: Vec<std::sync::Arc<Vec<u32>>> = if req.sort.key == SortKey::Path {
                    guards.par_iter().zip(&keys).map(|(g, k)| dir_rank(g, *k)).collect()
                } else {
                    Vec::new()
                };
                if cancel() {
                    return None;
                }
                let key_of = |h: &Hit| -> (u64, u16, u32) {
                    let g = &guards[h.vol as usize];
                    let i = h.id as usize;
                    let primary = match req.sort.key {
                        SortKey::Size => g.size[i],
                        SortKey::DateModified => g.mtime_s[i] as u64,
                        SortKey::DateCreated => g.ctime_s[i] as u64,
                        SortKey::DateAccessed => g.atime_s[i] as u64,
                        SortKey::Extension => ext_key(g.ext(h.id)),
                        _ => {
                            let p = g.parent.get(i).copied().filter(|p| (*p as usize) < g.len()).unwrap_or(accio_index::NONE);
                            // パス順はボリューム（ドライブ）が先、その中でフォルダの先行順
                            // ルート自身は 0、ルート直下は 1、… と親の順位 +1（親フォルダが子より前に来る）
                            let r = if p == accio_index::NONE { 0 } else { dr[h.vol as usize][p as usize] as u64 + 1 };
                            (h.vol as u64) << 32 | r
                        }
                    };
                    (primary, h.vol, nr[h.vol as usize].get(i).copied().unwrap_or(u32::MAX))
                };
                let mut keyed: Vec<((u64, u16, u32), Hit)> = hits.par_iter().map(|h| (key_of(h), *h)).collect();
                let is_ext = req.sort.key == SortKey::Extension;
                keyed.par_sort_unstable_by(|a, b| {
                    let o = a.0.cmp(&b.0).then_with(|| {
                        let (ga, gb) = (&guards[a.1.vol as usize], &guards[b.1.vol as usize]);
                        let mut o = std::cmp::Ordering::Equal;
                        if is_ext && a.0 .0 == b.0 .0 {
                            o = ga.ext(a.1.id).cmp(gb.ext(b.1.id));
                        }
                        if o == std::cmp::Ordering::Equal && a.0 .2 == u32::MAX {
                            o = ga.lower_name(a.1.id).cmp(gb.lower_name(b.1.id));
                        }
                        o
                    });
                    if asc { o } else { o.reverse() }
                });
                hits = keyed.into_iter().map(|(_, h)| h).collect();
            }
        }
    }
    if t0.elapsed() > std::time::Duration::from_secs(1) {
        log::info!(
            "検索が遅い: {:?} {:?} {} 件: ロック待ち {:.2}s, 評価+収集 {:.2}s, 並べ替え {:.2}s",
            req.query,
            req.sort.key,
            total,
            t_lock.as_secs_f64(),
            t_sort0.duration_since(t_eval0).as_secs_f64(),
            t_sort0.elapsed().as_secs_f64()
        );
    }
    if let Some(l) = limit {
        hits.truncate(l);
    }
    Some(Response {
        hits,
        total,
        files,
        folders,
        warnings: parsed.warnings,
        elapsed_ms: t0.elapsed().as_secs_f64() * 1000.0,
        sorted,
    })
}
