//! System tray icon and context menu.

use std::mem::size_of;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, GetSystemMetrics, LoadIconW,
    LoadImageW, SetForegroundWindow, TrackPopupMenu, HICON, IDI_APPLICATION, IMAGE_ICON, LR_SHARED,
    MF_SEPARATOR, MF_STRING, SM_CXSMICON, SM_CYSMICON, TPM_BOTTOMALIGN, TPM_LEFTALIGN,
};

use crate::app;

/// Иконка приложения из ресурсов EXE (см. `app.rc`), с запасным вариантом.
pub fn app_icon() -> HICON {
    let size = unsafe {
        GetSystemMetrics(SM_CXSMICON).max(GetSystemMetrics(SM_CYSMICON))
    };
    app_icon_for(size)
}

/// Иконка приложения нужного размера (Windows выберет ближайший кадр из ICO).
pub fn app_icon_for(size: i32) -> HICON {
    unsafe {
        let instance = app::with_state(|s| s.instance);
        // MAKEINTRESOURCE(1) — идентификатор иконки в app.rc.
        let resource = PCWSTR(1 as *const u16);
        let size = size.max(16);

        match LoadImageW(instance, resource, IMAGE_ICON, size, size, LR_SHARED) {
            Ok(handle) if !handle.is_invalid() => HICON(handle.0),
            _ => LoadIconW(None, IDI_APPLICATION).unwrap_or_default(),
        }
    }
}

pub const TRAY_MESSAGE: u32 = 0x0400 + 1;

pub const MENU_CAPTURE_REGION: usize = 2001;
pub const MENU_CAPTURE_WINDOW: usize = 2002;
pub const MENU_CAPTURE_FULL: usize = 2003;
pub const MENU_OPEN_FOLDER: usize = 2004;
pub const MENU_EXIT: usize = 2005;

pub fn add_icon(hwnd: HWND, tooltip: &str) {
    let data = build_nid(hwnd, tooltip);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &data);
    }
}

pub fn remove_icon(hwnd: HWND) {
    let mut data = NOTIFYICONDATAW::default();
    data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = hwnd;
    data.uID = 1;
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
    }
}

pub fn show_menu(hwnd: HWND) {
    unsafe {
        let menu = match CreatePopupMenu() {
            Ok(m) => m,
            Err(_) => return,
        };

        let _ = AppendMenuW(menu, MF_STRING, MENU_CAPTURE_REGION, w!("Снимок области\tPrtSc"));
        let _ = AppendMenuW(menu, MF_STRING, MENU_CAPTURE_WINDOW, w!("Снимок окна\tCtrl+PrtSc"));
        let _ = AppendMenuW(menu, MF_STRING, MENU_CAPTURE_FULL, w!("Снимок экрана\tShift+PrtSc"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, MENU_OPEN_FOLDER, w!("Открыть папку снимков"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, MENU_EXIT, w!("Выход"));

        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(
            menu,
            TPM_LEFTALIGN | TPM_BOTTOMALIGN,
            point.x,
            point.y,
            0,
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);
    }
}

fn build_nid(hwnd: HWND, tooltip: &str) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW::default();
    data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = hwnd;
    data.uID = 1;
    data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    data.uCallbackMessage = TRAY_MESSAGE;
    data.hIcon = app_icon();

    let tip_utf16: Vec<u16> = tooltip.encode_utf16().chain(Some(0)).collect();
    for (slot, &val) in data.szTip.iter_mut().zip(tip_utf16.iter()) {
        *slot = val;
    }

    data
}