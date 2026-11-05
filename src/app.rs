//! Application state management using thread-safe global state.

use std::sync::{Mutex, OnceLock};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::HFONT;
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
    /// Индекс выделенной фигуры (для поворота и удаления).
    pub selected: Option<usize>,
    /// Пользователь тянет ручку поворота выделенной фигуры.
    pub rotating: bool,
    /// Пользователь перетаскивает выделенную фигуру.
    pub moving: bool,
    /// Позиция курсора на предыдущем шаге перетаскивания.
    pub move_last: POINT,
    /// Прежняя рамка на время перевыделения: если пользователь просто
    /// кликнул по пустому месту, рамка и аннотации возвращаются.
    pub reclaim_selection: Option<Selection>,
    /// Цвет линий фигур и текста (COLORREF).
    pub color: u32,
    /// Заливка: полупрозрачный фон у фигур и подложка под текстом.
    pub fill: bool,
    /// Интенсивность заливки: 1 (почти прозрачная) – 5 (плотная).
    pub fill_level: i32,
    /// Сила размытия/пикселизации: 1–5.
    pub intensity: i32,
    /// Открыт ввод текста: дочерний EDIT виден и получает клавиши.
    pub text_active: bool,
    /// Точка, где пользователь начал вводить текст (экранные координаты).
    pub text_origin: POINT,
    /// DPI монитора на момент начала ввода.
    pub text_dpi: u32,
    /// Шрифт, которым набран текст в EDIT; освобождается при завершении.
    pub text_font: HFONT,
    /// Дочернее поле ввода текста.
    pub text_edit: HWND,
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
    Text,
}

impl Tool {
    pub const ALL: [Tool; 8] = [
        Tool::Select,
        Tool::Pixelate,
        Tool::Blur,
        Tool::Arrow,
        Tool::Line,
        Tool::Rect,
        Tool::Ellipse,
        Tool::Text,
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
            Tool::Text => "Текст",
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
    /// Поворот фигуры вокруг центра её рамки, в радианах.
    pub angle: f32,
    /// Толщина линии, уже масштабированная под DPI.
    pub width: i32,
    pub effect: Option<BitmapDc>,
    /// Цвет контура/текста (COLORREF).
    pub color: u32,
    /// Полупрозрачная заливка фигуры или подложка под текстом.
    pub fill: bool,
    /// Интенсивность заливки: 1–5.
    pub fill_level: i32,
    /// Текст аннотации (только для Tool::Text).
    pub text: String,
    /// Размер шрифта текста, уже масштабированный под DPI.
    pub font_size: i32,
    /// Сила эффекта 1–5 (только для Pixelate/Blur).
    pub strength: i32,
}

impl Annotation {
    /// Прямоугольник фигуры без учёта поворота.
    pub fn bounds(&self) -> RECT {
        RECT {
            left: self.start.x.min(self.end.x),
            top: self.start.y.min(self.end.y),
            right: self.start.x.max(self.end.x),
            bottom: self.start.y.max(self.end.y),
        }
    }

    /// Центр вращения — центр рамки фигуры.
    pub fn center(&self) -> POINT {
        let bounds = self.bounds();
        POINT {
            x: (bounds.left + bounds.right) / 2,
            y: (bounds.top + bounds.bottom) / 2,
        }
    }
}

/// Черновик аннотации, пока пользователь тянет мышь.
pub struct Draft {
    pub start: POINT,
    pub end: POINT,
    /// Shift удерживался в последнем движении: ровная фигура/линия.
    pub snap: bool,
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
            snap: false,
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
            selected: None,
            rotating: false,
            moving: false,
            move_last: POINT::default(),
            reclaim_selection: None,
            color: 0x004D5AFF,
            fill: false,
            fill_level: 2,
            intensity: 3,
            text_active: false,
            text_origin: POINT::default(),
            text_dpi: 96,
            text_font: HFONT(0),
            text_edit: HWND(0),
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