//! インメモリ索引。
//!
//! 責務: ファイル名・親参照・サイズ・日付・属性をカラム指向で保持し、
//! 検索層へ「小文字化された連続名前アリーナ」と各カラムへの読み取りアクセスを提供する。
//! 構築（MFT エントリ列からの組み立て）、永続化、USN 差分適用、フォルダサイズ集計もここ。
//!
//! 不変条件:
//! - エントリ 0 はボリュームルートで、名前は表示用ルート（`C:`）。
//! - `name_off` は主アリーナ内で単調増加（追記のみ）。伸びるリネームは `overflow` に逃がす。
//! - ディレクトリの `size` は配下の合計（USN 適用時も維持）。

pub mod builder;
pub mod filelist;
pub mod folder;
pub mod store;
pub mod update;
pub mod volume;

pub use builder::VolumeBuilder;
pub use volume::{EntryId, VolumeIndex, VolumeMeta, FLAG_DELETED, NONE};

use parking_lot::RwLock;
use std::sync::Arc;

pub type SharedVolume = Arc<RwLock<VolumeIndex>>;

/// 全ボリュームの索引。ボリューム一覧は検出・再スキャンに伴って入れ替わるので RwLock で持つ。
/// 各ボリュームも RwLock（書き込みは構築中の追記と USN 適用の短い区間のみ）。
#[derive(Default)]
pub struct Index {
    volumes: RwLock<Vec<SharedVolume>>,
    /// ファイルリストを開いている間はこちらが検索対象になる（実ボリュームの更新は裏で続く）
    overlay: RwLock<Option<Vec<SharedVolume>>>,
}

impl Index {
    pub fn new(volumes: Vec<SharedVolume>) -> Self {
        Self { volumes: RwLock::new(volumes), overlay: RwLock::new(None) }
    }
    /// 現時点の検索対象（Arc のクローン）。ファイルリストが開いていればそれ。
    ///
    /// ロックの方針: 書き込み（USN 反映、構築）はミリ秒単位に保ち、長い読み取り（保存）は列を複製して
    /// ロック外で行う。UI は try_read（取れなければ前フレームの行を描く）、検索・IPC は read で待つ。
    /// `read_recursive` は書き込みを飢えさせる（UI の毎フレームの読みで書き込みが数秒待った）ので使わない。
    pub fn snapshot(&self) -> Vec<SharedVolume> {
        if let Some(o) = self.overlay.read().as_ref() {
            return o.clone();
        }
        self.volumes.read().clone()
    }
    /// 実ボリュームの一覧（ファイルリストに関わらず）。
    pub fn volumes(&self) -> Vec<SharedVolume> {
        self.volumes.read().clone()
    }
    pub fn set_overlay(&self, o: Option<Vec<SharedVolume>>) {
        *self.overlay.write() = o;
    }
    pub fn has_overlay(&self) -> bool {
        self.overlay.read().is_some()
    }
    pub fn push(&self, v: SharedVolume) {
        self.volumes.write().push(v);
    }
    /// `old` を `new` に差し替える（再スキャン完了時）。`old` が無ければ末尾に追加。
    pub fn replace(&self, old: &SharedVolume, new: SharedVolume) {
        let mut g = self.volumes.write();
        match g.iter().position(|v| Arc::ptr_eq(v, old)) {
            Some(i) => g[i] = new,
            None => g.push(new),
        }
    }
    pub fn total_entries(&self) -> usize {
        self.snapshot().iter().map(|v| v.read().live_count()).sum()
    }
}
