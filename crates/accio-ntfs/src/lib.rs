//! NTFS 生アクセス層。
//!
//! 責務: Win32/NTFS の生データを安全な Rust 値（`MftEntry`, `UsnEvent`, `VolumeInfo`）に変換することのみ。
//! インデックスの構造や検索については一切関知しない。
//!
//! 事前条件: 生ボリュームのオープンと USN ジャーナルの読み取りは特権操作であり、
//! 管理者権限で動くプロセスからのみ成功する。

pub mod boot;
pub mod enumerate;
pub mod mft;
pub mod reader;
pub mod refs;
pub mod scan;
pub mod usn;
pub mod volumes;

pub use enumerate::{enum_usn_data, parse_usn_records, EnumRecord, FileIdReader};
pub use mft::{MftEntry, MftRecordParser};
pub use reader::{FileReader, PositionalRead};
pub use scan::{scan_mft, ScanOptions};
pub use usn::{RecordSource, UsnEvent, UsnJournal, UsnReason};
pub use volumes::{discover_volumes, VolumeInfo};
