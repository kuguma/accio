# 調査メモ: Everything ETP/FTP サーバープロトコル（2026-11-12 06:30 JST）

出典: voidtools/etp_server（1.5 用公式プラグイン、MIT）の `src/etp_server.c`、公式 ETP ページ、フォーラムの 1.4 クライアントログ（t=6154, t=8999, t=6230）。`crates/accio-app/src/etp.rs` はこれに従う。

## 接続
- TCP 21 既定、TLS なし、行は CRLF、制御接続は UTF-8。バナー `220 …`。
- ログイン: `USER x` → パスワードが必要なら `331 Password required for x`、不要なら即 `230 Logged on.`。`PASS` → `230 Logged on.` / `530 Login or password incorrect!`。未ログインの他コマンドは `530 Not logged on.`。
- 1.4 クライアントが送る順序: USER/PASS → `OPTS UTF8 ON` → `EVERYTHING <option>` 群（SIZE_COLUMN, DATE_MODIFIED_COLUMN, DATE_CREATED_COLUMN, PATH_COLUMN, COUNT <表示行数>, PATH, HIDE_EMPTY_SEARCH_RESULTS） → `EVERYTHING SEARCH <text>` → `EVERYTHING SORT …`（既定以外のとき） → `EVERYTHING QUERY`。FEAT/SYST/TYPE/PWD は送らない。`EVERYTHING` と `SITE EVERYTHING` は同じ。

## 応答文字列（抜粋）
- `FEAT`: `211-Features:` / ` MDTM` / ` REST STREAM` / ` SIZE` / ` MLST type*;size*;modify*;` / ` MLSD` / ` UTF8` / ` EVERYTHING` / `211 End`
- `SYST`: `215 UNIX emulated by Everything`、`OPTS UTF8 ON`: `200 UTF8 mode enabled.`、`PWD`: `257 "/C:/path" is current directory.`、`CWD`: `250 CWD successful. "…" is current directory.` / `550 Directory not found.`
- `PASV`: `227 Entering Passive Mode. (h1,h2,h3,h4,p1,p2)`（Mode の後にピリオド）、`EPSV`: `229 Entering Extended Passive Mode (|||port|)`
- `SIZE`: `200 N`（213 ではない）、`MDTM`: `213 YYYYMMDDHHMMSS.mmm`、`LIST`: `150 Data connection accepted; transfer starting.` … `226 Transfer complete.`
- LIST 行: `%cr--r--r-- 1 ftp ftp %13llu Mon DD HH:MM|YYYY name`

## EVERYTHING サブコマンド（すべて `200 … set to (v).`）
CASE / WHOLE_WORD / PATH / DIACRITICS / PREFIX / SUFFIX / IGNORE_PUNCTUATION / IGNORE_WHITESPACE / REGEX / HIDE_EMPTY_SEARCH_RESULTS / SEARCH / FILTER_* / SORT（NAME, PATH, SIZE, EXTENSION, DATE_CREATED, DATE_MODIFIED, ATTRIBUTES, FILE_LIST_FILENAME, DATE_RECENTLY_CHANGED ＋ `_ASCENDING|_DESCENDING`、`INVERSE_SIZE_*`。不明は `500 Unknown sort type.`）/ OFFSET / COUNT / SIZE_COLUMN / ATTRIBUTES_COLUMN / DATE_MODIFIED_COLUMN / DATE_CREATED_COLUMN / PATH_COLUMN / FILE_LIST_FILENAME_COLUMN / DATE_RECENTLY_CHANGED_COLUMN / QUERY。不明は `500 Unknown Everything command.`。

## QUERY 応答（制御接続上、データ接続は使わない）
```
200-Query results
 RESULT_COUNT <総件数>
 [ PATH <親フォルダ、Windows パス>]
 [ ATTRIBUTES <u32>]
 [ SIZE <u64、不明は 18446744073709551615>]
 [ DATE_MODIFIED <FILETIME u64 10 進>]
 [ DATE_CREATED <FILETIME>]
 [ FILE_LIST_FILENAME <path>]
 FOLDER <名前> | FILE <名前>      ← 1 件の終端
 …
200 End.
```
OFFSET/COUNT で切り出し、RESULT_COUNT は常に全件数。パスは `C:\dir` 形式（クライアントが `\\host\C` 等に書き換える）。

## 注意
- QUERY 実行中の別コマンドには `426 Query aborted.`、データ転送中は `503 Invalid sequence of commands.`。
- クライアントはキー入力ごと・スクロール（OFFSET）・リサイズ（COUNT）で QUERY を送るので、同一検索のキャッシュが要る。
- Windows ファイアウォールの FTP 検査でポート 21 の大きな応答が切られることがある（非 21 ポート推奨）。
