//! $MFT の一括読み取り。
//!
//! 手順: ブートセクタ → $MFT 自身のレコード (0) → その $DATA のデータランから MFT 全体の物理位置を得る →
//! 大きなチャンクに分けて並列に読み、各チャンク内のレコードを逐次パースして `sink` へ渡す。
//! ディレクトリ走査は一切行わないため、ジャンクション循環や遅い FS の影響を受けない。

use crate::boot::BootInfo;
use crate::mft::{decode_data_runs, fixup, for_each_attribute, MftEntry, MftRecordParser, ATTR_ATTRIBUTE_LIST, ATTR_DATA};
use crate::reader::{AlignedBuf, PositionalRead};
use anyhow::{anyhow, bail, Result};
use rayon::prelude::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// 1 回の読み取り単位（バイト）。レコードサイズの倍数に丸められる。
    pub chunk_bytes: usize,
    /// このスキャン専用のスレッド数。グローバルな rayon プールは使わない
    /// （`sink` がロック待ちでブロックしてもプール全体を巻き込まないようにするため）。
    pub threads: usize,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self { chunk_bytes: 32 << 20, threads: 4 }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ScanStats {
    pub total_records: u64,
    pub mft_bytes: u64,
}

/// データランの列: (LCN（疎なら None）, クラスタ数)
type Runs = Vec<(Option<u64>, u64)>;

/// MFT 全体を読み、使用中レコードを `sink` に渡す。`sink` は複数スレッドから同時に呼ばれる。
/// `progress(read_bytes, total_bytes)`、`cancel` が true になれば途中で打ち切る。
pub fn scan_mft<R: PositionalRead + ?Sized>(
    reader: &R,
    opts: &ScanOptions,
    cancel: &AtomicBool,
    progress: &(dyn Fn(u64, u64) + Sync),
    sink: &(dyn Fn(Vec<MftEntry>) + Sync),
) -> Result<ScanStats> {
    let mut sector = [0u8; 512];
    reader.read_at(0, &mut sector)?;
    let boot = BootInfo::parse(&sector)?;
    let rs = boot.bytes_per_record as usize;

    // $MFT レコード 0 を読み、$DATA のデータランを得る
    let mut rec0 = vec![0u8; rs];
    reader.read_at(boot.mft_lcn * boot.bytes_per_cluster as u64, &mut rec0)?;
    if &rec0[0..4] != b"FILE" || !fixup(&mut rec0) {
        bail!("$MFT レコード 0 が不正");
    }
    // (start_vcn, runs) の列。断片化した $MFT では $DATA が $ATTRIBUTE_LIST 経由で
    // 複数の拡張レコードに分かれるので、それらも集めて VCN 順に並べる。
    let mut pieces: Vec<(u64, Runs)> = Vec::new();
    let mut mft_real_size = 0u64;
    let mut attr_list: Option<Vec<u8>> = None;
    let mut attr_list_nonres: Option<(Runs, u64)> = None;
    for_each_attribute(&rec0, |ty, a| {
        if ty == ATTR_DATA && a[9] == 0 && a[8] != 0 {
            let start_vcn = u64::from_le_bytes(a[16..24].try_into().unwrap());
            if start_vcn == 0 {
                mft_real_size = u64::from_le_bytes(a[48..56].try_into().unwrap());
            }
            pieces.push((start_vcn, decode_data_runs(a)));
        }
        if ty == ATTR_ATTRIBUTE_LIST {
            if a[8] == 0 {
                let vlen = u16::from_le_bytes([a[16], a[17]]) as usize | ((u16::from_le_bytes([a[18], a[19]]) as usize) << 16);
                let vofs = u16::from_le_bytes([a[20], a[21]]) as usize;
                if vofs + vlen <= a.len() {
                    attr_list = Some(a[vofs..vofs + vlen].to_vec());
                }
            } else if a.len() >= 64 {
                // 断片化が激しいと属性リスト自体が非常駐になる（この環境の C: がそう）
                attr_list_nonres = Some((decode_data_runs(a), u64::from_le_bytes(a[48..56].try_into().unwrap())));
            }
        }
    });
    if pieces.is_empty() || mft_real_size == 0 {
        bail!("$MFT の $DATA が見つからない");
    }
    let bpc = boot.bytes_per_cluster as u64;
    if let Some((runs, real_size)) = attr_list_nonres {
        let mut buf = Vec::with_capacity(real_size as usize);
        for (lcn, clusters) in runs {
            let len = clusters * bpc;
            let mut chunk = vec![0u8; len as usize];
            if let Some(lcn) = lcn {
                reader.read_at(lcn * bpc, &mut chunk)?;
            }
            buf.extend_from_slice(&chunk);
        }
        buf.truncate(real_size as usize);
        attr_list = Some(buf);
    }
    // 拡張レコードの場所は、基底レコードにある先頭ランから辿れる範囲にあることを前提にする
    // （$MFT の拡張レコードは常に MFT 先頭付近にある）。
    if let Some(list) = attr_list {
        let first_runs = pieces[0].1.clone();
        let locate = |recno: u64| -> Option<u64> {
            let mut vcn = 0u64;
            let target = recno * rs as u64;
            for (lcn, clusters) in &first_runs {
                let len = clusters * bpc;
                if let Some(lcn) = lcn {
                    if target >= vcn && target < vcn + len {
                        return Some(lcn * bpc + (target - vcn));
                    }
                }
                vcn += len;
            }
            None
        };
        let mut p = 0usize;
        let mut seen = std::collections::HashSet::new();
        while p + 26 <= list.len() {
            let ty = u32::from_le_bytes(list[p..p + 4].try_into().unwrap());
            let len = u16::from_le_bytes([list[p + 4], list[p + 5]]) as usize;
            if len < 26 {
                break;
            }
            let name_len = list[p + 6];
            let base = u64::from_le_bytes(list[p + 16..p + 24].try_into().unwrap()) & 0x0000_FFFF_FFFF_FFFF;
            if ty == ATTR_DATA && name_len == 0 && base != 0 && seen.insert(base) {
                let phys = locate(base).ok_or_else(|| anyhow!("$MFT 拡張レコード {base} の位置が不明"))?;
                let mut ext = vec![0u8; rs];
                reader.read_at(phys, &mut ext)?;
                if &ext[0..4] == b"FILE" && fixup(&mut ext) {
                    for_each_attribute(&ext, |ty, a| {
                        if ty == ATTR_DATA && a[9] == 0 && a[8] != 0 {
                            let start_vcn = u64::from_le_bytes(a[16..24].try_into().unwrap());
                            pieces.push((start_vcn, decode_data_runs(a)));
                        }
                    });
                }
            }
            p += len;
        }
    }
    pieces.sort_by_key(|(v, _)| *v);
    pieces.dedup_by_key(|(v, _)| *v);

    // 論理 (VCN) 空間 → 物理オフセットの対応表。クラスタがレコードより小さいボリュームでは
    // 1 レコードが複数ランにまたがるため、論理チャンク単位で読み、物理ランごとに読み分ける。
    let mut extents: Vec<(u64, u64, u64)> = Vec::new(); // (logical_start, phys_start, len_bytes)
    for (start_vcn, runs) in pieces {
        let mut vcn_bytes = start_vcn * bpc;
        for (lcn, clusters) in runs {
            let run_len = clusters * bpc;
            if let Some(lcn) = lcn {
                extents.push((vcn_bytes, lcn * bpc, run_len));
            }
            vcn_bytes += run_len;
        }
    }
    let covered: u64 = extents.iter().map(|e| e.2).sum();
    if covered < mft_real_size {
        bail!("$MFT のデータランが不足 ({covered} < {mft_real_size} bytes)");
    }
    let read_logical = |start: u64, buf: &mut [u8]| -> Result<()> {
        let mut pos = 0u64;
        let want = buf.len() as u64;
        while pos < want {
            let l = start + pos;
            let (ls, ps, len) = *extents
                .iter()
                .find(|(ls, _, len)| *ls <= l && l < ls + len)
                .ok_or_else(|| anyhow!("MFT 論理オフセット {l} に対応するランがない"))?;
            let n = (ls + len - l).min(want - pos);
            reader.read_at(ps + (l - ls), &mut buf[pos as usize..(pos + n) as usize])?;
            pos += n;
        }
        Ok(())
    };

    let chunk = ((opts.chunk_bytes / rs).max(1) * rs) as u64;
    let usable = mft_real_size / rs as u64 * rs as u64;
    let jobs: Vec<(u64, u64)> = (0..usable).step_by(chunk as usize).map(|s| (s, (usable - s).min(chunk))).collect();

    let total = mft_real_size;
    let done = AtomicU64::new(0);
    let parser = MftRecordParser::new(&boot);
    progress(0, total);

    let pool = rayon::ThreadPoolBuilder::new().num_threads(opts.threads.max(1)).thread_name(|i| format!("mft-scan-{i}")).build()?;
    let result: Result<()> = pool.install(|| jobs.par_iter().try_for_each(|&(logical, len)| {
        if cancel.load(Ordering::Relaxed) {
            return Err(anyhow!("cancelled"));
        }
        let first_rec = logical / rs as u64;
        let mut buf = AlignedBuf::new(len as usize, 4096);
        read_logical(logical, buf.as_mut())?;
        let data = buf.as_mut();
        let n = data.len() / rs;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let rec = &mut data[i * rs..(i + 1) * rs];
            if let Some(e) = parser.parse(rec, first_rec + i as u64) {
                out.push(e);
            }
        }
        sink(out);
        let d = done.fetch_add(len, Ordering::Relaxed) + len;
        progress(d, total);
        Ok(())
    }));
    result?;

    Ok(ScanStats { total_records: mft_real_size / rs as u64, mft_bytes: mft_real_size })
}
