# 調査メモ: Everything 1.5 IPC v3（名前付きパイプ）プロトコル（2026-11-12 05:40 JST）

出典: github.com/voidtools/everything_sdk3 の `src/Everything3.c`（v3.0.1.12）と `include/Everything3.h`。`crates/accio-app/src/ipc3.rs` はこの要約に従う。

## 接続
- パイプ名 `\\.\PIPE\Everything IPC Session <sid>[ (<instance>)]`。`<sid>` は `ProcessIdToSessionId`、インスタンス名の `% : \` は `%XX` に escape。見つからなければ ` Session N` なしで再試行。
- クライアントは `GENERIC_READ|GENERIC_WRITE`、共有なし、`FILE_FLAG_OVERLAPPED`。バイトモードで読む。ハンドシェイクなし。
- ヘッダとペイロードは別々の `WriteFile` で送られるので、サーバーは 8 バイト読んでから `size` バイト読む。

## フレーミング
- `{code:u32, size:u32}` ＋ `data[size]`（リトルエンディアン、パック、UTF-8、NUL なし）。
- 応答 code: 200 完了、100 続きあり（ストリーム）、400 bad request、401 cancelled、404 not found、500 server、501 invalid command。
- VLQ: `v<0xFF: u8` / `0xFF,u16(v-0xFF)` / `0xFF,0xFFFF,u32(v-0xFF-0xFFFF)` / `…,u64`。
- SIZE_T は検索フラグ 0x4000 で u64、なければ u32。

## コマンド
0 GET_IPC_PIPE_VERSION, 1〜4 バージョン, 5 TARGET_MACHINE(2=x64), 6 FIND_PROPERTY_FROM_NAME(UTF-8 名 → u32 id、無ければ 0xFFFFFFFF), 7 SEARCH, 8 IS_DB_LOADED, 9/10 IS_PROPERTY_INDEXED/FAST_SORT(u32 id), 11/12 PROPERTY_NAME/CANONICAL_NAME(生 UTF-8), 13 PROPERTY_TYPE, 14 IS_RESULT_CHANGE, 15/16/17 RUN_COUNT(get/set/inc), 18 GET_FOLDER_SIZE(u64, 不明は全 1), 19 GET_FILE_ATTRIBUTES(u32), 20 GET_FILE_ATTRIBUTES_EX(win32_find_data 36B ＋名前), 21 GET_FIND_FIRST_FILE(各要素 find_data ＋ VLQ 名), 22 GET_RESULTS, 23 SORT, 24 WAIT_FOR_RESULT_CHANGE, 25/26/27 プロパティ表示属性, 28/29 ジャーナル（未実装）。
- `win32_find_data` = u64 created, accessed, modified, size, u32 attributes（未索引は全 1 / 0）。

## SEARCH 要求
`u32 flags, VLQ len, UTF-8 text, SIZE_T offset, SIZE_T count, VLQ nsort, {u32 id, u32 flags(1=desc)}[], VLQ nprop, {u32 id, u32 flags(1=format,2=highlight)}[]`
- flags: 1 case, 2 wholeword, 4 path, 8 regex, 0x10 diacritics, 0x20 prefix, 0x40 suffix, 0x80 ignore punct, 0x100 ignore ws, 0x600 folders-first (0 asc / 0x200 always / 0x400 never / 0x600 desc), 0x800 total size, 0x1000 hide omissions, 0x2000 sort mix, 0x4000 64bit, 0x8000 force。

## SEARCH 応答
`u32 valid_flags, SIZE_T folders, SIZE_T files, [u64 total_size if 0x800], SIZE_T offset, SIZE_T count, VLQ nsort, {id,flags}[], VLQ nprop, {id, flags, u8 value_type}[], item[count]: u8 item_flags(1 folder, 2 root), values[]`
- 値型: 3 DWORD(u32), 5 UINT64(u64), 10/14/15 文字列(VLQ len + UTF-8), 11 SIZE_T。format/highlight 指定時は常に文字列。
- プロパティ: 0 name(10), 1 path(14), 2 size(5), 3 extension(10), 4 type(10), 5 dm(5), 6 dc(5), 7 da(5), 8 attributes(3), 9 drc(5), 10 run-count(3), 11 date-run(5), 12 file-list-name(10), 41 size-on-disk(5), 48 name-length(11), 49 full-path-length(11), 240 full-path(15)。
- ハイライトは `*text*`（`*` 自体は `**`）。
