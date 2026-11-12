//! トレイアイコンとグローバルホットキー。
//!
//! イベントはライブラリ側のスレッドで受け取り、egui へは `request_repaint` で起こしたうえで
//! チャネル経由で UI スレッドが読み取る。

use crossbeam_channel::{unbounded, Receiver, Sender};
use global_hotkey::{hotkey::HotKey, GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use std::sync::Arc;
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    ShowWindow,
    ToggleWindow,
    NewSearch,
    Exit,
}

pub struct Tray {
    _icon: TrayIcon,
    _hotkeys: Option<GlobalHotKeyManager>,
    pub rx: Receiver<TrayCommand>,
    pub hotkey_label: String,
}

fn make_icon() -> Icon {
    // タイル付きのマーク 32px（MONOFORM 022 A-PULL）。Windows が DPI に合わせて縮める
    let img = image::load_from_memory(include_bytes!("../assets/icon/tray/accio-tray-tile@32.png")).expect("tray png").to_rgba8();
    let (w, h) = img.dimensions();
    Icon::from_rgba(img.into_raw(), w, h).expect("tray icon")
}

impl Tray {
    /// `hotkey` は "ctrl+alt+a" 形式。空なら登録しない。
    pub fn new(hotkey: &str, repaint: Arc<dyn Fn() + Send + Sync>) -> anyhow::Result<Self> {
        let (tx, rx): (Sender<TrayCommand>, Receiver<TrayCommand>) = unbounded();
        let menu = Menu::new();
        let show = MenuItem::with_id("show", "accio を表示", true, None);
        let new_search = MenuItem::with_id("new", "新しい検索", true, None);
        let exit = MenuItem::with_id("exit", "終了", true, None);
        menu.append_items(&[&show, &new_search, &exit])?;
        // 左クリックは表示/非表示の切り替えだけ。メニューは右クリック（既定では左でもメニューが出て、
        // ウィンドウがフォーカスを取った瞬間に消えるので「一瞬メニューが出る」ように見えていた）
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_tooltip("accio")
            .with_icon(make_icon())
            .build()?;

        let tx1 = tx.clone();
        let rp1 = repaint.clone();
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let cmd = match e.id.0.as_str() {
                "show" => TrayCommand::ShowWindow,
                "new" => TrayCommand::NewSearch,
                "exit" => TrayCommand::Exit,
                _ => return,
            };
            if matches!(cmd, TrayCommand::ShowWindow | TrayCommand::NewSearch) {
                crate::winshow::show();
            }
            let _ = tx1.send(cmd);
            rp1();
        }));
        let tx2 = tx.clone();
        let rp2 = repaint.clone();
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            // ダブルクリックで表示/非表示。シングルクリックは何もしない（誤操作で開かない）
            if let TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } = e {
                crate::winshow::toggle();
                let _ = tx2.send(TrayCommand::ToggleWindow);
                rp2();
            }
        }));

        let mut hotkeys = None;
        let mut hotkey_label = String::new();
        if !hotkey.trim().is_empty() {
            match hotkey.parse::<HotKey>() {
                Ok(hk) => match GlobalHotKeyManager::new() {
                    Ok(m) => match m.register(hk) {
                        Ok(()) => {
                            hotkey_label = hotkey.to_string();
                            let tx3 = tx.clone();
                            let rp3 = repaint.clone();
                            GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
                                if e.state == HotKeyState::Pressed {
                                    crate::winshow::toggle();
                                    let _ = tx3.send(TrayCommand::ToggleWindow);
                                    rp3();
                                }
                            }));
                            hotkeys = Some(m);
                        }
                        Err(e) => log::warn!("ホットキー {hotkey} を登録できない: {e}"),
                    },
                    Err(e) => log::warn!("ホットキー管理を初期化できない: {e}"),
                },
                Err(e) => log::warn!("ホットキー {hotkey} を解釈できない: {e}"),
            }
        }
        Ok(Self { _icon: icon, _hotkeys: hotkeys, rx, hotkey_label })
    }
}
