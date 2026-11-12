# Using accio

accio accepts everything you already do in Everything 1.4. This page lists what is specific to accio plus the complete keyboard, query syntax, command-line and `accio.ini` reference. (日本語版: [usage.md](usage.md))

## Starting

- Running `accio.exe` asks for elevation (reading the raw MFT and the USN journal needs administrator rights). The first start scans every volume (about 13 s for a 17M-record volume); afterwards the saved index loads in about a second and search is available immediately.
- You can search while a scan is running; entries that have already arrived are matched.
- Settings and data live in `%LOCALAPPDATA%\accio` (`accio.ini`, `*.accio` indexes, `accio.log`, `Run History.csv`).
- To keep the UI unelevated, enable the privilege-separated indexer in Settings: `accio -indexer` runs headless with the highest privileges and the UI follows its index files (`use_indexer=1`).
- With an empty search box (and the filter set to "all") nothing is listed.

## The window

- The window is a search bar and results, nothing else. Every search option (match case, path, regex, drives, sort, filter, view, bookmarks, settings…) lives in the hamburger menu (≡) at the left of the search bar.
- **Pinned** (default): a normal window. The column table, preview pane, thumbnails and tabs are available, and it can stay open as a file browser.
- **Unpinned** (Ctrl+Shift+P, or "Pin" at the right of the search bar): a Spotlight-style floating panel. Only the search box appears near the top of the screen and results grow below it as you type. Esc, or touching anything else (losing focus), hides it; the hotkey or the tray icon brings it back.

## Keyboard

| Key | Action |
|---|---|
| Typing | Always goes to the search box, whatever has focus |
| ↑ ↓ / PgUp PgDn / Ctrl+Home Ctrl+End | Move the selection |
| Shift+↑↓ | Extend the selection (Ctrl+click / Shift+click also work) |
| Enter | Open the selected item (does nothing without a selection) |
| Ctrl+Enter | Open the containing folder |
| Alt+Enter | Properties |
| Esc / Alt+Home | Clear the search (hides the window when running in the tray) |
| Ctrl+Shift+C | Copy full path |
| Ctrl+C | Copy the files (paste into Explorer) |
| F2 / Del | Rename (bulk rename for multiple selection) / Recycle |
| Ctrl+I / Ctrl+U / Ctrl+B / Ctrl+R | Match case / path / whole word / regex |
| Ctrl+O / Ctrl+Shift+O | Open / close a file list (EFU or text) |
| Ctrl+S | Export results (efu / csv / txt) |
| Ctrl+D | Bookmarks |
| Ctrl+T / Ctrl+W / Ctrl+Tab / Ctrl+1–9 | Tabs: new / close / next / jump (right-click or middle-click a tab to close it) |
| Alt+P | Preview pane |
| Ctrl+Shift+1 / 2 / 3 / 6 | Thumbnails (small / medium / large) / details |
| Ctrl+P | Settings |
| Ctrl+Shift+P | Toggle pinning (normal window / floating panel) |
| F5 | Search again (reload the index when following an indexer) |
| Ctrl+Q | Quit |
| ctrl+alt+a (global) / double-click the tray icon | Show or hide the window (configurable) |
| Hamburger "Drives" | Choose which drives to search (saved as `drives_off=`) |

## Query syntax

All of Everything 1.4's syntax is accepted. `a b` is AND, `a|b` is OR, `!a` is NOT, `<…>` groups, `"…"` quotes a term with spaces.

- Modifiers: `case:` `nocase:` `path:` `nopath:` `wfn:` `ww:` `regex:` `file:` `folder:` `diacritics:` `nodiacritics:` `nowildcards:` (`ascii:` `utf8:` are accepted and ignored)
- Functions: `ext:` `size:` `dm:` `dc:` `da:` `dr:` `rc:` `parent:` `infolder:` `ancestor:` `child:` `empty:` `root:` `attrib:` `len:` `depth:` `startwith:` `endwith:` `count:` `type:` `content:` `dupe:` `sizedupe:` `namepartdupe:` `dmdupe:` `childcount:` `childfilecount:` `childfoldercount:` `frn:` `fsi:` `runcount:`
- Macros: `audio:` `zip:` `doc:` `exe:` `pic:` `image:` `video:`, plus any macro defined by a custom filter
- Dates: `today` `yesterday` `tomorrow` `thisweek` `lastweek` `nextweek` `thismonth` `lastmonth` `nextmonth` `thisyear` `lastyear` `nextyear` `monday`…`sunday` (the most recent one), `last3days` `past2weeks` `next7days` `coming1months`, `2024` `2024-05` `2024-05-06` `2024-05-06T13:45[:30]`, month names, `a..b` and `<` `<=` `>` `>=` `=` `!=`
- Sizes: `size:>1mb` `size:1kb..10mb` `size:empty` `size:tiny|small|medium|large|huge|gigantic`
- Diacritics are folded by default (`cafe` matches `Café`, `strasse` matches `Straße`); kana voicing marks are kept distinct.
- `rc:` is the time Everything observed a change; accio uses the modification time instead. `da:` uses the access time stored in `$STANDARD_INFORMATION` (NTFS usually stops updating it, so values are often old).

## Command line

```
accio [options]
  -s, -search <text>   search text
  -i, -case            match case
  -p, -match-path      match path
  -ww                  match whole word
  -r, -regex           regex
  -a, -diacritics      match diacritics
  -sort <name|path|size|dm|dc|da|ext|runcount|dr>[-descending]
  -filter <name>       filter
  -filelist <path>     open a file list (EFU / text) and search it
  -instance <name>     instance name (IPC identification)
  -startup             start in the tray without showing the window
  -new-window          start a new instance instead of forwarding to the running one
  -new-tab             open a new tab in the running instance
  -toggle-window / -exit / -reindex / -save-db   forwarded to the running instance
  -indexer             headless indexer (elevated, maintains the index only)
  -url <es:query>      URL protocol (register it in Settings)
```

Environment: `ACCIO_NO_ELEVATE=1` (read-only, saved indexes only, no elevation), `ACCIO_DATA_DIR=<dir>` (data location).

## `accio.ini`

Everything that can be changed in Settings (Ctrl+P) is stored under the same key. One `key=value` per line.

| Key | Meaning |
|---|---|
| `folders=D:\data;\\server\share` | Folder indexes for non-NTFS or arbitrary paths (`;` separated) |
| `folder_rescan_minutes=30` | Periodic rescan of folder indexes (0 = off; change watching is always on) |
| `exclude_hidden=1` `exclude_system=1` | Exclude hidden / system entries |
| `exclude_folders=C:\Windows\Temp;…` `exclude_files=*.tmp;*.log` | Excluded folders / files |
| `include_only=D:\work;E:\photos` | Restrict to these folders (Everything's Include only) |
| `filter.0=name|query|macro` | Custom filters (macro name optional; usable as `macro:` in queries) |
| `match_case=` `match_path=` `whole_word=` `regex=` `match_diacritics=` | Initial search options |
| `sort=name` `sort_ascending=1` `columns=name,path,size,dm,dc,da,ext,attrib,type,runcount,daterun` | Sort order and columns |
| `drives_off=D:;E:` | Drives excluded from search |
| `theme=0|1|2` `zoom=100` | Theme (system/light/dark) and display scale (%) |
| `color_highlight=C83C00` `color_selection=` `color_text=` `color_background=` | Highlight / selection / text / background colors (RRGGBB, empty = theme default) |
| `font=C:\Windows\Fonts\meiryo.ttc` `font_size=13` | Font file (used first; empty = Yu Gothic UI) and base font size (px) |
| `preview=1` `thumb_size=0|96|160|256` | Preview pane / thumbnails |
| `lang=ja|en` | UI language (empty = follow the Windows display language; takes effect after restart) |
| `run_in_background=1` `hotkey=ctrl+alt+a` | Stay in the tray on close/Esc, global hotkey |
| `restore_session=1` | Restore tabs and queries from the last session |
| `pinned=1` | Pinned (normal window). 0 starts as the Spotlight-style floating panel |
| `use_indexer=1` | Follow a separate indexer process (the UI stays unelevated) |
| `http_server=1` `http_port=80` `http_bind=` `http_user=` `http_password=` | HTTP server (Everything-compatible `?search=&json=1` etc., Basic auth) |
| `etp_server=1` `etp_port=21` `etp_bind=` `etp_user=` `etp_password=` `etp_allow_download=1` | ETP/FTP server (the `EVERYTHING` command set) |

Exclusions apply to IPC / HTTP / ETP searches too, exactly as if the index itself excluded them.

## Integration

- Responds to Everything IPC v1/v2 (the `EVERYTHING_TASKBAR_NOTIFICATION` window, `-instance` suffixes supported) and IPC v3 (the Everything 1.5 SDK 3 named pipe), including the index journal (`GET_JOURNAL_INFO` / `READ_JOURNAL`). The official `es.exe` works unchanged.
- The Explorer folder context menu entry and the `es:` URL protocol are registered / unregistered from Settings (HKCU only).
- Result rows can be dragged into Explorer; the right-click menu is Explorer's own shell menu.
