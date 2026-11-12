# Everything 1.4 / 1.5 機能一覧と accio の対応状況（2026-11-12 01:20 JST）

Web 調査の要約（出典: voidtools.com/support/everything/{searching,search_syntax,search_functions,sdk,http,etp,command_line_options,indexes}, forum t=9787）。
右列は現時点の accio の状態。◯ 実装済み / △ 部分 / ✗ 未実装。

## 検索構文
| 機能 | Everything | accio |
|---|---|---|
| 空白 AND, `\|` OR（AND より強く結合）, `!` NOT, `< >` グループ, `""` 引用 | 1.4 | ◯ |
| ワイルドカード `*` `?`（ファイル名全体一致） | 1.4 | ◯ |
| 正規表現 `regex:` / Ctrl+R | 1.4 | ◯（regex crate; PCRE 完全互換ではない） |
| 修飾子 case: nocase: path: nopath: wfn: ww: file: folder: | 1.4 | ◯ |
| diacritics: ascii: utf8: wildcards: | 1.4 | ◯ diacritics:/nodiacritics:（既定は折り畳み: é=e, ß=ss, 結合文字除去。「é 区別」トグル、IPC 0x10、ETP DIACRITICS、HTTP diacritics、-a）、nowildcards:。ascii:/utf8: は受理して無視 |
| ext: size: dm: dc: da: | 1.4 | ◯（da: は索引形式 v4 でアクセス日時列を追加） |
| parent:/infolder:/nosubfolders: ancestor: child: empty: root: attrib: len: depth: startwith: endwith: count: | 1.4 | ◯ |
| サイズ定数 empty/tiny/…/gigantic、kb/mb/gb/tb、範囲 `..`、比較 `< <= = > >=` | 1.4 | ◯ |
| 日付定数 today/yesterday/thisweek/lastweek/thismonth/lastmonth/thisyear/lastyear/lastNdays、YYYY[-MM[-DD]]、月名 | 1.4 | ◯ |
| 曜日名、`coming`/`next`、時刻付き ISO | 1.4/1.5 | ◯（曜日は今日以前の直近、`YYYY-MM-DDTHH:MM[:SS]`） |
| dupe: sizedupe: namepartdupe: dmdupe:（全ボリューム横断）childcount: childfilecount: childfoldercount: | 1.4 | ◯ |
| content:（候補ファイルを読む、上限 20 万件/32MB）type:（シェル種別名） | 1.4 | ◯ |
| frn: fsi: rc: runcount: dr: da: | 1.4 | ◯ frn:/fsi:/runcount:/dr:（実行履歴から）、rc: は更新日時で代用、da: はアクセス日時列から |
| 1.5 の `name:` `stem:` `size-on-disk:` `child-size:` `descendant-count:` `sort:` 等 | 1.5 | ✗（size: はフォルダサイズにも効く） |
| マクロ audio: doc: exe: image: video: zip: | 1.4 | ◯（カスタムフィルタのマクロも `名前|検索式|マクロ` で定義し `マクロ:` として使える） |

## UI
| 機能 | accio |
|---|---|
| インクリメンタル検索、結果の仮想リスト、名前/パス/サイズ/更新日時列 | ◯ |
| ソート（名前/パス/サイズ/更新/作成/拡張子/実行回数、ヘッダクリック） | ◯（件数に関わらず常にソート。名前順は順列で O(N)、パス順は親フォルダごとの順位付け） |
| Match Case/Path/Whole Word/Regex のトグルとショートカット（Ctrl+I/U/B/R） | ◯ |
| ダブルクリックで開く、右クリック（開く/パスを開く/フルパスコピー/プロパティ） | ◯ |
| Ctrl+Shift+C フルパスコピー、Esc クリア、Enter で開く、↑↓/PgUp/PgDn | ◯ |
| 容量ビュー（フォルダサイズ降順、ダブルクリックで掘り下げ、↑で親へ）、最大ファイル | ◯（Everything に無い機能） |
| 索引構築中の検索（到着済み分がヒット） | ◯（Everything 1.4 に無い機能） |
| 作成日時/アクセス日時/属性/拡張子/種類/実行回数/実行日時列、列の追加削除（ヘッダ右クリック/設定） | ◯ |
| 検索語ハイライト（名前列、パス一致時はパス列も） | ◯ |
| フィルタバー（組み込み＋カスタム、-filter）、ドライブ選択、ブックマーク (Ctrl+D)、実行履歴（CSV、実行回数列/ソート） | ◯ |
| プレビューペイン (Alt+P)、サムネイル表示（Ctrl+Shift+1/2/3、Ctrl+Shift+6 で詳細） | ◯ |
| トレイアイコン、グローバルホットキー（ctrl+alt+a）、閉じる/Esc で常駐、ログオン時自動起動 | ◯ |
| タブ（Ctrl+T/W/Tab、Ctrl+1〜9、-new-tab）、セッション復元（終了時のタブと検索語）、二重起動時のコマンド転送、-new-window | ◯ |
| エクスポート (efu/csv/txt, Ctrl+S)、ファイルリストを開く (Ctrl+O / -filelist) / 閉じる (Ctrl+Shift+O) | ◯（EFU と 1 行 1 パスのテキスト。開いている間はリストだけが検索対象） |
| ごみ箱へ移動 (Del)、リネーム (F2)、一括リネーム、複数選択、ファイルのコピー (Ctrl+C)、Explorer のシェルメニュー（IContextMenu3）、結果からのドラッグ＆ドロップ (OLE CF_HDROP) | ◯ |
| 設定ウィンドウ (Ctrl+P): 常駐/ホットキー/HTTP/列/ログオン時自動起動/除外/カスタムフィルタ/表示倍率/配色 | ◯ |
| 配色（システム/ライト/ダーク）、表示倍率（80〜200%）、フォントファイル/文字サイズ、色（強調/選択/文字/背景）、ウィンドウ位置/サイズの記憶 | ◯ |

## 索引
| 機能 | accio |
|---|---|
| NTFS 固定ディスクの自動検出、GUID で一意化 | ◯ |
| MFT 一括読み（名前・サイズ・更新/作成日時・属性・ハードリンク全名） | ◯ |
| USN ジャーナル監視（作成/削除/リネーム/移動/サイズ更新） | ◯（USN ジャーナル無ければ 128MB で作成） |
| ジャーナルラップ時のバックグラウンド再スキャンと差し替え | ◯ |
| 索引の永続化と高速ロード、定期保存（10 分）と終了時保存 | ◯ |
| フォルダサイズ（配下合計）の索引と実時間更新 | ◯ |
| ReFS / Dev Drive | ◯（Dev Drive の VHDX で実機確認。初回はディレクトリ列挙、更新は USN V3、ID→合成レコード番号。`2026-11-12-research-refs.md`） |
| フォルダ索引（任意フォルダ/UNC、ReadDirectoryChangesW で変更時に再走査、リパースポイントは降りない） | ◯（定期再走査は `folder_rescan_minutes=`、設定ウィンドウから） |
| 昇格インデクサ（`-indexer`、ログオン時タスク）＋非昇格 UI（`use_indexer=1`、索引ファイル追従） | ◯（Everything Service とは方式が異なる。反映は約 10 秒遅れ） |
| 除外（隠し/システム/除外フォルダ/ワイルドカード除外ファイル）、Include only（対象フォルダの限定） | ◯（UI だけでなく IPC/HTTP/ETP の検索にも効く） |
| 内容（content）索引、プロパティ索引 | 対象外（Everything 1.5 alpha の機能。1.4 相当の `content:` 検索は ◯） |

## 連携
| 機能 | accio |
|---|---|
| IPC v1/v2（WM_COPYDATA）: 公式 es.exe で確認。IPC v3（名前付きパイプ、SDK3 互換: 検索/プロパティ/フォルダサイズ/属性/FindFirstFile/変更待ち）: SDK3 の符号化を再現したクライアントで確認 | ◯（索引ジャーナル GET_JOURNAL_INFO/READ_JOURNAL も対応。Run Count / Date Run は実行履歴から） |
| es.exe 相当のコマンドライン | ◯（公式 es.exe がそのまま使える） |
| HTTP サーバー（JSON/HTML、Everything 互換パラメータ、Basic 認証）、ETP/FTP サーバー（EVERYTHING コマンド群と QUERY、LIST/MLSD/SIZE/MDTM/RETR） | ◯（QUERY 実行中のコマンドで 426 中断も対応） |
| コマンドラインオプション | ◯（-search/-s, -case/-i, -match-path/-p, -ww, -regex/-r, -diacritics/-a, -sort, -filter, -filelist, -instance, -startup, -new-window, -new-tab, -toggle-window, -exit, -reindex, -save-db） |
| Explorer のフォルダ右クリック「accio で検索」、`es:` URL プロトコル（設定で登録/解除、HKCU） | ◯ |
