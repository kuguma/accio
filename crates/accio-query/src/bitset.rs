//! エントリ集合のビットセット。検索の各述語はこれを返し、論理演算で合成する。

use std::sync::atomic::{AtomicU64, Ordering};

pub struct Bitset {
    pub words: Vec<u64>,
    pub len: usize,
}

impl Bitset {
    pub fn zeros(len: usize) -> Self {
        Self { words: vec![0; len.div_ceil(64)], len }
    }
    pub fn ones(len: usize) -> Self {
        let mut b = Self { words: vec![u64::MAX; len.div_ceil(64)], len };
        b.trim();
        b
    }
    fn trim(&mut self) {
        let rem = self.len % 64;
        if rem != 0 {
            if let Some(last) = self.words.last_mut() {
                *last &= (1u64 << rem) - 1;
            }
        }
    }
    #[inline]
    pub fn set(&mut self, i: usize) {
        self.words[i >> 6] |= 1u64 << (i & 63);
    }
    #[inline]
    pub fn get(&self, i: usize) -> bool {
        self.words[i >> 6] & (1u64 << (i & 63)) != 0
    }
    pub fn and_assign(&mut self, o: &Bitset) {
        for (a, b) in self.words.iter_mut().zip(&o.words) {
            *a &= *b;
        }
    }
    pub fn or_assign(&mut self, o: &Bitset) {
        for (a, b) in self.words.iter_mut().zip(&o.words) {
            *a |= *b;
        }
    }
    pub fn and_not_assign(&mut self, o: &Bitset) {
        for (a, b) in self.words.iter_mut().zip(&o.words) {
            *a &= !*b;
        }
    }
    pub fn not_assign(&mut self) {
        for a in self.words.iter_mut() {
            *a = !*a;
        }
        self.trim();
    }
    pub fn count(&self) -> usize {
        self.words.iter().map(|w| w.count_ones() as usize).sum()
    }
    pub fn iter_ones(&self) -> impl Iterator<Item = usize> + '_ {
        self.words.iter().enumerate().flat_map(|(wi, &w)| {
            let mut w = w;
            std::iter::from_fn(move || {
                if w == 0 {
                    None
                } else {
                    let t = w.trailing_zeros() as usize;
                    w &= w - 1;
                    Some(wi * 64 + t)
                }
            })
        })
    }

    /// 並列書き込み用のアトミック視点。
    pub fn atomic(&self) -> &[AtomicU64] {
        // SAFETY: AtomicU64 と u64 は同じサイズ・配置。self を可変に使わない間だけ共有する。
        unsafe { std::slice::from_raw_parts(self.words.as_ptr() as *const AtomicU64, self.words.len()) }
    }
}

#[inline]
pub fn atomic_set(words: &[AtomicU64], i: usize) {
    words[i >> 6].fetch_or(1u64 << (i & 63), Ordering::Relaxed);
}
