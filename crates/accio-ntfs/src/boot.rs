//! NTFS ブートセクタの解釈。MFT の位置とレコードサイズを得るためだけに使う。

use anyhow::{bail, Result};

#[derive(Debug, Clone, Copy)]
pub struct BootInfo {
    pub bytes_per_sector: u32,
    pub bytes_per_cluster: u32,
    pub mft_lcn: u64,
    pub bytes_per_record: u32,
    pub total_sectors: u64,
}

impl BootInfo {
    pub fn parse(sector: &[u8]) -> Result<BootInfo> {
        if sector.len() < 512 || &sector[3..11] != b"NTFS    " {
            bail!("NTFS ブートセクタではない");
        }
        let bytes_per_sector = u16::from_le_bytes([sector[0x0B], sector[0x0C]]) as u32;
        let sectors_per_cluster = match sector[0x0D] {
            // 2^(256-n) 表現（クラスタが 128 セクタを超えるボリューム）
            n if n > 0x80 => 1u32 << (256 - n as u32),
            n => n as u32,
        };
        let total_sectors = u64::from_le_bytes(sector[0x28..0x30].try_into().unwrap());
        let mft_lcn = u64::from_le_bytes(sector[0x30..0x38].try_into().unwrap());
        let bytes_per_cluster = bytes_per_sector * sectors_per_cluster;
        let rec = sector[0x40] as i8;
        let bytes_per_record = if rec < 0 {
            1u32 << (-(rec as i32))
        } else {
            rec as u32 * bytes_per_cluster
        };
        if bytes_per_sector == 0 || bytes_per_cluster == 0 || bytes_per_record == 0 {
            bail!("ブートセクタの値が不正");
        }
        Ok(BootInfo { bytes_per_sector, bytes_per_cluster, mft_lcn, bytes_per_record, total_sectors })
    }
}
