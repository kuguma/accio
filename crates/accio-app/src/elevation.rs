//! 管理者昇格の判定と自己再起動。

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_sys::Win32::UI::Shell::ShellExecuteW;

pub fn is_elevated() -> bool {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: 自プロセスのトークンを照会するだけ。
    unsafe {
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut e = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut e as *mut _ as *mut _,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        CloseHandle(token);
        ok != 0 && e.TokenIsElevated != 0
    }
}

/// `runas` で自分自身を再起動する。UAC が拒否されたら false。
pub fn relaunch_elevated() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let exe: Vec<u16> = exe.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let verb: Vec<u16> = "runas".encode_utf16().chain(std::iter::once(0)).collect();
    let args: Vec<u16> = std::env::args().skip(1).collect::<Vec<_>>().join(" ").encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: NUL 終端文字列。戻り値 > 32 で成功。
    let r = unsafe { ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), exe.as_ptr(), args.as_ptr(), std::ptr::null(), 1) };
    r as usize > 32
}

use std::os::windows::ffi::OsStrExt;
