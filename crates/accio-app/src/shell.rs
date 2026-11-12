//! シェル連携（開く、フォルダを開く、クリップボード）。すべて非同期スレッドで実行し UI を止めない。

use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
    KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows_sys::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows_sys::Win32::UI::Shell::{
    SHFileOperationW, SHGetFileInfoW, ShellExecuteW, DROPFILES, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_SILENT, FO_DELETE, SHFILEINFOW,
    SHFILEOPSTRUCTW, SHGFI_TYPENAME, SHGFI_USEFILEATTRIBUTES,
};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn shell_execute(verb: &str, file: &str, params: Option<&str>) {
    let (verb, file, params) = (verb.to_string(), file.to_string(), params.map(|s| s.to_string()));
    std::thread::spawn(move || {
        let v = wide(&verb);
        let f = wide(&file);
        let p = params.as_deref().map(wide);
        // SAFETY: すべて NUL 終端文字列。SW_SHOWNORMAL = 1。
        unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                v.as_ptr(),
                f.as_ptr(),
                p.as_ref().map(|p| p.as_ptr()).unwrap_or(std::ptr::null()),
                std::ptr::null(),
                1,
            );
        }
    });
}

pub fn open(path: &str) {
    shell_execute("open", path, None);
}

/// エクスプローラーでそのファイルを選択状態で開く。
pub fn open_path(path: &str) {
    shell_execute("open", "explorer.exe", Some(&format!("/select,\"{path}\"")));
}

pub fn properties(path: &str) {
    shell_execute("properties", path, None);
}

/// 管理者として実行（UAC）。
pub fn open_as_admin(path: &str) {
    shell_execute("runas", path, None);
}

/// 引数付きで管理者として実行（UAC）。
pub fn run_as_admin(file: &str, params: &str) {
    shell_execute("runas", file, Some(params));
}

/// 起動中のインデクサ（-instance accio-indexer）の IPC ウィンドウへ WM_USER コマンドを送る。
pub fn send_to_indexer(cmd: usize) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, SendMessageW, WM_USER};
    let class = wide("EVERYTHING_TASKBAR_NOTIFICATION_(accio-indexer)");
    let title = wide("accio");
    // SAFETY: NUL 終端文字列。
    unsafe {
        let h = FindWindowW(class.as_ptr(), title.as_ptr());
        if h.is_null() {
            return false;
        }
        SendMessageW(h, WM_USER, cmd, 0);
        true
    }
}

/// 複数パスをごみ箱へ移動する（同期。呼び出し側はワーカースレッドで呼ぶ）。
pub fn recycle(paths: &[String]) -> Result<(), i32> {
    // 二重 NUL 終端の連結リスト
    let mut from: Vec<u16> = Vec::new();
    for p in paths {
        from.extend(p.encode_utf16());
        from.push(0);
    }
    from.push(0);
    // SAFETY: 構造体はスタック上、文字列は二重 NUL 終端。
    let mut op = SHFILEOPSTRUCTW {
        hwnd: std::ptr::null_mut(),
        wFunc: FO_DELETE,
        pFrom: from.as_ptr(),
        pTo: std::ptr::null(),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT) as u16,
        fAnyOperationsAborted: 0,
        hNameMappings: std::ptr::null_mut(),
        lpszProgressTitle: std::ptr::null(),
    };
    let r = unsafe { SHFileOperationW(&mut op) };
    if r == 0 {
        Ok(())
    } else {
        Err(r)
    }
}

/// 拡張子ごとのシェル種別名（"テキスト ドキュメント" など）。結果はキャッシュする。
pub fn type_name(ext: &str, is_dir: bool) -> String {
    static CACHE: parking_lot::Mutex<Option<std::collections::HashMap<String, String>>> = parking_lot::Mutex::new(None);
    let key = if is_dir { "\\dir".to_string() } else { ext.to_ascii_lowercase() };
    if let Some(c) = CACHE.lock().as_ref() {
        if let Some(v) = c.get(&key) {
            return v.clone();
        }
    }
    let name = if is_dir || ext.is_empty() { "x".to_string() } else { format!("x.{ext}") };
    let w = wide(&name);
    // SAFETY: 出力構造体はゼロ初期化、USEFILEATTRIBUTES なのでファイルにはアクセスしない。
    let mut info: SHFILEINFOW = unsafe { std::mem::zeroed() };
    let attrs = if is_dir { 0x10 } else { 0x80 };
    let r = unsafe { SHGetFileInfoW(w.as_ptr(), attrs, &mut info, std::mem::size_of::<SHFILEINFOW>() as u32, SHGFI_TYPENAME | SHGFI_USEFILEATTRIBUTES) };
    let s = if r != 0 {
        let end = info.szTypeName.iter().position(|&c| c == 0).unwrap_or(info.szTypeName.len());
        String::from_utf16_lossy(&info.szTypeName[..end])
    } else {
        String::new()
    };
    let mut c = CACHE.lock();
    c.get_or_insert_with(Default::default).insert(key, s.clone());
    s
}

/// ファイルをクリップボードへ（CF_HDROP）。Explorer に貼り付けるとコピーになる。
pub fn copy_files_to_clipboard(paths: &[String]) -> bool {
    const CF_HDROP: u32 = 15;
    let mut list: Vec<u16> = Vec::new();
    for p in paths {
        list.extend(p.encode_utf16());
        list.push(0);
    }
    list.push(0);
    let header = std::mem::size_of::<DROPFILES>();
    let total = header + list.len() * 2;
    // SAFETY: GlobalAlloc したブロックに DROPFILES ヘッダとワイド文字列を書き、所有権をクリップボードへ渡す。
    unsafe {
        let h = GlobalAlloc(GMEM_MOVEABLE, total);
        if h.is_null() {
            return false;
        }
        let p = GlobalLock(h) as *mut u8;
        if p.is_null() {
            return false;
        }
        let df = p as *mut DROPFILES;
        (*df).pFiles = header as u32;
        (*df).pt.x = 0;
        (*df).pt.y = 0;
        (*df).fNC = 0;
        (*df).fWide = 1;
        std::ptr::copy_nonoverlapping(list.as_ptr() as *const u8, p.add(header), list.len() * 2);
        GlobalUnlock(h);
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return false;
        }
        EmptyClipboard();
        let ok = !SetClipboardData(CF_HDROP, h as *mut _).is_null();
        CloseClipboard();
        ok
    }
}

/// 同じフォルダ内でのリネーム。
pub fn rename(path: &str, new_name: &str) -> std::io::Result<()> {
    let p = std::path::Path::new(path);
    let parent = p.parent().ok_or_else(|| std::io::Error::other("親なし"))?;
    std::fs::rename(p, parent.join(new_name))
}

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Explorer のフォルダ右クリック「accio で検索」（HKCU\Software\Classes 配下なので管理者不要）
const CTX_KEYS: [&str; 2] = [r"Software\Classes\Directory\shell\accio", r"Software\Classes\Directory\Background\shell\accio"];

fn set_reg_sz(key: HKEY, name: Option<&str>, value: &str) -> bool {
    let n = name.map(wide);
    let v = wide(value);
    // SAFETY: NUL 終端文字列と正しいバイト長。
    unsafe { RegSetValueExW(key, n.as_ref().map(|w| w.as_ptr()).unwrap_or(std::ptr::null()), 0, REG_SZ, v.as_ptr() as *const u8, (v.len() * 2) as u32) == 0 }
}

pub fn is_folder_context_menu() -> bool {
    let sub = wide(CTX_KEYS[0]);
    let mut h: HKEY = std::ptr::null_mut();
    let r = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, sub.as_ptr(), 0, KEY_READ, &mut h) };
    if r == 0 {
        unsafe { RegCloseKey(h) };
        true
    } else {
        false
    }
}

const URL_KEY: &str = r"Software\Classes\es";

pub fn is_url_protocol() -> bool {
    let sub = wide(URL_KEY);
    let mut h: HKEY = std::ptr::null_mut();
    let r = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, sub.as_ptr(), 0, KEY_READ, &mut h) };
    if r == 0 {
        unsafe { RegCloseKey(h) };
        true
    } else {
        false
    }
}

/// `es:<検索語>` URL プロトコル（Everything 互換）を HKCU に登録/削除する。
pub fn set_url_protocol(on: bool) -> bool {
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
    let sub = wide(URL_KEY);
    if !on {
        let r = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, sub.as_ptr()) };
        return r == 0 || r == 2;
    }
    let mut h: HKEY = std::ptr::null_mut();
    let r = unsafe { RegCreateKeyExW(HKEY_CURRENT_USER, sub.as_ptr(), 0, std::ptr::null(), REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, std::ptr::null(), &mut h, std::ptr::null_mut()) };
    if r != 0 {
        return false;
    }
    let mut ok = set_reg_sz(h, None, "URL:Everything Search (accio)");
    ok &= set_reg_sz(h, Some("URL Protocol"), "");
    let mut hc: HKEY = std::ptr::null_mut();
    let cmd = wide(r"shell\open\command");
    let r = unsafe { RegCreateKeyExW(h, cmd.as_ptr(), 0, std::ptr::null(), REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, std::ptr::null(), &mut hc, std::ptr::null_mut()) };
    if r == 0 {
        ok &= set_reg_sz(hc, None, &format!("\"{exe}\" -url \"%1\""));
        unsafe { RegCloseKey(hc) };
    } else {
        ok = false;
    }
    unsafe { RegCloseKey(h) };
    ok
}

/// フォルダ右クリックメニューに「accio で検索」を追加/削除する。
pub fn set_folder_context_menu(on: bool) -> bool {
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
    let mut ok = true;
    for (i, k) in CTX_KEYS.iter().enumerate() {
        let sub = wide(k);
        if on {
            let mut h: HKEY = std::ptr::null_mut();
            let r = unsafe { RegCreateKeyExW(HKEY_CURRENT_USER, sub.as_ptr(), 0, std::ptr::null(), REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, std::ptr::null(), &mut h, std::ptr::null_mut()) };
            if r != 0 {
                ok = false;
                continue;
            }
            ok &= set_reg_sz(h, None, "accio で検索");
            ok &= set_reg_sz(h, Some("Icon"), &format!("\"{exe}\",0"));
            let mut hc: HKEY = std::ptr::null_mut();
            let cmd = wide("command");
            let r = unsafe { RegCreateKeyExW(h, cmd.as_ptr(), 0, std::ptr::null(), REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, std::ptr::null(), &mut hc, std::ptr::null_mut()) };
            if r == 0 {
                // Background は %V（カレントフォルダ）、Directory は %1
                let arg = if i == 0 { "%1" } else { "%V" };
                ok &= set_reg_sz(hc, None, &format!("\"{exe}\" -s \"parent:\\\"{arg}\\\"\""));
                unsafe { RegCloseKey(hc) };
            } else {
                ok = false;
            }
            unsafe { RegCloseKey(h) };
        } else {
            let r = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, sub.as_ptr()) };
            ok &= r == 0 || r == 2;
        }
    }
    ok
}
const RUN_VALUE: &str = "accio";

fn open_run_key(access: u32) -> Option<HKEY> {
    let mut h: HKEY = std::ptr::null_mut();
    let sub = wide(RUN_KEY);
    // SAFETY: NUL 終端文字列と出力ハンドル。
    let r = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, sub.as_ptr(), 0, access, &mut h) };
    (r == 0).then_some(h)
}

/// ログオン時に `accio -startup` を自動起動する設定（HKCU\Run）。
pub fn is_start_on_login() -> bool {
    let Some(h) = open_run_key(KEY_READ) else { return false };
    let name = wide(RUN_VALUE);
    let mut len = 0u32;
    let r = unsafe { RegQueryValueExW(h, name.as_ptr(), std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut(), &mut len) };
    unsafe { RegCloseKey(h) };
    r == 0
}

pub fn set_start_on_login(on: bool) -> bool {
    let Some(h) = open_run_key(KEY_SET_VALUE) else { return false };
    let name = wide(RUN_VALUE);
    let ok = if on {
        let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
        let val = wide(&format!("\"{exe}\" -startup"));
        let bytes = val.len() * 2;
        unsafe { RegSetValueExW(h, name.as_ptr(), 0, REG_SZ, val.as_ptr() as *const u8, bytes as u32) == 0 }
    } else {
        unsafe { RegDeleteValueW(h, name.as_ptr()) == 0 }
    };
    unsafe { RegCloseKey(h) };
    ok
}
