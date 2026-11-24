//! メインウィンドウの表示/非表示を UI スレッド外からも直接行う。
//!
//! ウィンドウが隠れている間は egui のフレームが回らないので、トレイ/ホットキー/IPC の
//! コールバック側で Win32 の ShowWindow を直接呼び、そのあと再描画を要求する。

use std::sync::atomic::{AtomicIsize, Ordering};
use windows_sys::Win32::UI::WindowsAndMessaging::{IsIconic, IsWindowVisible, SendMessageTimeoutW, SendNotifyMessageW, SetForegroundWindow, ShowWindow, SMTO_ABORTIFHUNG, SW_HIDE, SW_RESTORE, SW_SHOW, WM_CLOSE, WM_PAINT};

static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);

pub fn set_main_hwnd(h: isize) {
    MAIN_HWND.store(h, Ordering::Relaxed);
}

pub fn hwnd() -> isize {
    MAIN_HWND.load(Ordering::Relaxed)
}

/// Deliver one UI turn even when Windows suppresses redraws for a hidden window.
/// Used for explicit commands only; background index updates must not wake the renderer.
pub fn wake() {
    signal(false);
}

pub fn request_close() {
    signal(true);
}

fn signal(close: bool) {
    let h = hwnd();
    log::debug!("UI command wake hwnd={h}, close={close}");
    if h != 0 {
        // Posted paints are suppressed while hidden. A nonqueued notification
        // drives one UI turn without showing the window. Send from another thread
        // to avoid re-entering App (and dropping the tray) inside its menu callback.
        if let Err(e) = std::thread::Builder::new().name("ui-command".into()).spawn(move || {
            // eframe acknowledges CloseRequested in a subsequent UI turn.
            // Deliver that event before the paint, not a Close viewport command
            // that would itself require another (suppressed) hidden redraw.
            if close && unsafe { SendMessageTimeoutW(h as _, WM_CLOSE, 0, 0, SMTO_ABORTIFHUNG, 5000, std::ptr::null_mut()) } == 0 {
                log::warn!("UI close notification: {}", std::io::Error::last_os_error());
                return;
            }
            if unsafe { SendNotifyMessageW(h as _, WM_PAINT, 0, 0) } == 0 {
                log::warn!("UI command wake: {}", std::io::Error::last_os_error());
            }
        }) {
            log::error!("UI command thread: {e}");
        }
    }
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
