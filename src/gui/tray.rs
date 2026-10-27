//! Notification-area (system tray) icon: the window can close to
//! the tray while downloads continue.
//!
//! The right-click menu is a plain Win32 popup shown with `TPM_RETURNCMD`, so
//! the choice is known at once. (A menu attached to the icon reported its
//! WM_COMMAND only after the main window was activated again.)

use std::sync::mpsc::{channel, Receiver};

use eframe::egui;
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    Show,
    AddUrl,
    StartQueue,
    StopQueue,
    ToggleLimiter,
    Exit,
}

pub struct Tray {
    icon: TrayIcon,
    rx: Receiver<TrayIconEvent>,
    last_tooltip: String,
}

impl Tray {
    /// Must run on the thread that owns the window's event loop.
    pub fn new(ctx: &egui::Context, rgba: Vec<u8>, size: u32) -> Option<Self> {
        // A handler instead of polling: it also wakes the GUI.
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |event| {
            let _ = tx.send(event);
            ctx.request_repaint();
        }));

        let icon = TrayIconBuilder::new()
            .with_tooltip("Klippo")
            .with_icon(Icon::from_rgba(rgba, size, size).ok()?)
            .build()
            .ok()?;

        Some(Self { icon, rx, last_tooltip: String::new() })
    }

    /// Clicks since the last frame. A right click opens the menu (blocking until
    /// the user picks an item or dismisses it).
    pub fn poll(&self, owner_hwnd: Option<isize>, queue_running: bool, limiter_on: bool) -> Vec<TrayAction> {
        let mut actions = Vec::new();
        for event in self.rx.try_iter() {
            match event {
                TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. }
                | TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } => actions.push(TrayAction::Show),
                TrayIconEvent::Click { button: MouseButton::Right, button_state: MouseButtonState::Up, .. } => {
                    if let Some(action) = show_menu(owner_hwnd, queue_running, limiter_on) {
                        actions.push(action);
                    }
                }
                _ => {}
            }
        }
        actions
    }

    /// Updates the hover text when it changes.
    pub fn set_tooltip(&mut self, tooltip: &str) {
        if self.last_tooltip != tooltip {
            let _ = self.icon.set_tooltip(Some(tooltip));
            self.last_tooltip = tooltip.to_string();
        }
    }
}

#[cfg(windows)]
fn show_menu(owner_hwnd: Option<isize>, queue_running: bool, limiter_on: bool) -> Option<TrayAction> {
    use windows_sys::Win32::Foundation::{HWND, POINT};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, PostMessageW, SetForegroundWindow, TrackPopupMenu,
        MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RETURNCMD,
        TPM_RIGHTBUTTON, WM_NULL,
    };

    let owner = owner_hwnd? as HWND;
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    let items: [(usize, &str, u32); 8] = [
        (1, "Show Klippo", MF_STRING),
        (2, "Add new download...", MF_STRING),
        (0, "", MF_SEPARATOR),
        (3, "Start queue", if queue_running { MF_STRING | MF_GRAYED } else { MF_STRING }),
        (4, "Stop queue", if queue_running { MF_STRING } else { MF_STRING | MF_GRAYED }),
        (5, "Speed Limiter", if limiter_on { MF_STRING | MF_CHECKED } else { MF_STRING }),
        (0, "", MF_SEPARATOR),
        (6, "Exit", MF_STRING),
    ];

    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return None;
        }
        let labels: Vec<Vec<u16>> = items.iter().map(|(_, text, _)| wide(text)).collect();
        for ((id, _, flags), label) in items.iter().zip(&labels) {
            let text = if *flags & MF_SEPARATOR != 0 { std::ptr::null() } else { label.as_ptr() };
            AppendMenuW(menu, *flags, *id, text);
        }
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        // The owner must be in front, or the menu does not close on an outside click.
        SetForegroundWindow(owner);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_LEFTALIGN,
            pt.x,
            pt.y,
            0,
            owner,
            std::ptr::null(),
        );
        PostMessageW(owner, WM_NULL, 0, 0);
        DestroyMenu(menu);

        match cmd {
            1 => Some(TrayAction::Show),
            2 => Some(TrayAction::AddUrl),
            3 => Some(TrayAction::StartQueue),
            4 => Some(TrayAction::StopQueue),
            5 => Some(TrayAction::ToggleLimiter),
            6 => Some(TrayAction::Exit),
            _ => None,
        }
    }
}

#[cfg(not(windows))]
fn show_menu(_owner_hwnd: Option<isize>, _queue_running: bool, _limiter_on: bool) -> Option<TrayAction> {
    None
}
