//! 索引の永続化。カラムごとに zstd 圧縮したセクションを並べる。
//! 読み込みは全セクションを並列に展開するので、ファイルサイズに対する起動時間はほぼ I/O 帯域で決まる。

use crate::volume::{VolumeIndex, VolumeMeta};
use anyhow::{anyhow, bail, Result};
use rayon::prelude::*;
use std::path::Path;

const MAGIC: &[u8; 4] = b"ACIO";
/// v3: 小文字アリーナがダイアクリティカルマークを折り畳む（旧版は再構築）、v4: アクセス日時列
const VERSION: u32 = 4;
const ZSTD_LEVEL: i32 = 1;

#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Sec {
    Arena = 1,
    NameOff,
    NameLen,
    Parent,
    Flags,
    Attrs,
    OrigOff,
    OrigLen,
    OrigArena,
    Size,
    Mtime,
    Ctime,
    Recno,
    ByRecno,
    Seq,
    Atime,
    RefsIds,
}

fn as_bytes<T: Copy>(v: &[T]) -> &[u8] {
    // SAFETY: T は POD（整数）で、スライスの寸法は len*size_of::<T>()。
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

fn from_bytes<T: Copy + Default>(b: &[u8]) -> Vec<T> {
    let n = b.len() / std::mem::size_of::<T>();
    let mut v = vec![T::default(); n];
    // SAFETY: v は n 要素分確保済みで、b は n*size_of::<T>() バイト以上。
    unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), v.as_mut_ptr() as *mut u8, n * std::mem::size_of::<T>()) };
    v
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

fn get_str(b: &[u8], p: &mut usize) -> Result<String> {
    let n = u32::from_le_bytes(b[*p..*p + 4].try_into()?) as usize;
    *p += 4;
    let s = std::str::from_utf8(&b[*p..*p + n])?.to_string();
    *p += n;
    Ok(s)
}

/// 保存用の生バイト列（列の複製）。ロック外で圧縮・書き出しするために作る。
pub struct Snapshot {
    meta: VolumeMeta,
    live: usize,
    sections: Vec<(Sec, Vec<u8>)>,
}

impl Snapshot {
    /// 列をまとめて複製する（1800 万件で 1.7GB、0.3 秒程度の読み取りロック）。`compact()` 済みであること。
    pub fn take(v: &VolumeIndex) -> Self {
        assert!(v.overflow.is_empty(), "save の前に compact が必要");
        let sections = vec![
            (Sec::Arena, v.arena.clone()),
            (Sec::NameOff, as_bytes(&v.name_off).to_vec()),
            (Sec::NameLen, as_bytes(&v.name_len).to_vec()),
            (Sec::Parent, as_bytes(&v.parent).to_vec()),
            (Sec::Flags, as_bytes(&v.flags).to_vec()),
            (Sec::Attrs, as_bytes(&v.attrs).to_vec()),
            (Sec::OrigOff, as_bytes(&v.orig_off).to_vec()),
            (Sec::OrigLen, as_bytes(&v.orig_len).to_vec()),
            (Sec::OrigArena, v.orig_arena.clone()),
            (Sec::Size, as_bytes(&v.size).to_vec()),
            (Sec::Mtime, as_bytes(&v.mtime_s).to_vec()),
            (Sec::Ctime, as_bytes(&v.ctime_s).to_vec()),
            (Sec::Atime, as_bytes(&v.atime_s).to_vec()),
            (Sec::RefsIds, as_bytes(&v.refs_ids).to_vec()),
            (Sec::Recno, as_bytes(&v.recno).to_vec()),
            (Sec::Seq, as_bytes(&v.seq).to_vec()),
            (Sec::ByRecno, as_bytes(&v.by_recno).to_vec()),
        ];
        Self { meta: v.meta.clone(), live: v.live, sections }
    }

    /// 圧縮して書き出す（ロック不要）。
    pub fn write(&self, path: &Path) -> Result<()> {
        write_sections(&self.meta, self.live, self.sections.iter().map(|(s, b)| (*s, b.as_slice())).collect(), path)
    }
}

/// 保存前に `compact()` 済みであること（overflow が空）。ロックを持ったまま圧縮するので、
/// 稼働中のボリュームには `Snapshot::take` → `write` を使う。
pub fn save(v: &VolumeIndex, path: &Path) -> Result<()> {
    assert!(v.overflow.is_empty(), "save の前に compact が必要");
    let sections: Vec<(Sec, &[u8])> = vec![
        (Sec::Arena, &v.arena),
        (Sec::NameOff, as_bytes(&v.name_off)),
        (Sec::NameLen, as_bytes(&v.name_len)),
        (Sec::Parent, as_bytes(&v.parent)),
        (Sec::Flags, as_bytes(&v.flags)),
        (Sec::Attrs, as_bytes(&v.attrs)),
        (Sec::OrigOff, as_bytes(&v.orig_off)),
        (Sec::OrigLen, as_bytes(&v.orig_len)),
        (Sec::OrigArena, &v.orig_arena),
        (Sec::Size, as_bytes(&v.size)),
        (Sec::Mtime, as_bytes(&v.mtime_s)),
        (Sec::Ctime, as_bytes(&v.ctime_s)),
        (Sec::Atime, as_bytes(&v.atime_s)),
        (Sec::RefsIds, as_bytes(&v.refs_ids)),
        (Sec::Recno, as_bytes(&v.recno)),
        (Sec::Seq, as_bytes(&v.seq)),
        (Sec::ByRecno, as_bytes(&v.by_recno)),
    ];
    write_sections(&v.meta, v.live, sections, path)
}

fn write_sections(meta: &VolumeMeta, live: usize, sections: Vec<(Sec, &[u8])>, path: &Path) -> Result<()> {
    let compressed: Vec<(Sec, usize, Vec<u8>)> = sections
        .par_iter()
        .map(|(id, raw)| (*id, raw.len(), zstd::bulk::compress(raw, ZSTD_LEVEL).unwrap_or_default()))
        .collect();

    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    put_str(&mut out, &meta.guid_path);
    put_str(&mut out, &meta.display_root);
    out.extend_from_slice(&meta.serial.to_le_bytes());
    out.extend_from_slice(&meta.journal_id.to_le_bytes());
    out.extend_from_slice(&meta.next_usn.to_le_bytes());
    out.extend_from_slice(&meta.mft_records.to_le_bytes());
    out.extend_from_slice(&(live as u64).to_le_bytes());
    out.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
    for (id, raw_len, data) in &compressed {
        out.extend_from_slice(&(*id as u32).to_le_bytes());
        out.extend_from_slice(&(*raw_len as u64).to_le_bytes());
        out.extend_from_slice(&(data.len() as u64).to_le_bytes());
        out.extend_from_slice(data);
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &out)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn load(path: &Path) -> Result<VolumeIndex> {
    let b = std::fs::read(path)?;
    if b.len() < 8 || &b[0..4] != MAGIC {
        bail!("不正な索引ファイル");
    }
    let ver = u32::from_le_bytes(b[4..8].try_into()?);
    if ver != VERSION {
        bail!("索引ファイルのバージョン不一致 ({ver} != {VERSION})");
    }
    let mut p = 8;
    let guid_path = get_str(&b, &mut p)?;
    let display_root = get_str(&b, &mut p)?;
    let serial = u32::from_le_bytes(b[p..p + 4].try_into()?);
    p += 4;
    let journal_id = u64::from_le_bytes(b[p..p + 8].try_into()?);
    p += 8;
    let next_usn = i64::from_le_bytes(b[p..p + 8].try_into()?);
    p += 8;
    let mft_records = u64::from_le_bytes(b[p..p + 8].try_into()?);
    p += 8;
    let live = u64::from_le_bytes(b[p..p + 8].try_into()?) as usize;
    p += 8;
    let n = u32::from_le_bytes(b[p..p + 4].try_into()?) as usize;
    p += 4;
    let mut secs: Vec<(u32, usize, &[u8])> = Vec::with_capacity(n);
    for _ in 0..n {
        let id = u32::from_le_bytes(b[p..p + 4].try_into()?);
        p += 4;
        let raw_len = u64::from_le_bytes(b[p..p + 8].try_into()?) as usize;
        p += 8;
        let comp_len = u64::from_le_bytes(b[p..p + 8].try_into()?) as usize;
        p += 8;
        secs.push((id, raw_len, &b[p..p + comp_len]));
        p += comp_len;
    }
    let raw: Vec<(u32, Vec<u8>)> = secs
        .par_iter()
        .map(|(id, raw_len, data)| Ok((*id, zstd::bulk::decompress(data, *raw_len)?)))
        .collect::<Result<_>>()?;

    let mut v = VolumeIndex {
        meta: VolumeMeta { guid_path, display_root, serial, journal_id, next_usn, mft_records },
        live,
        resolved: true,
        ..Default::default()
    };
    for (id, data) in raw {
        match id {
            x if x == Sec::Arena as u32 => v.arena = data,
            x if x == Sec::NameOff as u32 => v.name_off = from_bytes(&data),
            x if x == Sec::NameLen as u32 => v.name_len = from_bytes(&data),
            x if x == Sec::Parent as u32 => v.parent = from_bytes(&data),
            x if x == Sec::Flags as u32 => v.flags = from_bytes(&data),
            x if x == Sec::Attrs as u32 => v.attrs = from_bytes(&data),
            x if x == Sec::OrigOff as u32 => v.orig_off = from_bytes(&data),
            x if x == Sec::OrigLen as u32 => v.orig_len = from_bytes(&data),
            x if x == Sec::OrigArena as u32 => v.orig_arena = data,
            x if x == Sec::Size as u32 => v.size = from_bytes(&data),
            x if x == Sec::Mtime as u32 => v.mtime_s = from_bytes(&data),
            x if x == Sec::Ctime as u32 => v.ctime_s = from_bytes(&data),
            x if x == Sec::Atime as u32 => v.atime_s = from_bytes(&data),
            x if x == Sec::RefsIds as u32 => v.refs_ids = from_bytes(&data),
            x if x == Sec::Recno as u32 => v.recno = from_bytes(&data),
            x if x == Sec::Seq as u32 => v.seq = from_bytes(&data),
            x if x == Sec::ByRecno as u32 => v.by_recno = from_bytes(&data),
            _ => {}
        }
    }
    let n = v.parent.len();
    let cols = [
        v.name_off.len(),
        v.name_len.len(),
        v.flags.len(),
        v.attrs.len(),
        v.orig_off.len(),
        v.orig_len.len(),
        v.size.len(),
        v.mtime_s.len(),
        v.ctime_s.len(),
        v.atime_s.len(),
        v.recno.len(),
        v.seq.len(),
    ];
    if cols.iter().any(|&c| c != n) || n == 0 {
        return Err(anyhow!("索引ファイルのカラム長不一致"));
    }
    v.rebuild_multi();
    Ok(v)
}
