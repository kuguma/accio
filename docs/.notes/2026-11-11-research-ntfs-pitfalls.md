# 調査メモ: NTFS インデクサの落とし穴（2026-11-11 23:10 JST）

Web 調査の要約。設計への反映は `docs/adr/0001` を参照。

## MFT 列挙 (FSCTL_ENUM_USN_DATA / $MFT 生読み)
- 生ボリュームハンドルは実質的に管理者権限が必要。Everything は Everything Service（SYSTEM で動き名前情報だけを渡す）で回避している。
- `FSCTL_ENUM_USN_DATA` は DOS 専用 8.3 名を返さず、ハードリンクは FRN ごとに 1 レコード/名。USN レコードにはサイズ・日付が無い。サイズ・日付を得るには (a) ファイルごとに開く（遅い）、(b) `FSCTL_GET_NTFS_FILE_RECORD` で MFT レコードを読む、(c) `$MFT` を生で順読みして自前解釈（Everything/WizTree）。
- ルートは MFT レコード 5、自身を親として参照する。レコード 0〜15 と `$Extend` 配下はシステムファイル。
- リパースポイント（ジャンクション/シンボリックリンク/マウントポイント）は属性付きの葉として現れ、辿られない。
- MFT が巨大（断片化・ADS 大量）なボリュームでは列挙がレコード数でなく MFT サイズに比例する。

## USN ジャーナル
- `FSCTL_QUERY_USN_JOURNAL` → 無ければ `FSCTL_CREATE_USN_JOURNAL`。Windows 既定 32MB/8MB は Win10 以降では 1 日で埋まることがあり、voidtools は 128MB を推奨。満杯＝ラップで全再スキャン。
- `StartUsn < FirstUsn` → `ERROR_JOURNAL_ENTRY_DELETED`（ラップ）。`UsnJournalID` 不一致も再スキャン。`ERROR_JOURNAL_DELETE_IN_PROGRESS` は削除が終わるまで待つ。
- `BytesToWaitFor=0` で即時返却（ポーリング）。ブロッキング読みは dismount 時のキャンセルが必要。
- 理由ビットはクローズまで累積し、`USN_REASON_CLOSE` のレコードに集約される。リネームは OLD_NAME → NEW_NAME の順、同じ FRN。FRN は seq 込みの 64bit で比較する（再利用対策）。
- ハードリンクの別名削除はジャーナルから特定できないことがある（Everything 1.5 も「削除されたハードリンクは追跡しない」）。
- VSS/BitLocker ロック/取り外しで下からディスマウントされる → `ERROR_NOT_READY` 等で読み直し。

## ボリューム検出
- `FindFirstVolume` → `GetVolumePathNamesForVolumeName` でマウントポイント、`GetVolumeInformation` で FS 名。識別子はボリューム GUID（同一ボリュームの多重マウントを 1 回に）。
- `DRIVE_FIXED` の NTFS/ReFS のみ。SUBST は `QueryDosDevice` で `\??\C:\path` に解決される。
- 切断された VPN のネットワークドライブが固定ディスクとして報告され、ボリューム情報 API が同期ブロックして Everything がハングした実例（`scan_volume_drive_letters` 設定で回避）。死にかけの USB でも同様。→ ボリューム問い合わせは別スレッド＋期限。
- UNC/切断ドライブへの `CreateFile`/`GetFileAttributes` はタイムアウト引数が無く数十秒固まる。
- ReFS（Dev Drive）は MFT が無く USN 列挙経路のみ。`\\wsl$` は 9P で索引不可。
- OneDrive 等のプレースホルダー: `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS`。データを開くと勝手にダウンロードされる。名前・サイズ・日付は MFT/USN だけから取り、データは開かない。
- スピンダウン中の HDD は最初の I/O に 5〜15 秒かかる。

## ディレクトリ走査のフォールバック（非 NTFS 用、未実装）
- `FindFirstFileExW(FindExInfoBasic, FIND_FIRST_EX_LARGE_FETCH)`、`\\?\` プレフィックス、`IO_REPARSE_TAG_MOUNT_POINT`/`SYMLINK` は降りない、訪問済み (volume GUID, FRN) で循環防止。

## Everything 1.4 のフリーズ既知要因
- 到達不能ネットワークドライブのボリューム情報取得、死にかけ USB、ジャーナルラップによる 10 分再スキャン、大量書き込み時の単一更新スレッド、巨大結果集合、未索引プロパティの遅延取得、クラウドファイルのダウンロード、x86 の 2GB 上限。

主な出典: learn.microsoft.com (winioctl FSCTL_ENUM_USN_DATA / READ_USN_JOURNAL_DATA_V0 / change journals), voidtools.com/support/everything/{everything_service,indexes,sdk}, voidtools forum p=18573, 33317, 62073, 67483, 70007, 70634, 74571, 78094.
