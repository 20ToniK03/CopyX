//! CopyX — Lightweight ShareX alternative with Liquid Glass UI.

#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod capture;
mod clipboard;
#[allow(dead_code)]
mod glass;
mod hotkey;
mod overlay;
mod render;
mod save;
mod snapshot;
mod toast;
mod tray;
mod ui;

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;

unsafe extern "system" fn tray_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_HOTKEY => {
            let id = wparam.0 as i32;
            let instance = app::with_state(|s| s.instance);
            match id {
                hotkey::HOTKEY_REGION | hotkey::HOTKEY_PRTSC => {
                    overlay::show(instance);
                }
                hotkey::HOTKEY_WINDOW => {
                    capture_foreground_window();
                }
                hotkey::HOTKEY_FULLSCREEN => {
                    capture_fullscreen();
                }
                _ => {}
            }
            LRESULT(0)
        }

        msg if msg == tray::TRAY_MESSAGE => {
            let event = lparam.0 as u32;
            if event == WM_RBUTTONUP || event == WM_LBUTTONUP {
                tray::show_menu(hwnd);
            }
            LRESULT(0)
        }

        WM_COMMAND => {
            let cmd = wparam.0 as usize;
            match cmd {
                tray::MENU_CAPTURE_REGION => {
                    let instance = app::with_state(|s| s.instance);
                    overlay::show(instance);
                }
                tray::MENU_CAPTURE_WINDOW => {
                    capture_foreground_window();
                }
                tray::MENU_CAPTURE_FULL => {
                    capture_fullscreen();
                }
                tray::MENU_OPEN_FOLDER => {
                    let dir = save::screenshots_dir();
                    let _ = std::fs::create_dir_all(&dir);
                    let dir_str: Vec<u16> = dir
                        .to_string_lossy()
                        .encode_utf16()
                        .chain(Some(0))
                        .collect();
                    unsafe {
                        ShellExecuteW(
                            None,
                            w!("open"),
                            windows::core::PCWSTR(dir_str.as_ptr()),
                            None,
                            None,
                            SW_SHOWNORMAL,
                        );
                    }
                }
                tray::MENU_EXIT => {
                    PostQuitMessage(0);
                }
                _ => {}
            }
            LRESULT(0)
        }

        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }

        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn capture_foreground_window() {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0 == 0 {
            return;
        }

        let mut rect = RECT::default();
        let dwm_ok = DwmGetWindowAttribute(
            fg,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut rect as *mut _ as *mut _,
            std::mem::size_of::<RECT>() as u32,
        );
        if dwm_ok.is_err() {
            let _ = GetWindowRect(fg, &mut rect);
        }

        if rect.right - rect.left < 1 || rect.bottom - rect.top < 1 {
            return;
        }

        std::thread::sleep(std::time::Duration::from_millis(50));

        if let Ok(img) = capture::capture_rect(&rect) {
            deliver(img);
        }
    }
}

fn capture_fullscreen() {
    std::thread::sleep(std::time::Duration::from_millis(50));
    match capture::capture_full_screen() {
        Ok(img) => deliver(img),
        Err(_) => {}
    }
}

/// Скопировать снимок в буфер обмена и показать стеклянный тост.
fn deliver(img: capture::CapturedImage) {
    let copied = clipboard::copy_to_clipboard(&img).is_ok();
    toast::show(img, copied);
}

fn main() {
    unsafe {
        // Per-monitor DPI awareness: координаты оверлея, курсора и снимка
        // совпадают на мониторах с масштабированием.
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );

        let instance = GetModuleHandleW(None).unwrap().into();

        app::init(instance);

        let class_name = w!("CopyXTray");
        let class = WNDCLASSW {
            hInstance: instance,
            lpfnWndProc: Some(tray_wndproc),
            lpszClassName: class_name,
            hIcon: tray::app_icon(),
            ..Default::default()
        };
        let _ = RegisterClassW(&class);

        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class_name,
            w!("CopyX"),
            WS_OVERLAPPED,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            0,
            0,
            None,
            HMENU::default(),
            instance,
            None,
        );

        app::with_state(|s| s.tray_hwnd = hwnd);

        let hotkeys = hotkey::register_all(hwnd);
        let registered: Vec<&str> = hotkeys
            .iter()
            .filter(|h| h.registered)
            .map(|h| h.shortcut)
            .collect();
        let tooltip = format!("CopyX ({})", registered.join(", "));

        tray::add_icon(hwnd, &tooltip);
        toast::show_started();

        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }

        hotkey::unregister_all(hwnd);
        tray::remove_icon(hwnd);
    }
}