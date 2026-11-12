//! メインウィンドウの表示/非表示を UI スレッド外からも直接行う。
//!
//! ウィンドウが隠れている間は egui のフレームが回らないので、トレイ/ホットキー/IPC の
//! コールバック側で Win32 の ShowWindow を直接呼び、そのあと再描画を要求する。

use std::sync::atomic::{AtomicIsize, Ordering};
use windows_sys::Win32::UI::WindowsAndMessaging::{IsIconic, IsWindowVisible, SetForegroundWindow, ShowWindow, SW_HIDE, SW_RESTORE, SW_SHOW};

static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);

pub fn set_main_hwnd(h: isize) {
    MAIN_HWND.store(h, Ordering::Relaxed);
}

pub fn hwnd() -> isize {
    MAIN_HWND.load(Ordering::Relaxed)
}

pub fn is_visible() -> bool {
    let h = hwnd();
    // SAFETY: 自プロセスのウィンドウハンドル（0 なら偽）。
    h != 0 && unsafe { IsWindowVisible(h as _) != 0 }
}

pub fn show() {
    let h = hwnd();
    if h == 0 {
        return;
    }
    // SAFETY: 自プロセスのウィンドウハンドル。
    unsafe {
        ShowWindow(h as _, SW_SHOW);
        if IsIconic(h as _) != 0 {
            ShowWindow(h as _, SW_RESTORE);
        }
        SetForegroundWindow(h as _);
    }
}

pub fn hide() {
    let h = hwnd();
    if h == 0 {
        return;
    }
    // SAFETY: 自プロセスのウィンドウハンドル。
    unsafe {
        ShowWindow(h as _, SW_HIDE);
    }
}

pub fn toggle() {
    if is_visible() {
        hide();
    } else {
        show();
    }
}
