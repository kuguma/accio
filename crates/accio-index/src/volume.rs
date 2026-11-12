//! 1 ボリューム分の索引本体。

pub type EntryId = u32;
pub const NONE: u32 = u32::MAX;

/// `flags` のビット。下位 16bit はそのまま FILE_ATTRIBUTE_* を保持しない（`attrs` が別にある）。
pub const FLAG_DELETED: u16 = 0x1;
/// 名前が `overflow` にある（`orig_off` が overflow の添字。`name_off` は旧スロットのまま単調性を保つ）
pub const FLAG_OVERFLOW: u16 = 0x2;

pub const ATTR_DIRECTORY: u32 = 0x10;
pub const ATTR_REPARSE_POINT: u32 = 0x400;
pub const ATTR_HIDDEN: u32 = 0x2;
pub const ATTR_SYSTEM: u32 = 0x4;

#[derive(Debug, Clone, Default)]
pub struct VolumeMeta {
    pub guid_path: String,
    pub display_root: String,
    pub serial: u32,
    pub journal_id: u64,
    pub next_usn: i64,
    /// MFT レコード数（`by_recno` の長さ）
    pub mft_records: u64,
}

#[derive(Default)]
pub struct VolumeIndex {
    pub meta: VolumeMeta,
    // ---- 検索ホット列（起動時にまず読み込む）----
    /// 小文字化名の連続バッファ。エントリ順に追記される。
    pub arena: Vec<u8>,
    pub name_off: Vec<u32>,
    pub name_len: Vec<u16>,
    pub parent: Vec<EntryId>,
    pub flags: Vec<u16>,
    pub attrs: Vec<u32>,
    // ---- コールド列 ----
    /// 原綴り名（小文字化で変化した名前のみ）。`orig_off == NONE` なら arena と同一。
    pub orig_off: Vec<u32>,
    pub orig_len: Vec<u16>,
    pub orig_arena: Vec<u8>,
    pub size: Vec<u64>,
    /// 更新/作成日時（UTC、1970 起点の秒。FILETIME は `mtime()` / `ctime()` で得る）
    pub mtime_s: Vec<u32>,
    pub ctime_s: Vec<u32>,
    /// 最終アクセス日時（同上）
    pub atime_s: Vec<u32>,
    /// MFT レコード番号（下位 32bit）とシーケンス番号。FRN は `frn()` で得る
    pub recno: Vec<u32>,
    pub seq: Vec<u16>,
    /// MFT レコード番号 → 先頭エントリ
    pub by_recno: Vec<EntryId>,
    /// 伸びるリネームで主アリーナに収まらなかった名前 (lower, orig)
    pub overflow: Vec<(String, String)>,
    pub live: usize,
    /// 名前順（小文字化名、同名はエントリ順）の順列。先頭 `order_covers` 件のエントリだけを含む。
    /// 空なら未計算。追記されたエントリ（`order_covers` 以降）は検索側で別途ソートして合流させる。
    pub order_name: Vec<EntryId>,
    pub order_covers: usize,
    /// 同じ MFT レコード番号を持つエントリが複数ある（ハードリンク）場合の、先頭以外のエントリ
    pub multi: std::collections::HashMap<u32, Vec<EntryId>>,
    /// false の間は `parent` 列が MFT レコード番号の仮置きであり、パス再構成はできない。
    pub resolved: bool,
    /// フォルダの構造（追加/改名/移動/削除）が変わるたびに増える。パス順の順位キャッシュの鍵。
    pub dir_epoch: u64,
    /// ReFS: 合成レコード番号 → ファイル ID（lo, hi の u64 列）。NTFS では空。
    pub refs_ids: Vec<u64>,
}

pub struct ResolvedName<'a> {
    pub lower: &'a [u8],
    pub orig: &'a [u8],
}

const UNIX_EPOCH_FT: i64 = 116_444_736_000_000_000;

/// FILETIME（100ns, 1601 起点）→ 1970 起点の秒（u32、範囲外は端に丸める）
#[inline]
pub fn filetime_to_secs(ft: i64) -> u32 {
    let s = (ft - UNIX_EPOCH_FT).div_euclid(10_000_000);
    s.clamp(0, u32::MAX as i64) as u32
}

/// 1970 起点の秒 → FILETIME
#[inline]
pub fn secs_to_filetime(s: u32) -> i64 {
    s as i64 * 10_000_000 + UNIX_EPOCH_FT
}

impl VolumeIndex {
    pub fn new(meta: VolumeMeta) -> Self {
        let mut v = Self { meta, resolved: true, ..Default::default() };
        let root = v.meta.display_root.clone();
        v.push_raw(&root, NONE, ATTR_DIRECTORY, 0, 0, 0, 5 | (5u64 << 48));
        v
    }

    pub fn len(&self) -> usize {
        self.parent.len()
    }
    pub fn is_empty(&self) -> bool {
        self.parent.is_empty()
    }
    pub fn live_count(&self) -> usize {
        self.live
    }

    #[inline]
    pub fn is_deleted(&self, i: EntryId) -> bool {
        self.flags[i as usize] & FLAG_DELETED != 0
    }
    #[inline]
    pub fn is_dir(&self, i: EntryId) -> bool {
        self.attrs[i as usize] & ATTR_DIRECTORY != 0
    }
    /// 更新日時（FILETIME）
    #[inline]
    pub fn mtime(&self, i: EntryId) -> i64 {
        secs_to_filetime(self.mtime_s[i as usize])
    }
    /// 作成日時（FILETIME）
    #[inline]
    pub fn ctime(&self, i: EntryId) -> i64 {
        secs_to_filetime(self.ctime_s[i as usize])
    }
    #[inline]
    pub fn atime(&self, i: EntryId) -> i64 {
        secs_to_filetime(self.atime_s[i as usize])
    }
    #[inline]
    pub fn set_atime(&mut self, i: EntryId, ft: i64) {
        self.atime_s[i as usize] = filetime_to_secs(ft);
    }
    /// MFT 参照番号（recno | seq<<48）
    #[inline]
    pub fn frn(&self, i: EntryId) -> u64 {
        self.recno[i as usize] as u64 | ((self.seq[i as usize] as u64) << 48)
    }
    #[inline]
    pub fn set_mtime(&mut self, i: EntryId, ft: i64) {
        self.mtime_s[i as usize] = filetime_to_secs(ft);
    }

    /// 小文字化済み名（検索用）。
    pub fn lower_name(&self, i: EntryId) -> &[u8] {
        let i = i as usize;
        if self.flags[i] & FLAG_OVERFLOW != 0 {
            self.overflow[self.orig_off[i] as usize].0.as_bytes()
        } else {
            let o = self.name_off[i] as usize;
            &self.arena[o..o + self.name_len[i] as usize]
        }
    }

    /// 表示用の原綴り名。
    pub fn name(&self, i: EntryId) -> &str {
        let iu = i as usize;
        if self.flags[iu] & FLAG_OVERFLOW != 0 {
            return &self.overflow[self.orig_off[iu] as usize].1;
        }
        let b = if self.orig_off[iu] == NONE {
            self.lower_name(i)
        } else {
            let o = self.orig_off[iu] as usize;
            &self.orig_arena[o..o + self.orig_len[iu] as usize]
        };
        // SAFETY: アリーナには常に有効な UTF-8 の名前だけを書き込む（push_raw）。
        unsafe { std::str::from_utf8_unchecked(b) }
    }

    /// 親ディレクトリのフルパス（末尾 `\` なし）。ルート直下なら `C:`。親未解決なら `?`。
    pub fn parent_path(&self, i: EntryId) -> String {
        if !self.resolved {
            return "…".to_string();
        }
        let mut parts: Vec<&str> = Vec::with_capacity(16);
        let mut p = self.parent[i as usize];
        let mut guard = 0;
        while p != NONE && guard < 512 {
            parts.push(self.name(p));
            p = self.parent[p as usize];
            guard += 1;
        }
        if parts.is_empty() {
            return if i == 0 { String::new() } else { "?".to_string() };
        }
        parts.reverse();
        parts.join("\\")
    }

    pub fn full_path(&self, i: EntryId) -> String {
        if i == 0 {
            return format!("{}\\", self.name(0));
        }
        let pp = self.parent_path(i);
        format!("{}\\{}", pp, self.name(i))
    }

    pub fn depth(&self, i: EntryId) -> u32 {
        if !self.resolved {
            return 0;
        }
        let mut d = 0;
        let mut p = self.parent[i as usize];
        while p != NONE {
            d += 1;
            p = self.parent[p as usize];
        }
        d
    }

    /// 名前の拡張子（小文字、ドットなし）。ディレクトリや拡張子なしは空。
    pub fn ext(&self, i: EntryId) -> &[u8] {
        if self.is_dir(i) {
            return &[];
        }
        let n = self.lower_name(i);
        match n.iter().rposition(|&c| c == b'.') {
            Some(p) if p > 0 => &n[p + 1..],
            _ => &[],
        }
    }

    /// 主アリーナ末尾へエントリを追記する。戻り値は EntryId。
    #[allow(clippy::too_many_arguments)]
    pub fn push_raw(&mut self, name: &str, parent: EntryId, attrs: u32, size: u64, mtime: i64, ctime: i64, frn: u64) -> EntryId {
        let id = self.parent.len() as EntryId;
        if attrs & ATTR_DIRECTORY != 0 {
            self.dir_epoch += 1;
        }
        let lower = lowercase(name);
        let off = self.arena.len() as u32;
        self.arena.extend_from_slice(lower.as_bytes());
        self.name_off.push(off);
        self.name_len.push(lower.len().min(u16::MAX as usize) as u16);
        if lower == name {
            self.orig_off.push(NONE);
            self.orig_len.push(0);
        } else {
            self.orig_off.push(self.orig_arena.len() as u32);
            self.orig_len.push(name.len().min(u16::MAX as usize) as u16);
            self.orig_arena.extend_from_slice(name.as_bytes());
        }
        self.parent.push(parent);
        self.flags.push(0);
        self.attrs.push(attrs);
        self.size.push(size);
        self.mtime_s.push(filetime_to_secs(mtime));
        self.ctime_s.push(filetime_to_secs(ctime));
        self.atime_s.push(0);
        self.recno.push((frn & 0xFFFF_FFFF) as u32);
        self.seq.push((frn >> 48) as u16);
        self.live += 1;
        let recno = (frn & 0x0000_FFFF_FFFF_FFFF) as usize;
        // MFT が成長して by_recno の範囲を超えたら伸ばす（USN 追従で新しいレコード番号が来る）
        if recno >= self.by_recno.len() && !self.by_recno.is_empty() && recno < self.by_recno.len() + (1 << 24) {
            self.by_recno.resize(recno + 1024, NONE);
        }
        if recno < self.by_recno.len() {
            if self.by_recno[recno] == NONE {
                self.by_recno[recno] = id;
            } else {
                self.multi.entry(recno as u32).or_default().push(id);
            }
        }
        id
    }

    /// 名前を変更する。収まれば主アリーナ内で上書き、収まらなければ overflow へ。
    pub fn rename(&mut self, i: EntryId, new_name: &str) {
        let iu = i as usize;
        if self.attrs[iu] & ATTR_DIRECTORY != 0 {
            self.dir_epoch += 1;
        }
        let lower = lowercase(new_name);
        let same = lower == new_name;
        if self.flags[iu] & FLAG_OVERFLOW != 0 {
            self.overflow[self.orig_off[iu] as usize] = (lower, new_name.to_string());
            return;
        }
        let cap = self.name_len[iu] as usize;
        if lower.len() <= cap {
            let o = self.name_off[iu] as usize;
            self.arena[o..o + lower.len()].copy_from_slice(lower.as_bytes());
            for b in &mut self.arena[o + lower.len()..o + cap] {
                *b = 0;
            }
            self.name_len[iu] = lower.len() as u16;
            if same {
                self.orig_off[iu] = NONE;
                self.orig_len[iu] = 0;
            } else {
                self.orig_off[iu] = self.orig_arena.len() as u32;
                self.orig_len[iu] = new_name.len() as u16;
                self.orig_arena.extend_from_slice(new_name.as_bytes());
            }
        } else {
            // 旧スロットは 0 埋めして一致しないようにする
            let o = self.name_off[iu] as usize;
            for b in &mut self.arena[o..o + cap] {
                *b = 0;
            }
            self.name_len[iu] = 0;
            self.orig_off[iu] = self.overflow.len() as u32;
            self.orig_len[iu] = 0;
            self.flags[iu] |= FLAG_OVERFLOW;
            self.overflow.push((lower, new_name.to_string()));
        }
    }

    pub fn mark_deleted(&mut self, i: EntryId) {
        let iu = i as usize;
        if self.flags[iu] & FLAG_DELETED != 0 {
            return;
        }
        self.flags[iu] |= FLAG_DELETED;
        if self.attrs[iu] & ATTR_DIRECTORY != 0 {
            self.dir_epoch += 1;
        }
        self.live -= 1;
        if self.flags[iu] & FLAG_OVERFLOW == 0 {
            let o = self.name_off[iu] as usize;
            let l = self.name_len[iu] as usize;
            for b in &mut self.arena[o..o + l] {
                *b = 0;
            }
        }
        let recno = self.recno[iu] as usize;
        if recno < self.by_recno.len() && self.by_recno[recno] == i {
            // 先頭を消したら別名（ハードリンク）を昇格させる
            let next = self.multi.get_mut(&(recno as u32)).and_then(|v| if v.is_empty() { None } else { Some(v.remove(0)) });
            self.by_recno[recno] = next.unwrap_or(NONE);
        } else if let Some(v) = self.multi.get_mut(&(recno as u32)) {
            v.retain(|&e| e != i);
        }
    }

    /// 同じレコード番号を持つ全エントリ（先頭を含む）。
    pub fn entries_by_recno(&self, recno: u64) -> Vec<EntryId> {
        let mut out = Vec::new();
        if let Some(first) = self.entry_by_recno(recno) {
            out.push(first);
            if let Some(rest) = self.multi.get(&(recno as u32)) {
                out.extend(rest.iter().copied());
            }
        }
        out
    }

    /// by_recno から multi を作り直す（読み込み直後に呼ぶ）。
    pub fn rebuild_multi(&mut self) {
        self.multi.clear();
        if self.by_recno.is_empty() {
            return;
        }
        for i in 0..self.len() {
            if self.flags[i] & FLAG_DELETED != 0 {
                continue;
            }
            let r = self.recno[i] as usize;
            if r < self.by_recno.len() && self.by_recno[r] != i as EntryId && self.by_recno[r] != NONE {
                self.multi.entry(r as u32).or_default().push(i as EntryId);
            }
        }
    }

    /// レコード番号からエントリを引く（先頭のみ）。
    pub fn entry_by_recno(&self, recno: u64) -> Option<EntryId> {
        let r = recno as usize;
        if r < self.by_recno.len() && self.by_recno[r] != NONE {
            Some(self.by_recno[r])
        } else {
            None
        }
    }

    /// 主アリーナ上の位置 `pos` から `hit_len` バイトが 1 つの生きた名前に収まるならそのエントリ。
    /// `name_off` は主アリーナについて単調増加（追記のみ）なので二分探索できる。
    #[inline]
    pub fn entry_at_arena_pos(&self, pos: u32, hit_len: u32) -> Option<EntryId> {
        let i = self.name_off.partition_point(|&o| o <= pos);
        if i == 0 {
            return None;
        }
        let k = i - 1;
        if self.flags[k] & FLAG_DELETED != 0 {
            return None;
        }
        let end = self.name_off[k] + self.name_len[k] as u32;
        if pos + hit_len <= end {
            Some(k as EntryId)
        } else {
            None
        }
    }

    /// 配下サイズの再集計（ディレクトリの size を配下ファイル合計に置き換える）。
    pub fn recompute_dir_sizes(&mut self) {
        for i in 0..self.len() {
            if self.attrs[i] & ATTR_DIRECTORY != 0 {
                self.size[i] = 0;
            }
        }
        for i in 0..self.len() {
            if self.flags[i] & FLAG_DELETED != 0 || self.attrs[i] & ATTR_DIRECTORY != 0 {
                continue;
            }
            let s = self.size[i];
            if s == 0 {
                continue;
            }
            let mut p = self.parent[i];
            while p != NONE {
                self.size[p as usize] += s;
                p = self.parent[p as usize];
            }
        }
    }

    /// `i` の先祖に `delta` を加減する（USN 差分適用用）。
    pub fn adjust_ancestors(&mut self, i: EntryId, delta: i64) {
        if delta == 0 {
            return;
        }
        let mut p = self.parent[i as usize];
        while p != NONE {
            let s = self.size[p as usize] as i64 + delta;
            self.size[p as usize] = s.max(0) as u64;
            p = self.parent[p as usize];
        }
    }

    /// 主アリーナを詰め直し、overflow を解消する（保存前に呼ぶ）。
    pub fn compact(&mut self) {
        let n = self.len();
        let mut arena = Vec::with_capacity(self.arena.len());
        let mut orig_arena = Vec::with_capacity(self.orig_arena.len());
        let mut name_off = Vec::with_capacity(n);
        let mut name_len = Vec::with_capacity(n);
        let mut orig_off = Vec::with_capacity(n);
        let mut orig_len = Vec::with_capacity(n);
        for i in 0..n {
            let lower = self.lower_name(i as u32).to_vec();
            let orig = self.name(i as u32).as_bytes().to_vec();
            name_off.push(arena.len() as u32);
            name_len.push(lower.len() as u16);
            arena.extend_from_slice(&lower);
            if orig == lower {
                orig_off.push(NONE);
                orig_len.push(0);
            } else {
                orig_off.push(orig_arena.len() as u32);
                orig_len.push(orig.len() as u16);
                orig_arena.extend_from_slice(&orig);
            }
            self.flags[i] &= !FLAG_OVERFLOW;
        }
        self.arena = arena;
        self.orig_arena = orig_arena;
        self.name_off = name_off;
        self.name_len = name_len;
        self.orig_off = orig_off;
        self.orig_len = orig_len;
        self.overflow.clear();
    }

    /// 名前順の順列を計算する（並列ソート）。読み取りロック下で呼べるよう &self で計算して返す。
    pub fn compute_name_order(&self) -> Vec<EntryId> {
        use rayon::prelude::*;
        let mut v: Vec<EntryId> = (0..self.len() as EntryId).collect();
        v.par_sort_unstable_by(|&a, &b| self.lower_name(a).cmp(self.lower_name(b)).then(a.cmp(&b)));
        v
    }

    pub fn heap_bytes(&self) -> usize {
        self.arena.len()
            + self.orig_arena.len()
            + self.name_off.len() * 4
            + self.name_len.len() * 2
            + self.parent.len() * 4
            + self.flags.len() * 2
            + self.attrs.len() * 4
            + self.orig_off.len() * 4
            + self.orig_len.len() * 2
            + self.size.len() * 8
            + self.mtime_s.len() * 4
            + self.ctime_s.len() * 4
            + self.atime_s.len() * 4
            + self.recno.len() * 4
            + self.seq.len() * 2
            + self.by_recno.len() * 4
    }
}

/// Unicode 小文字化。ASCII 高速経路あり。
/// 照合用の正規化: 小文字化し、ラテン文字のダイアクリティカルマーク（é→e, ß→ss, œ→oe …）と
/// 結合文字（U+0300–U+036F）を除く。Everything の「Match Diacritics」オフと同じ扱い。
/// かな（濁点など）には触れない。
pub fn lowercase(s: &str) -> String {
    if s.bytes().all(|b| !b.is_ascii_uppercase() && b < 0x80) {
        return s.to_string();
    }
    if s.is_ascii() {
        return s.to_ascii_lowercase();
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        for l in c.to_lowercase() {
            fold_into(l, &mut out);
        }
    }
    out
}

/// 小文字化のみ（ダイアクリティカルマークを区別する照合用）。
pub fn lowercase_nofold(s: &str) -> String {
    if s.is_ascii() {
        return s.to_ascii_lowercase();
    }
    s.to_lowercase()
}

/// 小文字 1 文字を折り畳んで `out` に追加する。
pub fn fold_into(c: char, out: &mut String) {
    match fold_char(c) {
        Some(r) => out.push_str(r),
        None => out.push(c),
    }
}

/// ラテン文字の折り畳み表（小文字のみ）。`None` はそのまま、`Some("")` は除去。
pub fn fold_char(c: char) -> Option<&'static str> {
    let u = c as u32;
    if u < 0xC0 {
        return None;
    }
    if (0x300..=0x36F).contains(&u) {
        return Some("");
    }
    Some(match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' | 'ǎ' | 'ǟ' | 'ǡ' | 'ǻ' | 'ȁ' | 'ȃ' | 'ȧ' => "a",
        'æ' | 'ǣ' | 'ǽ' => "ae",
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => "c",
        'ď' | 'đ' | 'ð' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' | 'ȅ' | 'ȇ' | 'ȩ' => "e",
        'ĝ' | 'ğ' | 'ġ' | 'ģ' | 'ǧ' | 'ǵ' => "g",
        'ĥ' | 'ħ' | 'ȟ' => "h",
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' | 'ǐ' | 'ȉ' | 'ȋ' => "i",
        'ĳ' => "ij",
        'ĵ' | 'ǰ' => "j",
        'ķ' | 'ĸ' | 'ǩ' => "k",
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => "l",
        'ñ' | 'ń' | 'ņ' | 'ň' | 'ŉ' | 'ŋ' | 'ǹ' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' | 'ǒ' | 'ǫ' | 'ǭ' | 'ǿ' | 'ȍ' | 'ȏ' | 'ȫ' | 'ȭ' | 'ȯ' | 'ȱ' => "o",
        'œ' => "oe",
        'ŕ' | 'ŗ' | 'ř' | 'ȑ' | 'ȓ' => "r",
        'ś' | 'ŝ' | 'ş' | 'š' | 'ș' | 'ſ' => "s",
        'ß' => "ss",
        'ţ' | 'ť' | 'ŧ' | 'ț' => "t",
        'þ' => "th",
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' | 'ǔ' | 'ǖ' | 'ǘ' | 'ǚ' | 'ǜ' | 'ȕ' | 'ȗ' => "u",
        'ŵ' => "w",
        'ý' | 'ÿ' | 'ŷ' => "y",
        'ź' | 'ż' | 'ž' | 'ƶ' => "z",
        _ => return None,
    })
}

#[cfg(test)]
mod fold_tests {
    use super::*;
    #[test]
    fn folds_latin_and_combining() {
        assert_eq!(lowercase("Café"), "cafe");
        assert_eq!(lowercase("STRASSE Straße"), "strasse strasse");
        assert_eq!(lowercase("Œuvre"), "oeuvre");
        // NFD: e + U+0301
        assert_eq!(lowercase("cafe\u{301}"), "cafe");
        // かなの濁点はそのまま
        assert_eq!(lowercase("ガギグ"), "ガギグ");
        assert_eq!(lowercase_nofold("Café"), "café");
    }
}
