//! Explorer と同じシェルコンテキストメニュー（IContextMenu / IContextMenu2 / IContextMenu3）。
//!
//! UI スレッドで呼ぶ。`TrackPopupMenuEx` はモーダルなメッセージループを回すので、メニューが
//! 開いている間 egui の描画は止まる（Explorer と同じ挙動）。
//! 「送る」「プログラムから開く」などの動的サブメニューは、メニューのオーナーとして作った
//! 専用ウィンドウが WM_INITMENUPOPUP / WM_DRAWITEM / WM_MEASUREITEM / WM_MENUCHAR を
//! IContextMenu3::HandleMenuMsg2 へ転送することで動く。

use std::cell::RefCell;
use windows::core::{Interface, PCSTR, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    IContextMenu, IContextMenu2, IContextMenu3, IShellFolder, SHBindToParent, SHParseDisplayName, CMF_EXPLORE, CMINVOKECOMMANDINFO,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow, GetCursorPos, GetMenuItemCount, RegisterClassW, TrackPopupMenuEx,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_DRAWITEM, WM_INITMENUPOPUP, WM_MEASUREITEM, WM_MENUCHAR, WM_MENUSELECT, WNDCLASSW, WINDOW_EX_STYLE,
    WINDOW_STYLE,
};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

thread_local! {
    /// メニュー表示中だけ有効。オーナーウィンドウの wndproc から参照する。
    static ACTIVE: RefCell<Option<(Option<IContextMenu2>, Option<IContextMenu3>)>> = const { RefCell::new(None) };
}

unsafe extern "system" fn owner_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if matches!(msg, WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM | WM_MENUCHAR | WM_MENUSELECT) {
        let handled = ACTIVE.with(|a| {
            let a = a.borrow();
            let (m2, m3) = a.as_ref()?;
            if let Some(m3) = m3 {
                let mut result = LRESULT(0);
                if m3.HandleMenuMsg2(msg, wparam, lparam, Some(&mut result)).is_ok() {
                    return Some(result);
                }
            }
            if let Some(m2) = m2 {
                if m2.HandleMenuMsg(msg, wparam, lparam).is_ok() {
                    return Some(LRESULT(0));
                }
            }
            None
        });
        if let Some(r) = handled {
            return r;
        }
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// メニューのオーナーにする不可視ウィンドウを作る（UI スレッド上）。
unsafe fn create_owner(parent: HWND) -> Option<HWND> {
    static CLASS: std::sync::OnceLock<Vec<u16>> = std::sync::OnceLock::new();
    let class = CLASS.get_or_init(|| {
        let name = wide("accio_menu_owner");
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(owner_wndproc);
        wc.lpszClassName = PCWSTR(name.as_ptr());
        RegisterClassW(&wc);
        name
    });
    CreateWindowExW(
        WINDOW_EX_STYLE(0),
        PCWSTR(class.as_ptr()),
        PCWSTR::null(),
        WINDOW_STYLE(0),
        0,
        0,
        0,
        0,
        Some(parent),
        None,
        None,
        None,
    )
    .ok()
}

/// `paths`（同じ親フォルダ内）のシェルメニューをカーソル位置に出し、選ばれたコマンドを実行する。
pub fn show(hwnd: isize, paths: &[String]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    // SAFETY: COM 呼び出し。各ポインタはこの関数内で生成・解放する。
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let parent = HWND(hwnd as *mut _);
        let mut pidls: Vec<*mut ITEMIDLIST> = Vec::new();
        for p in paths {
            let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
            let w = wide(p);
            SHParseDisplayName(PCWSTR(w.as_ptr()), None, &mut pidl, 0, None).map_err(|e| e.to_string())?;
            pidls.push(pidl);
        }
        let mut children: Vec<*const ITEMIDLIST> = Vec::new();
        let mut folder: Option<IShellFolder> = None;
        for pidl in &pidls {
            let mut child: *mut ITEMIDLIST = std::ptr::null_mut();
            let f: IShellFolder = SHBindToParent(*pidl, Some(&mut child)).map_err(|e| e.to_string())?;
            if folder.is_none() {
                folder = Some(f);
            }
            children.push(child);
        }
        let folder = folder.ok_or(crate::i18n::tr("フォルダなし"))?;
        let owner = create_owner(parent).ok_or(crate::i18n::tr("メニュー用ウィンドウを作れない"))?;
        let menu: IContextMenu = folder.GetUIObjectOf(owner, &children, None).map_err(|e| e.to_string())?;
        let m2: Option<IContextMenu2> = menu.cast().ok();
        let m3: Option<IContextMenu3> = menu.cast().ok();
        let hmenu = CreatePopupMenu().map_err(|e| e.to_string())?;
        let first = 1u32;
        let last = 0x7fffu32;
        let hr = menu.QueryContextMenu(hmenu, 0, first, last, CMF_EXPLORE);
        if hr.is_err() {
            DestroyMenu(hmenu).ok();
            DestroyWindow(owner).ok();
            return Err(format!("QueryContextMenu: {hr:?}"));
        }
        let mut pt = POINT::default();
        GetCursorPos(&mut pt).ok();
        ACTIVE.with(|a| *a.borrow_mut() = Some((m2, m3)));
        let cmd = TrackPopupMenuEx(hmenu, (TPM_RETURNCMD | TPM_RIGHTBUTTON).0, pt.x, pt.y, owner, None);
        ACTIVE.with(|a| *a.borrow_mut() = None);
        let id = cmd.0 as u32;
        if id >= first && id <= last {
            let verb = (id - first) as usize;
            let info = CMINVOKECOMMANDINFO {
                cbSize: std::mem::size_of::<CMINVOKECOMMANDINFO>() as u32,
                fMask: 0,
                hwnd: parent,
                lpVerb: PCSTR(verb as *const u8),
                lpParameters: PCSTR::null(),
                lpDirectory: PCSTR::null(),
                nShow: 1,
                dwHotKey: 0,
                hIcon: Default::default(),
            };
            if let Err(e) = menu.InvokeCommand(&info) {
                log::warn!("InvokeCommand: {e}");
            }
        }
        DestroyMenu(hmenu).ok();
        DestroyWindow(owner).ok();
        for pidl in pidls {
            windows::Win32::System::Com::CoTaskMemFree(Some(pidl as *const _));
        }
        let _ = menu.as_raw();
    }
    Ok(())
}

/// 検証用: メニューを構築して項目数を返す（表示はしない）。
pub fn probe(path: &str) -> Result<i32, String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        let w = wide(path);
        SHParseDisplayName(PCWSTR(w.as_ptr()), None, &mut pidl, 0, None).map_err(|e| e.to_string())?;
        let mut child: *mut ITEMIDLIST = std::ptr::null_mut();
        let folder: IShellFolder = SHBindToParent(pidl, Some(&mut child)).map_err(|e| e.to_string())?;
        let owner = create_owner(HWND::default()).ok_or(crate::i18n::tr("メニュー用ウィンドウを作れない"))?;
        let menu: IContextMenu = folder.GetUIObjectOf(owner, &[child as *const _], None).map_err(|e| e.to_string())?;
        let m2: Option<IContextMenu2> = menu.cast().ok();
        let m3: Option<IContextMenu3> = menu.cast().ok();
        let hmenu = CreatePopupMenu().map_err(|e| e.to_string())?;
        let hr = menu.QueryContextMenu(hmenu, 0, 1, 0x7fff, CMF_EXPLORE);
        if hr.is_err() {
            return Err(format!("QueryContextMenu: {hr:?}"));
        }
        let n = GetMenuItemCount(Some(hmenu));
        println!("IContextMenu2={} IContextMenu3={}", m2.is_some(), m3.is_some());
        DestroyMenu(hmenu).ok();
        DestroyWindow(owner).ok();
        windows::Win32::System::Com::CoTaskMemFree(Some(pidl as *const _));
        Ok(n)
    }
}
