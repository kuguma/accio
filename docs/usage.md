# accio の使い方

（English: [usage.en.md](usage.en.md)）

Everything 1.4 の操作がそのまま通じるように作ってある。ここでは accio 固有の点と、設定ファイル・コマンドライン・検索構文の一覧をまとめる。

## 起動

- `accio.exe` を起動すると UAC で昇格を求める（MFT の生読みと USN ジャーナル監視に管理者権限が要る）。初回は全ボリュームをスキャンし（C: 約 9 秒、1800 万件で合計 1 分弱）、以後は保存済み索引を 0.1 秒以内に読み込んで検索可能になる。
- スキャン中も検索できる（到着済みの分がヒットする）。
- 検索語が空（かつフィルタが「すべて」）のときは何も表示しない。
- 設定とデータは `%LOCALAPPDATA%\accio`（`accio.ini`、`*.accio` 索引、`accio.log`、`Run History.csv`、`Search History.csv`）。
- UI を昇格させたくないときは設定の「昇格インデクサ」を使う（`-indexer` を最高権限のタスクで常駐させ、UI は `use_indexer=1` で索引ファイルを追従する）。

## キーボード

| キー | 動作 |
|---|---|
| 文字入力 | 常に検索窓へ入る（どこにフォーカスがあっても） |
| ↑ ↓ / PgUp PgDn / Ctrl+Home Ctrl+End | 選択の移動 |
| Shift+↑↓ | 範囲選択の拡張（Ctrl+クリック / Shift+クリックも可） |
| Enter | 選択したものを開く（選択なしなら何もしない） |
| Ctrl+Enter | 選択したもののフォルダを開く |
| Alt+Enter | プロパティ |
| Esc / Alt+Home | 検索語をクリア（常駐設定時はウィンドウを隠す） |
| Ctrl+Shift+C | フルパスをコピー |
| Ctrl+C | ファイルをコピー（Explorer に貼り付けられる） |
| F2 / Del | 名前の変更（複数選択なら一括リネーム） / ごみ箱へ移動 |
| Ctrl+I / Ctrl+U / Ctrl+B / Ctrl+R | 大小区別 / パス一致 / 単語一致 / 正規表現 |
| Ctrl+O / Ctrl+Shift+O | ファイルリスト（EFU/テキスト）を開く / 閉じる |
| Ctrl+S | 結果のエクスポート（efu / csv / txt） |
| Ctrl+D | ブックマーク |
| Ctrl+T / Ctrl+W / Ctrl+Tab / Ctrl+1〜9 | タブの追加 / 閉じる / 切り替え |
| Alt+P | プレビューペイン |
| Ctrl+Shift+1 / 2 / 3 / 6 | サムネイル（小 / 中 / 大） / 詳細表示 |
| Ctrl+P | 設定 |
| F5 | 再検索（インデクサ追従時は索引の再読込） |
| Ctrl+Q | 終了 |
| ctrl+alt+a（グローバル） / トレイアイコンのダブルクリック | ウィンドウの表示/非表示（設定で変更） |
| ツールバー「ドライブ」 | 検索対象のドライブを選ぶ（`drives_off=` に保存） |

## 検索構文

Everything 1.4 の構文をすべて受け付ける。`a b` は AND、`a|b` は OR、`!a` は NOT、`<…>` でグループ、`"…"` で空白を含む語。

- 修飾子: `case:` `nocase:` `path:` `nopath:` `wfn:` `ww:` `regex:` `file:` `folder:` `diacritics:` `nodiacritics:` `nowildcards:`（`ascii:` `utf8:` は受理して無視）
- 関数: `ext:` `size:` `dm:` `dc:` `da:` `dr:` `rc:` `parent:` `infolder:` `ancestor:` `child:` `empty:` `root:` `attrib:` `len:` `depth:` `startwith:` `endwith:` `count:` `type:` `content:` `dupe:` `sizedupe:` `namepartdupe:` `dmdupe:` `childcount:` `childfilecount:` `childfoldercount:` `frn:` `fsi:` `runcount:`
- マクロ: `audio:` `zip:` `doc:` `exe:` `pic:` `image:` `video:`、および設定のカスタムフィルタで定義したマクロ
- 日付: `today` `yesterday` `tomorrow` `thisweek` `lastweek` `nextweek` `thismonth` `lastmonth` `nextmonth` `thisyear` `lastyear` `nextyear` `monday`〜`sunday`（今日以前の直近）、`last3days` `past2weeks` `next7days` `coming1months`、`2024` `2024-05` `2024-05-06` `2024-05-06T13:45[:30]`、月名、`a..b` と `<` `<=` `>` `>=` `=` `!=`
- サイズ: `size:>1mb` `size:1kb..10mb` `size:empty` `size:tiny|small|medium|large|huge|gigantic`
- 既定ではダイアクリティカルマークを区別しない（`cafe` は `Café` にも一致、`strasse` は `Straße` にも一致）。かなの濁点は区別する。
- `rc:` は Everything では「変更を観測した日時」だが、accio は更新日時で代用する。`da:`（アクセス日時）は $STANDARD_INFORMATION の値をそのまま索引する（NTFS 既定では OS がアクセス日時の更新を抑制するので、古い値のことが多い）。

## コマンドライン

```
accio [options]
  -s, -search <text>   検索語
  -i, -case            大小区別
  -p, -match-path      パス一致
  -ww                  単語一致
  -r, -regex           正規表現
  -a, -diacritics      ダイアクリティカルマークを区別
  -sort <name|path|size|dm|dc|da|ext|runcount|dr>[-descending]
  -filter <name>       フィルタ
  -filelist <path>     ファイルリスト（EFU/テキスト）を開いて検索
  -instance <name>     インスタンス名（IPC の識別）
  -startup             トレイ常駐で開始（ウィンドウを出さない）
  -new-window          起動中のインスタンスへ転送せず新しく起動
  -new-tab             起動中のインスタンスに新しいタブで検索
  -toggle-window / -exit / -reindex / -save-db   起動中のインスタンスへ転送
  -indexer             ウィンドウなしのインデクサ（昇格して索引だけを保守）
  -url <es:検索語>     URL プロトコル（設定で登録）
```

環境変数: `ACCIO_NO_ELEVATE=1`（昇格せず保存済み索引だけで読み取り専用）、`ACCIO_DATA_DIR=<dir>`（データの置き場所）。

## 設定ファイル `accio.ini`

設定ウィンドウ（Ctrl+P）で変えられるものは同じキーに保存される。`key=value` を 1 行ずつ。

| キー | 意味 |
|---|---|
| `folders=D:\data;\\server\share` | NTFS 以外や任意フォルダをフォルダ索引にする（`;` 区切り） |
| `folder_rescan_minutes=30` | フォルダ索引の定期再走査（0 で無効。変更監視は常時） |
| `exclude_hidden=1` `exclude_system=1` | 隠し / システムを除外 |
| `exclude_folders=C:\Windows\Temp;…` `exclude_files=*.tmp;*.log` | 除外フォルダ / 除外ファイル |
| `include_only=D:\work;E:\photos` | 対象フォルダの限定（Everything の Include only） |
| `filter.0=名前|検索式|マクロ` | カスタムフィルタ（マクロ名は省略可。`マクロ:` として検索語に書ける） |
| `match_case=` `match_path=` `whole_word=` `regex=` `match_diacritics=` | 検索オプションの初期値 |
| `sort=name` `sort_ascending=1` `columns=name,path,size,dm,dc,da,ext,attrib,type,runcount,daterun` | 並び順と列 |
| `theme=0|1|2` `zoom=100` | 配色（システム/ライト/ダーク）と表示倍率（%） |
| `color_highlight=C83C00` `color_selection=` `color_text=` `color_background=` | 検索語の強調色 / 選択行 / 文字 / 背景（RRGGBB、空で配色の既定） |
| `font=C:\Windows\Fonts\meiryo.ttc` `font_size=13` | フォントファイル（最優先で使う。空なら Segoe UI + Yu Gothic）と基準の文字サイズ（px） |
| `preview=1` `thumb_size=0|96|160|256` | プレビューペイン / サムネイル |
| `run_in_background=1` `hotkey=ctrl+alt+a` | 閉じる/Esc で常駐、グローバルホットキー |
| `restore_session=1` | 終了時のタブと検索語を復元 |
| `use_indexer=1` | 別プロセスのインデクサの索引を追従する（UI は昇格しない） |
| `http_server=1` `http_port=80` `http_bind=` `http_user=` `http_password=` | HTTP サーバー（Everything 互換の `?search=&json=1` など、Basic 認証） |
| `etp_server=1` `etp_port=21` `etp_bind=` `etp_user=` `etp_password=` `etp_allow_download=1` | ETP/FTP サーバー（Everything の `EVERYTHING` コマンド群） |

除外設定は UI の検索だけでなく IPC / HTTP / ETP の検索にも効く（Everything と同じく索引そのものが除外されているように見える）。

## 連携

- Everything SDK / es.exe / 他アプリからの IPC v1・v2（`EVERYTHING_TASKBAR_NOTIFICATION` ウィンドウ、`-instance` 付きも可）と IPC v3（Everything 1.5 SDK3 の名前付きパイプ）に応答する。索引ジャーナル（`GET_JOURNAL_INFO` / `READ_JOURNAL`）も配信する。
- Explorer のフォルダ右クリック「accio で検索」と `es:` URL プロトコルは設定ウィンドウから登録/解除する（HKCU のみ）。
- 結果の行は Explorer にドラッグ＆ドロップできる。右クリックメニューは Explorer と同じシェルメニュー。
