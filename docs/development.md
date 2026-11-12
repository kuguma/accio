# accio 開発の入口

コマンドはすべて mise のタスクに集約してある（`mise tasks` で一覧、各タスクの description が説明）。

- `mise run test`: ワークスペースのテスト。NTFS テストイメージ `testdata/testfs1.ntfs`（ntfs crate 由来、512B セクタ 2MiB）に対してスキャン→索引→検索まで通す。USN 適用は偽のレコード源で検証する。
- `mise run bench`: 合成 1500 万件での検索・保存・読込のレイテンシ計測（`docs/adr/0002` の数値の再現）。
- `mise run build`: リリースビルド。`target/release/accio.exe` は非昇格で起動すると自己昇格（UAC）する。
- `mise run run`: リリースビルドして起動する。ログは `%LOCALAPPDATA%\accio\accio.log`、索引は同ディレクトリの `*.accio`。

## 構成

- `crates/accio-ntfs`: Win32/NTFS 生アクセス（ブートセクタ、MFT レコード解釈、$MFT 一括読み、USN、ボリューム検出）。
- `crates/accio-index`: カラム指向のインメモリ索引、構築、永続化、USN 差分適用、フォルダサイズ、フォルダ索引（ディレクトリ走査）。
- `crates/accio-query`: Everything 構文パーサと並列評価器、ソート、重複検出。
- `crates/accio-app`: エンジン統括、egui GUI、トレイ/ホットキー、Everything 互換 IPC、HTTP サーバー、シェル連携。

## 昇格なしで検証できる道具

- `cargo run --release -p accio-query --example query_store -- %LOCALAPPDATA%\accio "<query>"`: 保存済み索引に対する検索と各列のメモリ内訳。
- `ACCIO_NO_ELEVATE=1`: 読み取り専用で GUI を起動（保存済み索引のみ、更新なし）。`-s <query>` `-sort size-descending` `-instance <name>` `-startup` を併用できる。
- `ACCIO_DATA_DIR=<dir>`: 設定・索引・ログの置き場所を切り替える（`folders=` だけ書いた ini を置けばフォルダ索引だけで起動できる）。
- Everything 公式の `es.exe` に `-instance <name>` を付けると IPC 経由で検索・列・ソート・EFU 出力が確認できる。
- `ACCIO_SHELLMENU_PROBE=<path>`: シェルコンテキストメニューの構築だけを試して終了する。
- `ACCIO_PREVIEW_PROBE=<path>` / `ACCIO_DND_PROBE=a;b` / `ACCIO_BULK_PREVIEW=find|repl|regex|case|p1;p2`: プレビュー読み込み・CF_HDROP 往復・一括リネームの置換を試して終了する。
- `-instance <name>` で起動した accio には、同じ `-instance` を付けた 2 つ目の accio から `-s` `-new-tab` `-toggle-window` `-exit` `-reindex` `-save-db` を転送できる。
- `ACCIO_FONT_Y=<f32>`: 日本語フォントのベースライン補正を試す。
- `python -I scripts/etp_client.py <host> <port> "<query>" [user] [password]`: ETP サーバーを Everything 1.4 クライアントと同じ手順で叩き、ftplib で LIST/MLSD も試す。
- `python -I scripts/ipc3_journal.py <instance>`: 索引ジャーナル（READ_JOURNAL ストリーム）を読む。`ACCIO_JOURNAL_PROBE=1` で合成の変更 3 件が入る。
- `python -I scripts/ipc3_client.py <instance> "<query>"`: Everything 1.5 SDK3 互換の IPC v3（名前付きパイプ）を SDK3 と同じ符号化で叩く。

## 権限分離（インデクサ）の試し方

`accio -indexer` はウィンドウなしのインデクサ。UI 側は accio.ini に `use_indexer=1` を書くと昇格せずに起動し、索引ファイルの更新を追従する。
非昇格で試すには `ACCIO_NO_ELEVATE=1 ACCIO_DATA_DIR=<dir>` と `folders=` で両方を起動し、`es.exe -instance ui` と `-instance accio-indexer` で件数を比べる。

## 昇格が必要な経路の検証

MFT スキャンと USN 追従は管理者権限でしか動かない。`scripts/dev-relaunch.ps1` を最高権限のタスク（`accio-dev`）として登録しておくと、
非昇格のセッションから `schtasks /Run /TN accio-dev` で「停止 → リリースビルド → 起動」が回せる。結果は `accio.log` で確認する。

## 検証の考え方（0.x）

内部モジュールの回帰テストは最小限にし、外側で検証する。

1. ユニット: 構文パーサ（`accio-query` の `#[cfg(test)]`）。
2. イメージ E2E: テストイメージで MFT 解釈〜検索の正しさ、USN 適用（`cargo test`）。管理者権限不要。
3. ライブ E2E（管理者権限が必要）: 実機で起動し、`accio.log` の各ボリュームのスキャン時間・件数、ステータスバーの「検索可能まで N s」、USN 反映（ファイルを作って検索に出るか）を確認する。
