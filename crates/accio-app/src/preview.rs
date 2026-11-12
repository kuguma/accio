//! プレビューペイン（Alt+P）。選択ファイルの画像またはテキスト先頭をワーカースレッドで読み、UI は結果を描くだけ。

use arc_swap::ArcSwapOption;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub enum Content {
    Image { size: [usize; 2], rgba: Vec<u8> },
    Text(String),
    Info(String),
}

pub struct Loaded {
    pub path: String,
    pub content: Content,
}

pub struct Previewer {
    gen: Arc<AtomicU64>,
    pub result: Arc<ArcSwapOption<Loaded>>,
    repaint: Arc<dyn Fn() + Send + Sync>,
}

const MAX_IMAGE_BYTES: u64 = 64 << 20;
const TEXT_BYTES: usize = 16 << 10;

impl Previewer {
    pub fn new(repaint: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self { gen: Arc::new(AtomicU64::new(0)), result: Arc::new(ArcSwapOption::empty()), repaint }
    }

    /// 選択が変わったら呼ぶ。古い読み込みは結果を捨てる。
    pub fn request(&self, path: String, is_dir: bool) {
        let my = self.gen.fetch_add(1, Ordering::AcqRel) + 1;
        let gen = self.gen.clone();
        let out = self.result.clone();
        let repaint = self.repaint.clone();
        std::thread::spawn(move || {
            let content = load(&path, is_dir);
            if gen.load(Ordering::Acquire) == my {
                out.store(Some(Arc::new(Loaded { path, content })));
                repaint();
            }
        });
    }

    pub fn clear(&self) {
        self.gen.fetch_add(1, Ordering::AcqRel);
        self.result.store(None);
    }
}

/// 検証用の入口。
pub fn load_for_probe(path: &str) -> Content {
    let is_dir = std::path::Path::new(path).is_dir();
    load(path, is_dir)
}

/// サムネイル読み込み（非同期）。`request` で頼み、`take` で出来上がった分を受け取る。
/// (パス, 復号した RGBA 画像 (サイズ, 画素) / 失敗なら None)
pub type ThumbResult = (String, Option<([usize; 2], Vec<u8>)>);

pub struct Thumbs {
    tx: crossbeam_channel::Sender<(String, u32)>,
    rx: crossbeam_channel::Receiver<ThumbResult>,
}

impl Thumbs {
    pub fn new(repaint: Arc<dyn Fn() + Send + Sync>) -> Self {
        let (tx, jobs) = crossbeam_channel::unbounded::<(String, u32)>();
        let (done_tx, rx) = crossbeam_channel::unbounded();
        for _ in 0..4 {
            let jobs = jobs.clone();
            let done_tx = done_tx.clone();
            let repaint = repaint.clone();
            std::thread::spawn(move || {
                while let Ok((path, size)) = jobs.recv() {
                    let r = std::fs::metadata(&path).ok().filter(|m| m.len() <= MAX_IMAGE_BYTES).and_then(|_| image::open(&path).ok()).map(|img| {
                        let t = img.thumbnail(size, size).to_rgba8();
                        ([t.width() as usize, t.height() as usize], t.into_raw())
                    });
                    let _ = done_tx.send((path, r));
                    repaint();
                }
            });
        }
        Self { tx, rx }
    }

    pub fn request(&self, path: String, size: u32) {
        let _ = self.tx.send((path, size));
    }

    pub fn take(&self) -> Vec<ThumbResult> {
        self.rx.try_iter().collect()
    }
}

pub fn is_image_ext(ext: &str) -> bool {
    matches!(ext, "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" | "tif" | "tiff")
}

fn load(path: &str, is_dir: bool) -> Content {
    if is_dir {
        let n = std::fs::read_dir(path).map(|r| r.count()).unwrap_or(0);
        return Content::Info(crate::i18n::trf("フォルダ（直下 {} 項目）", &[&n]));
    }
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let md = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => return Content::Info(crate::i18n::trf("読めません: {}", &[&e])),
    };
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" | "tif" | "tiff" => {
            if md.len() > MAX_IMAGE_BYTES {
                return Content::Info(crate::i18n::trf("画像が大きすぎます（{} MB）", &[&(md.len() >> 20)]));
            }
            match image::open(path) {
                Ok(img) => {
                    // 表示用に縮小（長辺 1024）
                    let img = if img.width().max(img.height()) > 1024 { img.thumbnail(1024, 1024) } else { img };
                    let rgba = img.to_rgba8();
                    Content::Image { size: [rgba.width() as usize, rgba.height() as usize], rgba: rgba.into_raw() }
                }
                Err(e) => Content::Info(crate::i18n::trf("画像を読めません: {}", &[&e])),
            }
        }
        "txt" | "md" | "rs" | "c" | "h" | "cpp" | "hpp" | "py" | "js" | "ts" | "json" | "toml" | "yaml" | "yml" | "ini" | "cfg" | "log" | "csv" | "xml" | "html" | "htm" | "css" | "bat" | "ps1" | "sh" | "java" | "cs" | "go" | "lua" | "sql" | "tex" | "efu" => {
            match std::fs::File::open(path) {
                Ok(mut f) => {
                    use std::io::Read;
                    let mut buf = vec![0u8; TEXT_BYTES];
                    let n = f.read(&mut buf).unwrap_or(0);
                    buf.truncate(n);
                    let s = String::from_utf8_lossy(&buf).into_owned();
                    Content::Text(if md.len() as usize > n { crate::i18n::trf("{}\n…（先頭 {} KB）", &[&s, &(n >> 10)]) } else { s })
                }
                Err(e) => Content::Info(crate::i18n::trf("読めません: {}", &[&e])),
            }
        }
        _ => Content::Info(crate::i18n::trf("{} バイト（プレビュー非対応の種類）", &[&(md.len())])),
    }
}
