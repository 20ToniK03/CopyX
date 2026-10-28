//! Application state management using thread-safe global state.

use std::sync::{Mutex, OnceLock};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use copyx_core::CaptureState;

static APP_STATE: OnceLock<Mutex<AppState>> = OnceLock::new();

pub struct AppState {
    pub overlay_hwnd: HWND,
    pub tray_hwnd: HWND,
    pub instance: windows::Win32::Foundation::HINSTANCE,
    pub selection: Selection,
    pub pointer_down: bool,
    pub hover_window: HWND,
    pub hover_rect: RECT,
    pub capture_state: CaptureState,
}

#[derive(Clone, Copy, Debug)]
pub struct Selection {
    pub active: bool,
    pub start: POINT,
    pub current: POINT,
}

impl Default for Selection {
    fn default() -> Self {
        Self {
            active: false,
            start: POINT { x: 0, y: 0 },
            current: POINT { x: 0, y: 0 },
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            overlay_hwnd: HWND(0),
            tray_hwnd: HWND(0),
            instance: windows::Win32::Foundation::HINSTANCE(0),
            selection: Selection::default(),
            pointer_down: false,
            hover_window: HWND(0),
            hover_rect: RECT::default(),
            capture_state: CaptureState::Idle,
        }
    }
}

pub fn init(instance: windows::Win32::Foundation::HINSTANCE) {
    APP_STATE.get_or_init(|| {
        Mutex::new(AppState {
            instance,
            ..Default::default()
        })
    });
}

pub fn with_state<F, R>(f: F) -> R
where
    F: FnOnce(&mut AppState) -> R,
{
    let mutex = APP_STATE.get().expect("AppState not initialized");
    let mut state = mutex.lock().unwrap();
    f(&mut state)
}

pub fn selection_rect(sel: &Selection) -> RECT {
    RECT {
        left: sel.start.x.min(sel.current.x),
        top: sel.start.y.min(sel.current.y),
        right: sel.start.x.max(sel.current.x),
        bottom: sel.start.y.max(sel.current.y),
    }
}