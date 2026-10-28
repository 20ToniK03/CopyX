//! Hotkey registration and management.

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_CONTROL, MOD_SHIFT,
};

pub const HOTKEY_REGION: i32 = 1;
pub const HOTKEY_WINDOW: i32 = 2;
pub const HOTKEY_FULLSCREEN: i32 = 3;
pub const HOTKEY_PRTSC: i32 = 4;

const VK_S: u32 = 0x53;
const VK_SNAPSHOT: u32 = 0x2C;
const NO_MODIFIERS: HOT_KEY_MODIFIERS = HOT_KEY_MODIFIERS(0);

pub struct HotkeyInfo {
    pub id: i32,
    pub label: &'static str,
    pub shortcut: &'static str,
    pub registered: bool,
}

pub fn register_all(hwnd: HWND) -> Vec<HotkeyInfo> {
    let mut results = Vec::new();

    // Основной набор: PrtSc с модификаторами.
    let ok = unsafe { RegisterHotKey(hwnd, HOTKEY_PRTSC, NO_MODIFIERS, VK_SNAPSHOT).is_ok() };
    results.push(HotkeyInfo {
        id: HOTKEY_PRTSC,
        label: "Снимок области",
        shortcut: "PrtSc",
        registered: ok,
    });

    let ok = unsafe { RegisterHotKey(hwnd, HOTKEY_WINDOW, MOD_CONTROL, VK_SNAPSHOT).is_ok() };
    results.push(HotkeyInfo {
        id: HOTKEY_WINDOW,
        label: "Снимок окна",
        shortcut: "Ctrl+PrtSc",
        registered: ok,
    });

    let ok = unsafe { RegisterHotKey(hwnd, HOTKEY_FULLSCREEN, MOD_SHIFT, VK_SNAPSHOT).is_ok() };
    results.push(HotkeyInfo {
        id: HOTKEY_FULLSCREEN,
        label: "Снимок экрана",
        shortcut: "Shift+PrtSc",
        registered: ok,
    });

    // Резервная комбинация для области.
    let ok = unsafe { RegisterHotKey(hwnd, HOTKEY_REGION, MOD_CONTROL | MOD_SHIFT, VK_S).is_ok() };
    results.push(HotkeyInfo {
        id: HOTKEY_REGION,
        label: "Снимок области (резерв)",
        shortcut: "Ctrl+Shift+S",
        registered: ok,
    });

    results
}

pub fn unregister_all(hwnd: HWND) {
    unsafe {
        let _ = UnregisterHotKey(hwnd, HOTKEY_REGION);
        let _ = UnregisterHotKey(hwnd, HOTKEY_WINDOW);
        let _ = UnregisterHotKey(hwnd, HOTKEY_FULLSCREEN);
        let _ = UnregisterHotKey(hwnd, HOTKEY_PRTSC);
    }
}