//! Application state management using thread-safe global state.

use std::sync::{Mutex, OnceLock};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use copyx_core::CaptureState;

use crate::render::BitmapDc;

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
    /// Активный инструмент редактора.
    pub tool: Tool,
    /// Готовые аннотации в координатах экрана.
    pub annotations: Vec<Annotation>,
    /// Аннотация, которую пользователь рисует прямо сейчас.
    pub draft: Option<Draft>,
    /// Подсвеченная кнопка панели инструментов (0 — нет, 1–7 — инструмент, 8 — «Копировать»).
    pub hot: i32,
}

/// Инструменты редактора, доступные после нажатия PrtSc.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Select,
    Pixelate,
    Blur,
    Arrow,
    Line,
    Rect,
    Ellipse,
}

impl Tool {
    pub const ALL: [Tool; 7] = [
        Tool::Select,
        Tool::Pixelate,
        Tool::Blur,
        Tool::Arrow,
        Tool::Line,
        Tool::Rect,
        Tool::Ellipse,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Tool::Select => "Область",
            Tool::Pixelate => "Пиксели",
            Tool::Blur => "Размытие",
            Tool::Arrow => "Стрелка",
            Tool::Line => "Линия",
            Tool::Rect => "Квадрат",
            Tool::Ellipse => "Круг",
        }
    }

    pub fn is_effect(self) -> bool {
        matches!(self, Tool::Pixelate | Tool::Blur)
    }

    pub fn is_shape(self) -> bool {
        matches!(self, Tool::Arrow | Tool::Line | Tool::Rect | Tool::Ellipse)
    }
}

/// Нарисованная аннотация. Для эффектов заранее готовится bitmap,
/// чтобы превью не пересчитывалось на каждой перерисовке.
pub struct Annotation {
    pub tool: Tool,
    pub start: POINT,
    pub end: POINT,
    /// Толщина линии, уже масштабированная под DPI.
    pub width: i32,
    pub effect: Option<BitmapDc>,
}

/// Черновик аннотации, пока пользователь тянет мышь.
pub struct Draft {
    pub start: POINT,
    pub end: POINT,
    /// Живое превью эффекта: обновляется не чаще, чем раз в несколько кадров.
    pub preview: Option<BitmapDc>,
    /// Область, для которой построено превью.
    pub preview_rect: Option<RECT>,
    /// Когда превью пересчитывалось в последний раз.
    pub preview_at: Option<std::time::Instant>,
}

impl Draft {
    pub fn new(start: POINT) -> Self {
        Self {
            start,
            end: start,
            preview: None,
            preview_rect: None,
            preview_at: None,
        }
    }
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
            tool: Tool::Select,
            annotations: Vec::new(),
            draft: None,
            hot: 0,
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