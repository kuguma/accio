//! 検索ワーカー。UI から最新のリクエストだけを受け取り、古い検索は世代番号でキャンセルする。
//! 結果は ArcSwap で公開し、UI は毎フレーム読むだけ。

use accio_index::Index;
use accio_query::{search, Request, Response};
use arc_swap::ArcSwapOption;
use parking_lot::{Condvar, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub struct Searcher {
    pending: Mutex<Option<Request>>,
    cv: Condvar,
    gen: AtomicU64,
    pub result: ArcSwapOption<Response>,
    /// 公開中の結果を生んだ (リクエスト世代, 索引世代)
    pub result_gen: AtomicU64,
    pub busy: AtomicU64,
}

impl Searcher {
    pub fn spawn(index: Arc<Index>, index_gen: Arc<AtomicU64>, repaint: Arc<dyn Fn() + Send + Sync>) -> Arc<Self> {
        let me = Arc::new(Self {
            pending: Mutex::new(None),
            cv: Condvar::new(),
            gen: AtomicU64::new(0),
            result: ArcSwapOption::empty(),
            result_gen: AtomicU64::new(0),
            busy: AtomicU64::new(0),
        });
        let worker = me.clone();
        std::thread::Builder::new()
            .name("search".into())
            .spawn(move || loop {
                let req = {
                    let mut p = worker.pending.lock();
                    while p.is_none() {
                        worker.cv.wait(&mut p);
                    }
                    p.take().unwrap()
                };
                let my_gen = worker.gen.load(Ordering::Acquire);
                worker.busy.store(1, Ordering::Relaxed);
                let vols = index.snapshot();
                let idx_gen = index_gen.load(Ordering::Relaxed);
                let cancel = || worker.gen.load(Ordering::Acquire) != my_gen;
                let req = Request { index_generation: idx_gen.max(1), ..req };
                // 検索の panic で検索スレッドを失わない（索引の不整合は検索結果なしとして扱い、ログに残る）
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| search(&vols, &req, &cancel)));
                if let Ok(Some(r)) = outcome {
                    worker.result.store(Some(Arc::new(r)));
                    worker.result_gen.store(idx_gen, Ordering::Relaxed);
                }
                worker.busy.store(0, Ordering::Relaxed);
                repaint();
            })
            .unwrap();
        me
    }

    /// 最新リクエストに置き換える。実行中の古い検索は次のチャンク境界で打ち切られる。
    pub fn submit(&self, req: Request) {
        self.gen.fetch_add(1, Ordering::AcqRel);
        *self.pending.lock() = Some(req);
        self.cv.notify_one();
    }
}
