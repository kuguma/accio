# ReFS 対応の調査メモ（2026-11-12）

ReFS は MFT を持たないので accio-ntfs の生読みスキャンは使えない。Everything 1.4 は ReFS を
USN ベースで扱う。accio でも同じ構成にする。開発機には ReFS ボリュームが無かったため、Dev Drive（ReFS）の VHDX を作って検証した。

## 非昇格で確かめたこと（scripts/probe_usn.py）

- `\\.\C:` はアクセス権 0 なら非昇格でも開けるが、`FSCTL_ENUM_USN_DATA` は error 1 で失敗する。
  `GENERIC_READ` で開くのは error 5（拒否）。列挙には管理者権限が要る（Everything と同じ）。
- `OpenFileById`（ルート `C:\` を `FILE_FLAG_BACKUP_SEMANTICS` で開いたハンドルを基点にする）は
  非昇格で動く。FRN からサイズ・日時・属性（`GetFileInformationByHandle`）が取れる。
  ReFS でのサイズ/日時の補完はこれで行える（NTFS でも検証できる）。

## 実装済みの土台（2026-11-12、`crates/accio-ntfs/src/enumerate.rs`）

- `parse_usn_records`: USN_RECORD_V2 / V3 の解釈（単体テスト）。`usn.rs::read` もこれを使い、
  `set_max_major(3)` で V3 を受け付けられる（NTFS は 2 のまま）。
- `enum_usn_data`: `FSCTL_ENUM_USN_DATA`（MFT_ENUM_DATA_V1、2..=3）のループ。昇格が要るので未検証。
- `FileIdReader`: `OpenFileById` でサイズ・日時・属性・パス。NTFS で非昇格のテストが通る。

## 実装（2026-11-12、`refs.rs` と engine）

- `IdMap`: ファイル ID ⇔ 合成レコード番号（ルートは 5、以後 16 から）。索引ファイルの `refs_ids` に保存
- `to_mft_entries`: 列挙結果を `MftEntry` 列にして通常の `VolumeBuilder` で組む（親が後から来ても可）
- `RefsAdapter`: ジャーナルの ID を合成 FRN に直し、`read_file_record` は `OpenFileById` で組む。
  `UsnJournal::open_refs` ＋ `set_refs` で既存の USN 反映ループがそのまま動く
- engine `refs_scan`: 列挙 → 検索可能 → サイズ・日時を 1 万件ごとに補完 → 保存
- テスト: 偽データでの構築と作成イベントの反映（tests/refs_build.rs）、NTFS 上の OpenFileById 往復。
  Dev Drive（ReFS）の R: で列挙・ジャーナル追従・サイズ補完・保存/再読込を実機確認（2026-11-12 夜）

## 設計案（上の実装の元）

1. `volumes.rs`: `fs_name == "ReFS"` も対象にする。`VolumeInfo.fs_name` で経路を分ける。
2. 列挙: `FSCTL_ENUM_USN_DATA`（`MFT_ENUM_DATA_V1`、MinMajorVersion 2 / MaxMajorVersion 3）を
   StartFileReferenceNumber を進めながら呼び、`USN_RECORD_V2`（64bit ID）と `USN_RECORD_V3`
   （128bit ID）の両方を解釈する。得られるのは FRN・親 FRN・属性・名前だけで、サイズ・日時は無い。
3. サイズ・日時: 索引が検索可能になった後、バックグラウンドで `OpenFileById` を回して埋める
   （1800 万件なら数分。Everything も ReFS では同様に遅い）。
4. ジャーナル: `FSCTL_READ_USN_JOURNAL` は `READ_USN_JOURNAL_DATA_V1` で V3 レコードも受ける。
   `usn.rs::read` は今 V2 しか解釈していないので V3 を足す。
5. `RecordSource::read_file_record` の ReFS 実装: `FSCTL_GET_NTFS_FILE_RECORD` は使えないので
   `OpenFileById` ＋ `GetFinalPathNameByHandleW`（名前と親）で `MftEntry` を組む。
6. 索引側の課題: `VolumeIndex` は recno を u32（＋seq u16）で持ち、`by_recno` を密な表として
   引いている。ReFS の ID は 64/128bit で疎なので、ReFS ボリュームでは `by_recno` を
   `HashMap<u128, EntryId>` に置き換える必要がある（索引形式の変更）。NTFS 側の表現は変えない。

## 着手の条件

- ReFS ボリューム（VHD で可）と、昇格して列挙できる環境。`scripts/dev-relaunch.ps1` をタスク
  `accio-dev` として登録してもらえば、非対話で昇格経路を回せる。
