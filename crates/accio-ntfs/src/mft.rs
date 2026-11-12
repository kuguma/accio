//! MFT FILE レコードのパーサ。1 レコード → `MftEntry`。
//!
//! 対象属性は $STANDARD_INFORMATION / $ATTRIBUTE_LIST / $FILE_NAME / 無名 $DATA のみ。
//! それ以外（インデックス、セキュリティ、代替データストリーム）は読み飛ばす。

use crate::boot::BootInfo;

pub const ATTR_STANDARD_INFORMATION: u32 = 0x10;
pub const ATTR_ATTRIBUTE_LIST: u32 = 0x20;
pub const ATTR_FILE_NAME: u32 = 0x30;
pub const ATTR_DATA: u32 = 0x80;
pub const ATTR_END: u32 = 0xFFFF_FFFF;

/// $FILE_NAME の名前空間。DOS 専用 8.3 名は検索対象にしない。
pub const NS_DOS: u8 = 2;

pub const RECORD_IN_USE: u16 = 0x1;
pub const RECORD_IS_DIR: u16 = 0x2;

pub const FILE_ATTR_DIRECTORY: u32 = 0x10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MftName {
    /// 親ディレクトリの MFT レコード番号（下位 48bit）
    pub parent: u64,
    /// 親参照のシーケンス番号（上位 16bit）
    pub parent_seq: u16,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MftEntry {
    pub record: u64,
    pub seq: u16,
    /// 0 以外なら拡張レコード（値は基底レコードの番号）。属性のみを持ち、基底側へ統合する。
    pub base_record: u64,
    pub is_dir: bool,
    pub has_attr_list: bool,
    /// $STANDARD_INFORMATION のファイル属性。ディレクトリなら FILE_ATTR_DIRECTORY を立てる。
    pub attrs: u32,
    /// 無名 $DATA の実サイズ。見つからなければ None（拡張レコード側にある可能性）。
    pub size: Option<u64>,
    pub alloc_size: Option<u64>,
    /// FILETIME（100ns 単位, 1601-01-01 起点）
    pub mtime: i64,
    pub ctime: i64,
    /// 最終アクセス日時（NTFS 既定では更新が抑制されるので古い値のことが多い）
    pub atime: i64,
    pub names: Vec<MftName>,
}

pub struct MftRecordParser {
    record_size: usize,
}

#[inline]
fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
#[inline]
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
#[inline]
fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}
#[inline]
fn i64_at(b: &[u8], o: usize) -> i64 {
    i64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

impl MftRecordParser {
    pub fn new(boot: &BootInfo) -> Self {
        Self { record_size: boot.bytes_per_record as usize }
    }

    pub fn record_size(&self) -> usize {
        self.record_size
    }

    /// フィックスアップを適用する。成功なら true。`rec` はレコード 1 個分。
    fn apply_fixups(rec: &mut [u8]) -> bool {
        let usa_ofs = u16_at(rec, 4) as usize;
        let usa_count = u16_at(rec, 6) as usize;
        if usa_count < 2 || usa_ofs + usa_count * 2 > rec.len() {
            return false;
        }
        let stride = rec.len() / (usa_count - 1);
        if stride < 2 || stride * (usa_count - 1) != rec.len() {
            return false;
        }
        let usn = [rec[usa_ofs], rec[usa_ofs + 1]];
        // FSCTL_GET_NTFS_FILE_RECORD はすでに fixup を適用した状態で返す（セクタ末尾が USN ではなく
        // USA の値になっている。2026-11-12 に実機で確認）。その場合はそのまま有効とみなす。
        let already_applied = (1..usa_count).all(|i| {
            let end = i * stride;
            let src = usa_ofs + i * 2;
            rec[end - 2] == rec[src] && rec[end - 1] == rec[src + 1]
        });
        if already_applied {
            return true;
        }
        for i in 1..usa_count {
            let end = i * stride;
            if rec[end - 2] != usn[0] || rec[end - 1] != usn[1] {
                return false;
            }
            let src = usa_ofs + i * 2;
            let (a, b) = (rec[src], rec[src + 1]);
            rec[end - 2] = a;
            rec[end - 1] = b;
        }
        true
    }

    /// 未使用・破損レコードは None。
    pub fn parse(&self, rec: &mut [u8], record_number: u64) -> Option<MftEntry> {
        if rec.len() < 48 || &rec[0..4] != b"FILE" {
            return None;
        }
        let flags = u16_at(rec, 22);
        if flags & RECORD_IN_USE == 0 {
            return None;
        }
        if !Self::apply_fixups(rec) {
            return None;
        }
        let seq = u16_at(rec, 16);
        let attrs_ofs = u16_at(rec, 20) as usize;
        let bytes_used = (u32_at(rec, 24) as usize).min(rec.len());
        let base_ref = u64_at(rec, 32);
        let is_dir = flags & RECORD_IS_DIR != 0;

        let mut e = MftEntry {
            record: record_number,
            seq,
            base_record: base_ref & 0x0000_FFFF_FFFF_FFFF,
            is_dir,
            ..Default::default()
        };

        let mut ofs = attrs_ofs;
        while ofs + 16 <= bytes_used {
            let ty = u32_at(rec, ofs);
            if ty == ATTR_END {
                break;
            }
            let len = u32_at(rec, ofs + 4) as usize;
            if len < 24 || ofs + len > bytes_used {
                break;
            }
            let a = &rec[ofs..ofs + len];
            let nonresident = a[8] != 0;
            let name_len = a[9] as usize;
            match ty {
                ATTR_STANDARD_INFORMATION if !nonresident => {
                    if let Some(v) = resident_value(a) {
                        if v.len() >= 36 {
                            e.ctime = i64_at(v, 0);
                            e.mtime = i64_at(v, 8);
                            e.atime = i64_at(v, 24);
                            e.attrs = u32_at(v, 32);
                        }
                    }
                }
                ATTR_ATTRIBUTE_LIST => e.has_attr_list = true,
                ATTR_FILE_NAME if !nonresident => {
                    if let Some(v) = resident_value(a) {
                        if v.len() >= 66 {
                            let ns = v[65];
                            let nlen = v[64] as usize;
                            if ns != NS_DOS && 66 + nlen * 2 <= v.len() {
                                let pref = u64_at(v, 0);
                                let name = utf16_to_string(&v[66..66 + nlen * 2]);
                                e.names.push(MftName {
                                    parent: pref & 0x0000_FFFF_FFFF_FFFF,
                                    parent_seq: (pref >> 48) as u16,
                                    name,
                                });
                            }
                        }
                    }
                }
                ATTR_DATA if name_len == 0 => {
                    if nonresident {
                        if a.len() >= 64 {
                            let start_vcn = u64_at(a, 16);
                            if start_vcn == 0 {
                                e.alloc_size = Some(u64_at(a, 40));
                                e.size = Some(u64_at(a, 48));
                            }
                        }
                    } else {
                        let vlen = u32_at(a, 16) as u64;
                        e.size = Some(vlen);
                        e.alloc_size = Some(vlen);
                    }
                }
                _ => {}
            }
            ofs += len;
        }
        if is_dir {
            e.attrs |= FILE_ATTR_DIRECTORY;
        }
        Some(e)
    }
}

fn resident_value(a: &[u8]) -> Option<&[u8]> {
    if a.len() < 24 {
        return None;
    }
    let vlen = u32_at(a, 16) as usize;
    let vofs = u16_at(a, 20) as usize;
    if vofs + vlen > a.len() {
        return None;
    }
    Some(&a[vofs..vofs + vlen])
}

pub fn utf16_to_string(b: &[u8]) -> String {
    let units = b.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]]));
    char::decode_utf16(units).map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER)).collect()
}

/// 非常駐属性のデータランを (LCN, クラスタ数) の列へ復号する。スパース部分は LCN=None。
pub fn decode_data_runs(a: &[u8]) -> Vec<(Option<u64>, u64)> {
    let mut out = Vec::new();
    if a.len() < 64 || a[8] == 0 {
        return out;
    }
    let mut p = u16_at(a, 32) as usize;
    let mut lcn: i64 = 0;
    while p < a.len() {
        let h = a[p];
        if h == 0 {
            break;
        }
        let len_sz = (h & 0xF) as usize;
        let ofs_sz = (h >> 4) as usize;
        p += 1;
        if len_sz == 0 || p + len_sz + ofs_sz > a.len() {
            break;
        }
        let mut len: u64 = 0;
        for i in 0..len_sz {
            len |= (a[p + i] as u64) << (8 * i);
        }
        p += len_sz;
        if ofs_sz == 0 {
            out.push((None, len));
            continue;
        }
        let mut delta: i64 = 0;
        for i in 0..ofs_sz {
            delta |= (a[p + i] as i64) << (8 * i);
        }
        // 符号拡張
        let shift = 64 - 8 * ofs_sz as u32;
        delta = (delta << shift) >> shift;
        p += ofs_sz;
        lcn += delta;
        out.push((Some(lcn as u64), len));
    }
    out
}

/// レコード内の属性を列挙し、`f(type, attr_bytes)` を呼ぶ（$MFT 自身の $DATA ランを得るために使う）。
pub fn for_each_attribute(rec: &[u8], mut f: impl FnMut(u32, &[u8])) {
    let attrs_ofs = u16_at(rec, 20) as usize;
    let bytes_used = (u32_at(rec, 24) as usize).min(rec.len());
    let mut ofs = attrs_ofs;
    while ofs + 16 <= bytes_used {
        let ty = u32_at(rec, ofs);
        if ty == ATTR_END {
            break;
        }
        let len = u32_at(rec, ofs + 4) as usize;
        if len < 24 || ofs + len > bytes_used {
            break;
        }
        f(ty, &rec[ofs..ofs + len]);
        ofs += len;
    }
}

/// フィックスアップ適用だけを公開（$MFT レコード 0 の読み取り用）。
pub fn fixup(rec: &mut [u8]) -> bool {
    MftRecordParser::apply_fixups(rec)
}
