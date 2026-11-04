//! Fullscreen overlay: region/window selection **and** annotation editor.
//!
//! Visual model (ShareX-like):
//! * the screen is captured **before** the overlay appears;
//! * the frozen frame is painted back dimmed;
//! * the selected region is painted at full brightness;
//! * annotations (pixelate, blur, arrows, lines, rectangles, ellipses) are drawn
//!   on top of the frozen frame;
//! * tools, hints and coordinate/size badges are frosted glass panels.
//!
//! The selection stays after the mouse is released, so the user can apply as
//! many annotations as needed. Enter or «Копировать» finishes the capture,
//! Esc / right click cancels it.

use crate::app::{self, Annotation, Draft, Selection, Tool};
use crate::capture::{self, CapturedImage};
use crate::clipboard;
use crate::render::*;
use crate::snapshot;
use crate::toast;
use crate::ui;
use copyx_core::CaptureState;
use std::mem::size_of;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows::core::w;
use windows::Win32::Foundation::{
    BOOL, COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::Graphics::Gdi::{
    AlphaBlend, Arc, BitBlt, CreatePolygonRgn, CreateRoundRectRgn, CreateSolidBrush,
    DeleteObject, Ellipse, FillRect, GetMonitorInfoW, GetStockObject, IntersectClipRect,
    InvalidateRect, LineTo, MonitorFromPoint, MoveToEx, Polygon, Polyline, RestoreDC, SaveDC,
    SelectClipRgn, SelectObject, SetBkColor, SetTextColor, UpdateWindow, AC_SRC_OVER,
    BLENDFUNCTION, HDC, HFONT, HRGN, MONITORINFO, MONITOR_DEFAULTTONEAREST, NULL_BRUSH, NULL_PEN,
    SRCCOPY, WINDING,
};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Controls::{EM_SETSEL, EM_SETLIMITTEXT};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, SetFocus};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::*;

// --- Палитра (COLORREF = 0x00BBGGRR) ---
const ACCENT: u32 = 0x00FFB86B; // RGB(107, 184, 255) — мягкий голубой
const ACCENT_HOT: u32 = 0x00FFD7A0; // RGB(160, 215, 255) — подсвеченная ручка
const ACCENT_BGRA: [u8; 4] = [0xFF, 0xB8, 0x6B, 255];
const PANEL_BG: u32 = 0x00221612; // тёмное "стекло" RGB(18, 22, 34)
const PANEL_BG_BGRA: [u8; 4] = [0x12, 0x16, 0x22, 255]; // тот же цвет в BGRA
const SIZE_BG_BGRA: [u8; 4] = [0x6E, 0x34, 0x18, 255]; // тёмно-синее стекло для размера
const PANEL_BORDER: u32 = 0x00785A46; // RGB(70, 90, 120)
const GLASS_BORDER: u32 = 0x00BE9A7A; // RGB(122, 154, 190) — светлая стеклянная кромка
const COPY_BORDER: u32 = 0x00FFE6D2; // светлая кромка акцентной кнопки
const TEXT_MAIN: u32 = 0x00FFFFFF;
const TEXT_MUTED: u32 = 0x00D8C8B4; // RGB(180, 200, 216)
const WHITE: u32 = 0x00FFFFFF;

/// Палитра линий и текста (COLORREF).
const PALETTE: [u32; 9] = [
    0x00FFFFFF, // белый
    0x004D5AFF, // красный RGB(255, 90, 77)
    0x003DA5FF, // оранжевый RGB(255, 165, 61)
    0x004AD5FF, // жёлтый RGB(255, 213, 74)
    0x0080DE4A, // зелёный RGB(74, 222, 128)
    0x00E1E138, // голубой RGB(56, 225, 225)
    0x00FFA95A, // синий RGB(90, 169, 255)
    0x00FC84C0, // фиолетовый RGB(192, 132, 252)
    0x00281E1E, // графит RGB(30, 30, 40)
];

/// Непрозрачность заливки фигур по уровню 1–5 (2 — прежнее значение).
const FILL_SHAPE_ALPHA: [u8; 5] = [40, 70, 105, 145, 195];
/// Непрозрачность подложки под текстом по уровню 1–5.
const FILL_TEXT_ALPHA: [u8; 5] = [150, 190, 220, 240, 250];

fn fill_shape_alpha(level: i32) -> u8 {
    FILL_SHAPE_ALPHA[(level.clamp(1, 5) - 1) as usize]
}

fn fill_text_alpha(level: i32) -> u8 {
    FILL_TEXT_ALPHA[(level.clamp(1, 5) - 1) as usize]
}

/// Кисть фона дочернего EDIT: создаётся один раз и живёт до выхода.
static EDIT_BG: OnceLock<isize> = OnceLock::new();

fn edit_bg_brush() -> usize {
    *EDIT_BG.get_or_init(|| unsafe { CreateSolidBrush(COLORREF(PANEL_BG)).0 }) as usize
}

/// Enter завершает ввод текста, Esc отменяет его.
unsafe extern "system" fn edit_subclass(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    if msg == WM_KEYDOWN {
        match wparam.0 as u32 {
            // Enter — добавить текст, Esc — отменить ввод.
            0x0D => {
                let _ = PostMessageW(GetParent(hwnd), WM_TEXT_COMMIT, WPARAM(0), LPARAM(0));
                return LRESULT(0);
            }
            0x1B => {
                let _ = PostMessageW(GetParent(hwnd), WM_TEXT_CANCEL, WPARAM(0), LPARAM(0));
                return LRESULT(0);
            }
            _ => {}
        }
    }
    DefSubclassProc(hwnd, msg, wparam, lparam)
}

/// Подсказка, пока область ещё не выделена.
const HINT_SELECT: &str =
    "Зажмите ЛКМ и выделите область · короткий клик — окно · Esc/СКМ — отмена · ПКМ — копировать";

/// Подсказка редактора: панель инструментов уже доступна.
const HINT_EDIT: &str =
    "1–8 — инструменты · Shift — ровно · C — цвет · F — заливка · [ ] — сила/фон · Enter/ПКМ — копировать · СКМ — отмена";

/// Подсказка во время ввода текста.
const HINT_TEXT: &str = "Введите текст · Enter — добавить · Esc — отмена";

/// Сообщения от дочернего поля ввода текста.
const WM_TEXT_COMMIT: u32 = WM_APP + 1;
const WM_TEXT_CANCEL: u32 = WM_APP + 2;

/// Кнопки панели инструментов.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ToolbarHit {
    Tool(Tool),
    Copy,
    /// Выбор цвета линий/текста (индекс в PALETTE).
    Color(usize),
    /// Включить/выключить заливку.
    Fill,
    /// Задать силу эффекта 1–5.
    IntensitySet(i32),
    /// Уменьшить/увеличить силу на 1.
    IntensityStep(i32),
}

/// Что регулирует общая шкала «Сила/Фон».
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum StyleTarget {
    Effect,
    Fill,
}

struct ToolbarLayout {
    panel: RECT,
    buttons: Vec<(RECT, ToolbarHit)>,
    /// Кнопки второго ряда: цвет, заливка и сила эффекта.
    options: Vec<(RECT, ToolbarHit)>,
    /// Подписи «Цвет» и «Сила» рядом с группами.
    labels: Vec<(RECT, &'static str)>,
    /// Тонкая линия-разделитель перед акцентной кнопкой «Копировать».
    separator: Option<RECT>,
}

pub fn show(instance: HINSTANCE) {
    let needs_create = app::with_state(|s| s.overlay_hwnd.0 == 0);

    if needs_create {
        create_overlay(instance);
    }

    let hwnd = app::with_state(|s| s.overlay_hwnd);
    if hwnd.0 == 0 {
        return;
    }

    // Повторный PrtSc при уже открытом оверлее игнорируем.
    if unsafe { IsWindowVisible(hwnd).as_bool() } {
        return;
    }

    // Панель от предыдущего снимка не должна попасть в новый кадр.
    toast::hide();
    cancel_text();

    // Снимок делаем ДО показа окна, пока экран ничем не перекрыт.
    if let Err(e) = snapshot::capture() {
        show_error(&format!("Не удалось сделать снимок экрана:\n{e}"));
        return;
    }

    app::with_state(|s| {
        s.selection = Selection::default();
        s.pointer_down = false;
        s.hover_window = HWND(0);
        s.hover_rect = RECT::default();
        s.capture_state = CaptureState::OverlayShown;
        // Каждый новый снимок начинается с выделения области.
        s.tool = Tool::Select;
        s.annotations.clear();
        s.draft = None;
        s.hot = 0;
        s.selected = None;
        s.rotating = false;
        s.moving = false;
    });

    unsafe {
        ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(hwnd);

        // Подсветить окно под курсором сразу, ещё до первого движения мыши.
        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        let (candidate, bounds) = window_at_point(cursor, hwnd);
        app::with_state(|s| {
            s.hover_window = candidate;
            s.hover_rect = bounds;
        });

        let _ = InvalidateRect(hwnd, None, false);
        let _ = UpdateWindow(hwnd);
    }
}

pub fn hide() {
    cancel_text();
    let hwnd = app::with_state(|s| {
        s.capture_state = CaptureState::Idle;
        s.selection = Selection::default();
        s.pointer_down = false;
        s.hover_window = HWND(0);
        s.hover_rect = RECT::default();
        s.tool = Tool::Select;
        s.annotations.clear();
        s.draft = None;
        s.hot = 0;
        s.selected = None;
        s.rotating = false;
        s.moving = false;
        s.overlay_hwnd
    });
    if hwnd.0 != 0 {
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
    snapshot::clear();
    clear_back_buffer();
}

fn create_overlay(instance: HINSTANCE) {
    unsafe {
        let class_name = w!("CopyXOverlay");
        let class = WNDCLASSW {
            hInstance: instance,
            lpfnWndProc: Some(overlay_proc),
            lpszClassName: class_name,
            hCursor: LoadCursorW(None, IDC_CROSS).unwrap_or_default(),
            ..Default::default()
        };
        let _ = RegisterClassW(&class);

        let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let w = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let h = GetSystemMetrics(SM_CYVIRTUALSCREEN);

        // Без WS_EX_LAYERED / акрила: фон рисуется из замороженного снимка.
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class_name,
            w!(""),
            WS_POPUP,
            x,
            y,
            w,
            h,
            None,
            HMENU::default(),
            instance,
            None,
        );

        // Поле ввода текста — обычный EDIT, скрытый до выбора инструмента «Текст».
        let edit = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("EDIT"),
            w!(""),
            WS_CHILD | WINDOW_STYLE((ES_LEFT | ES_AUTOHSCROLL | ES_NOHIDESEL) as u32),
            0,
            0,
            0,
            0,
            hwnd,
            HMENU::default(),
            instance,
            None,
        );
        let _ = SetWindowSubclass(edit, Some(edit_subclass), 1, 0);
        let _ = SendMessageW(edit, EM_SETLIMITTEXT, WPARAM(512), LPARAM(0));

        app::with_state(|s| {
            s.overlay_hwnd = hwnd;
            s.text_edit = edit;
        });
    }
}

// ---------------------------------------------------------------------------
// Поиск окна под курсором
// ---------------------------------------------------------------------------

struct WindowSearch {
    overlay: HWND,
    point: POINT,
    pid: u32,
    result: HWND,
}

/// Найти верхнее видимое окно под точкой, пропуская собственный оверлей,
/// окна нашего процесса, невидимые/свёрнутые/скрытые (cloaked) окна.
unsafe fn window_at_point(cursor: POINT, overlay: HWND) -> (HWND, RECT) {
    let mut ctx = WindowSearch {
        overlay,
        point: cursor,
        pid: GetCurrentProcessId(),
        result: HWND(0),
    };

    let _ = EnumWindows(
        Some(window_search_proc),
        LPARAM(&mut ctx as *mut _ as isize),
    );

    if ctx.result.0 == 0 {
        return (HWND(0), RECT::default());
    }

    (ctx.result, window_frame_rect(ctx.result))
}

unsafe extern "system" fn window_search_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut WindowSearch);

    // EnumWindows идёт в z-order сверху вниз — первое подходящее окно и есть
    // то, что видит пользователь.
    if ctx.result.0 != 0 {
        return BOOL(0);
    }

    if hwnd == ctx.overlay || hwnd.0 == 0 {
        return BOOL(1);
    }

    if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
        return BOOL(1);
    }

    // Окна CopyX (трей, панель действий) не должны подсвечиваться.
    let mut pid = 0u32;
    let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid == ctx.pid {
        return BOOL(1);
    }

    // Click-through оверлеи и прочие невидимые помощники.
    let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    if ex_style & WS_EX_TRANSPARENT.0 != 0 {
        return BOOL(1);
    }

    // UWP-приложения держат невидимые "cloaked" окна на весь экран.
    let mut cloaked = 0u32;
    let cloak_ok = DwmGetWindowAttribute(
        hwnd,
        DWMWA_CLOAKED,
        &mut cloaked as *mut _ as *mut _,
        size_of::<u32>() as u32,
    );
    if cloak_ok.is_ok() && cloaked != 0 {
        return BOOL(1);
    }

    let rect = window_frame_rect(hwnd);
    if rect.right - rect.left < 2 || rect.bottom - rect.top < 2 {
        return BOOL(1);
    }

    let inside = ctx.point.x >= rect.left
        && ctx.point.x < rect.right
        && ctx.point.y >= rect.top
        && ctx.point.y < rect.bottom;
    if !inside {
        return BOOL(1);
    }

    ctx.result = hwnd;
    BOOL(0)
}

unsafe fn window_frame_rect(hwnd: HWND) -> RECT {
    let mut rect = RECT::default();
    let ok = DwmGetWindowAttribute(
        hwnd,
        DWMWA_EXTENDED_FRAME_BOUNDS,
        &mut rect as *mut _ as *mut _,
        size_of::<RECT>() as u32,
    );
    if ok.is_err() || rect.right - rect.left < 1 || rect.bottom - rect.top < 1 {
        let _ = GetWindowRect(hwnd, &mut rect);
    }
    rect
}

// ---------------------------------------------------------------------------
// Оконная процедура
// ---------------------------------------------------------------------------

unsafe extern "system" fn overlay_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_KEYDOWN => {
            let key = wparam.0 as u32;
            let ctrl = ctrl_down();
            match key {
                // Esc — снять выделение с фигуры, иначе закрыть оверлей.
                0x1B => {
                    if app::with_state(|s| s.text_active) {
                        cancel_text();
                        let _ = InvalidateRect(hwnd, None, false);
                        return LRESULT(0);
                    }
                    let deselected = app::with_state(|s| {
                        s.rotating = false;
                        s.moving = false;
                        s.selected.take().is_some()
                    });
                    if deselected {
                        let _ = InvalidateRect(hwnd, None, false);
                    } else {
                        hide();
                    }
                }
                // Delete / Backspace — удалить выделенную фигуру.
                0x2E | 0x08 => delete_selected(hwnd),
                // Enter — скопировать текущее выделение (или окно под курсором).
                0x0D => {
                    if app::with_state(|s| s.text_active) {
                        commit_text();
                        let _ = InvalidateRect(hwnd, None, false);
                        return LRESULT(0);
                    }
                    if let Some(target) = current_target() {
                        finish_capture(target);
                    }
                }
                // Ctrl+A — выделить весь экран, но не копировать сразу.
                0x41 if ctrl => select_all(hwnd),
                // Ctrl+C — то же, что Enter: скопировать выделение.
                0x43 if ctrl => {
                    if let Some(target) = current_target() {
                        finish_capture(target);
                    }
                }
                // C без Ctrl — следующий цвет из палитры.
                0x43 => cycle_color(hwnd),
                // F — включить/выключить заливку.
                0x46 => toggle_fill(hwnd),
                // [ и ] — сила размытия/пикселизации.
                0xDB => change_intensity(hwnd, -1),
                0xDD => change_intensity(hwnd, 1),
                // Ctrl+Z — отменить последнюю аннотацию.
                0x5A if ctrl => undo(hwnd),
                // 1–8 — выбор инструмента (Ctrl+1…8 тоже работает).
                0x31..=0x38 => {
                    let tool = Tool::ALL[(key - 0x31) as usize];
                    set_tool(hwnd, tool);
                }
                _ => {}
            }
            LRESULT(0)
        }

        // Тёмное поле ввода текста в стиле стеклянной панели.
        WM_CTLCOLOREDIT => {
            let hdc = HDC(wparam.0 as isize);
            unsafe {
                let _ = SetTextColor(hdc, COLORREF(WHITE));
                let _ = SetBkColor(hdc, COLORREF(PANEL_BG));
            }
            LRESULT(edit_bg_brush() as isize)
        }

        WM_TEXT_COMMIT => {
            commit_text();
            let _ = InvalidateRect(hwnd, None, false);
            LRESULT(0)
        }

        WM_TEXT_CANCEL => {
            cancel_text();
            let _ = InvalidateRect(hwnd, None, false);
            LRESULT(0)
        }

        // ПКМ — то же, что Enter: скопировать выделение (или окно под курсором).
        WM_RBUTTONUP => {
            if app::with_state(|s| s.text_active) {
                commit_text();
                let _ = InvalidateRect(hwnd, None, false);
                return LRESULT(0);
            }
            if let Some(target) = current_target() {
                finish_capture(target);
            }
            LRESULT(0)
        }

        // Нажатие колеса (СКМ) — отмена снимка.
        WM_MBUTTONUP => {
            hide();
            LRESULT(0)
        }

        // Потеря фокуса/отмена режима — жест прерываем, чтобы состояние
        // перетаскивания не «залипло».
        WM_KILLFOCUS | WM_CANCELMODE => {
            app::with_state(|s| {
                s.pointer_down = false;
                s.rotating = false;
                s.moving = false;
            });
            LRESULT(0)
        }

        WM_SETCURSOR => {
            let mut cursor = POINT::default();
            let _ = GetCursorPos(&mut cursor);
            let dpi = dpi_at(cursor);
            let over_toolbar = toolbar_hit(hwnd, cursor).is_some();
            let over_handle = rotation_handle_at(cursor, dpi).is_some();
            let over_move = !over_handle && move_hit_at(cursor, dpi).is_some();
            let over_shape = !over_handle
                && !over_move
                && app::with_state(|s| s.tool == Tool::Select && !s.rotating)
                && (hit_annotation_at(cursor, dpi).is_some()
                    || hit_interior_at(cursor).is_some());
            let shape = if over_toolbar {
                IDC_ARROW
            } else if over_move {
                IDC_SIZEALL
            } else if over_handle || over_shape {
                IDC_HAND
            } else {
                IDC_CROSS
            };
            if let Ok(handle) = LoadCursorW(None, shape) {
                SetCursor(handle);
            }
            LRESULT(1)
        }

        WM_LBUTTONDOWN => {
            let mut cursor = POINT::default();
            let _ = GetCursorPos(&mut cursor);

            // Клик мимо поля ввода завершает текст, а уже потом обрабатывается
            // как обычное нажатие (в том числе по панели).
            let was_editing = app::with_state(|s| s.text_active);
            if was_editing {
                commit_text();
            }

            // Клик по панели инструментов не начинает выделение.
            if let Some(hit) = toolbar_hit(hwnd, cursor) {
                match hit {
                    ToolbarHit::Tool(tool) => set_tool(hwnd, tool),
                    ToolbarHit::Copy => {
                        if let Some(target) = current_target() {
                            finish_capture(target);
                        }
                    }
                    ToolbarHit::Color(index) => set_color(hwnd, index),
                    ToolbarHit::Fill => toggle_fill(hwnd),
                    ToolbarHit::IntensitySet(value) => set_control_value(hwnd, value, cursor),
                    ToolbarHit::IntensityStep(delta) => {
                        let target = control_target();
                        let current = app::with_state(|s| match target {
                            StyleTarget::Effect => s.intensity,
                            StyleTarget::Fill => s.fill_level,
                        });
                        set_control_value(hwnd, current + delta, cursor);
                    }
                }
                return LRESULT(0);
            }

            // Клик, завершивший ввод текста, не должен сразу открывать новый.
            if was_editing {
                let _ = InvalidateRect(hwnd, None, false);
                return LRESULT(0);
            }

            let dpi = dpi_at(cursor);

            // Ручка поворота выделенной фигуры — приоритетнее всего остального.
            if let Some(index) = rotation_handle_at(cursor, dpi) {
                app::with_state(|s| {
                    s.selected = Some(index);
                    s.rotating = true;
                    s.moving = false;
                    s.pointer_down = true;
                });
                let _ = InvalidateRect(hwnd, None, false);
                return LRESULT(0);
            }

            // Тянем выделенную фигуру из её тела — она остаётся активной.
            if let Some(index) = move_hit_at(cursor, dpi) {
                app::with_state(|s| {
                    s.selected = Some(index);
                    s.moving = true;
                    s.rotating = false;
                    s.pointer_down = true;
                    s.move_last = cursor;
                });
                let _ = InvalidateRect(hwnd, None, false);
                return LRESULT(0);
            }

            let (selection, tool) = app::with_state(|s| (s.selection, s.tool));

            // Инструмент «Область»: текст и эффекты выбираются кликом внутри,
            // фигуры — по контуру.
            if tool == Tool::Select {
                if let Some(index) =
                    hit_interior_at(cursor).or_else(|| hit_annotation_at(cursor, dpi))
                {
                    select_annotation(index);
                    let _ = InvalidateRect(hwnd, None, false);
                    return LRESULT(0);
                }
            }

            let sel_rect = app::selection_rect(&selection);
            let inside_selection = selection.active
                && sel_rect.right - sel_rect.left > 4
                && sel_rect.bottom - sel_rect.top > 4
                && point_in_rect(cursor, &sel_rect);

            // «Текст»: клик внутри области открывает поле ввода в этой точке.
            if inside_selection && tool == Tool::Text {
                start_text(hwnd, clamp_point(cursor, &sel_rect), &sel_rect, dpi);
                return LRESULT(0);
            }

            // «Область»: клик по пустому месту внутри выделения ничего не
            // меняет — иначе случайное нажатие стёрло бы все аннотации.
            if inside_selection && tool == Tool::Select {
                return LRESULT(0);
            }

            if inside_selection && tool != Tool::Select {
                // Рисуем аннотацию внутри уже выбранной области.
                let start = clamp_point(cursor, &sel_rect);
                app::with_state(|s| {
                    s.pointer_down = true;
                    s.capture_state = CaptureState::Selecting;
                    s.draft = Some(Draft::new(start));
                    s.selected = None;
                    s.rotating = false;
                    s.moving = false;
                });
                let _ = InvalidateRect(hwnd, None, false);
                return LRESULT(0);
            }

            // Новое выделение: старые аннотации к нему уже не относятся.
            let (candidate, bounds) = window_at_point(cursor, hwnd);
            app::with_state(|s| {
                s.pointer_down = true;
                s.selection.active = false;
                s.selection.start = cursor;
                s.selection.current = cursor;
                s.hover_window = candidate;
                s.hover_rect = bounds;
                s.annotations.clear();
                s.draft = None;
                s.selected = None;
                s.rotating = false;
                s.moving = false;
                s.capture_state = CaptureState::OverlayShown;
            });
            let _ = InvalidateRect(hwnd, None, false);
            LRESULT(0)
        }

        WM_MOUSEMOVE => {
            let mut cursor = POINT::default();
            let _ = GetCursorPos(&mut cursor);
            let dpi = dpi_at(cursor);
            // Состояние ЛКМ именно на момент генерации сообщения: очередь
            // сообщений может отставать, поэтому GetKeyState ненадёжен.
            let message_button = (wparam.0 as u32 & 0x0001) != 0; // MK_LBUTTON

            // Тянем ручку поворота: угол считается от центра фигуры.
            if app::with_state(|s| s.rotating) {
                // Защита от потерянного WM_LBUTTONUP (Alt+Tab, смена фокуса,
                // синтетический ввод): кнопка отпущена и в сообщении, и сейчас.
                if !message_button && !left_button_down() {
                    app::with_state(|s| {
                        s.rotating = false;
                        s.pointer_down = false;
                    });
                    let _ = InvalidateRect(hwnd, None, false);
                    return LRESULT(0);
                }
                apply_rotation(cursor);
                let _ = InvalidateRect(hwnd, None, false);
                return LRESULT(0);
            }

            // Перетаскиваем выделенную фигуру.
            if app::with_state(|s| s.moving) {
                if !message_button && !left_button_down() {
                    app::with_state(|s| {
                        s.moving = false;
                        s.pointer_down = false;
                    });
                    let _ = InvalidateRect(hwnd, None, false);
                    return LRESULT(0);
                }
                apply_move(cursor);
                let _ = InvalidateRect(hwnd, None, false);
                return LRESULT(0);
            }

            let (pointer_down, has_draft, selection) =
                app::with_state(|s| (s.pointer_down, s.draft.is_some(), s.selection));

            // Та же страховка для рисования/выделения: если UP потерялся,
            // завершаем жест штатным обработчиком.
            if pointer_down && !message_button && !left_button_down() {
                let _ = PostMessageW(hwnd, WM_LBUTTONUP, WPARAM(0), LPARAM(0));
                return LRESULT(0);
            }

            if pointer_down {
                if has_draft {
                    // Тянем аннотацию внутри выделения.
                    let sel_rect = app::selection_rect(&selection);
                    let tool = app::with_state(|s| s.tool);
                    let shift = shift_down();
                    let (preview_request, moved) = app::with_state(|s| {
                        let end = clamp_point(cursor, &sel_rect);
                        let mut request = None;
                        let mut moved = false;

                        if let Some(draft) = s.draft.as_mut() {
                            // Shift держит квадрат/круг и линии по 45°.
                            draft.snap = shift;
                            let end = if shift {
                                clamp_point(constrain_end(tool, draft.start, end), &sel_rect)
                            } else {
                                end
                            };
                            if draft.end.x != end.x || draft.end.y != end.y {
                                draft.end = end;
                                moved = true;
                            }

                            if moved && tool.is_effect() {
                                let rect = normalized(draft.start, draft.end);
                                let area = (rect.right - rect.left) as i64
                                    * (rect.bottom - rect.top) as i64;
                                let due = draft.preview_at.map_or(true, |at| {
                                    at.elapsed() >= Duration::from_millis(40)
                                });
                                if due
                                    && rect.right - rect.left > 2
                                    && rect.bottom - rect.top > 2
                                    // Очень большие области не превьюим: размытие
                                    // такого размера заметно подвесило бы оверлей.
                                    && area <= 900_000
                                {
                                    draft.preview_at = Some(Instant::now());
                                    request = Some((tool, rect, s.intensity));
                                }
                            }
                        }

                        (request, moved)
                    });

                    if let Some((tool, rect, strength)) = preview_request {
                        let preview = make_effect(tool, &rect, dpi, strength);
                        app::with_state(|s| {
                            if let Some(draft) = s.draft.as_mut() {
                                draft.preview = preview;
                                draft.preview_rect = Some(rect);
                            }
                        });
                    }

                    if moved || preview_request.is_some() {
                        let _ = InvalidateRect(hwnd, None, false);
                    }
                } else {
                    // Тянем новое выделение.
                    app::with_state(|s| {
                        s.selection.current = cursor;
                        let dx = (cursor.x - s.selection.start.x).abs();
                        let dy = (cursor.y - s.selection.start.y).abs();
                        if !s.selection.active && (dx > 4 || dy > 4) {
                            s.selection.active = true;
                            s.capture_state = CaptureState::Selecting;
                        }
                    });
                    let _ = InvalidateRect(hwnd, None, false);
                }
            } else {
                // Подсветка кнопок панели и окна под курсором.
                let hot = hot_key(toolbar_hit(hwnd, cursor));
                let hot_changed = app::with_state(|s| {
                    if s.hot != hot {
                        s.hot = hot;
                        true
                    } else {
                        false
                    }
                });

                if selection.active {
                    if hot_changed {
                        let _ = InvalidateRect(hwnd, None, false);
                    }
                } else {
                    let (candidate, bounds) = window_at_point(cursor, hwnd);
                    let hover_changed = app::with_state(|s| {
                        let changed = s.hover_window != candidate
                            || !same_rect(&s.hover_rect, &bounds);
                        s.hover_window = candidate;
                        s.hover_rect = bounds;
                        changed
                    });
                    if hover_changed || hot_changed {
                        let _ = InvalidateRect(hwnd, None, false);
                    }
                }
            }

            LRESULT(0)
        }

        WM_LBUTTONUP => {
            let mut cursor = POINT::default();
            let _ = GetCursorPos(&mut cursor);
            let dpi = dpi_at(cursor);

            if app::with_state(|s| s.rotating) {
                apply_rotation(cursor);
                app::with_state(|s| {
                    s.rotating = false;
                    s.pointer_down = false;
                });
                let _ = InvalidateRect(hwnd, None, false);
                return LRESULT(0);
            }

            if app::with_state(|s| s.moving) {
                apply_move(cursor);
                app::with_state(|s| {
                    s.moving = false;
                    s.pointer_down = false;
                });
                let _ = InvalidateRect(hwnd, None, false);
                return LRESULT(0);
            }

            let (pointer_down, has_draft) =
                app::with_state(|s| (s.pointer_down, s.draft.is_some()));
            if !pointer_down {
                return LRESULT(0);
            }

            if has_draft {
                let (mut draft, tool, sel_rect) = app::with_state(|s| {
                    s.pointer_down = false;
                    s.capture_state = CaptureState::OverlayShown;
                    (s.draft.take(), s.tool, app::selection_rect(&s.selection))
                });

                if let Some(draft_ref) = draft.as_mut() {
                    // Последний WM_MOUSEMOVE мог не успеть за курсором —
                    // фиксируем точный конец перетаскивания здесь.
                    let end = clamp_point(cursor, &sel_rect);
                    draft_ref.end = if shift_down() || draft_ref.snap {
                        clamp_point(constrain_end(tool, draft_ref.start, end), &sel_rect)
                    } else {
                        end
                    };
                }

                if let Some(draft) = draft {
                    let rect = normalized(draft.start, draft.end);
                    let width = rect.right - rect.left;
                    let height = rect.bottom - rect.top;
                    let big_enough = if matches!(tool, Tool::Line | Tool::Arrow) {
                        // Линия/стрелка может быть строго горизонтальной.
                        width > 2 || height > 2
                    } else {
                        width > 2 && height > 2
                    };

                    if big_enough && tool.is_effect() {
                        let strength = app::with_state(|s| s.intensity);
                        if let Some(effect) = make_effect(tool, &rect, dpi, strength) {
                            app::with_state(|state| {
                                let color = state.color;
                                let fill = state.fill;
                                state.annotations.push(Annotation {
                                    tool,
                                    start: POINT {
                                        x: rect.left,
                                        y: rect.top,
                                    },
                                    end: POINT {
                                        x: rect.right,
                                        y: rect.bottom,
                                    },
                                    angle: 0.0,
                                    width: 0,
                                    effect: Some(effect),
                                    color,
                                    fill,
                                    fill_level: state.fill_level,
                                    text: String::new(),
                                    font_size: 0,
                                    strength,
                                });
                                // Эффект сразу выделен: его можно тащить мышью.
                                state.selected = Some(state.annotations.len() - 1);
                            });
                        }
                    } else if big_enough && tool.is_shape() {
                        let stroke = s(3, dpi).max(2);
                        app::with_state(|state| {
                            let color = state.color;
                            let fill = state.fill;
                            state.annotations.push(Annotation {
                                tool,
                                start: draft.start,
                                end: draft.end,
                                angle: 0.0,
                                width: stroke,
                                effect: None,
                                color,
                                fill,
                                fill_level: state.fill_level,
                                text: String::new(),
                                font_size: 0,
                                strength: 0,
                            });
                            // Свежая фигура сразу выделена — ручка поворота рядом.
                            state.selected = Some(state.annotations.len() - 1);
                            state.rotating = false;
                        });
                    }
                }

                let _ = InvalidateRect(hwnd, None, false);
                return LRESULT(0);
            }

            // Отпустили кнопку после выделения области: не копируем сразу,
            // а оставляем рамку — пользователь может добавить аннотации.
            let (was_selecting, hover_rect) = app::with_state(|s| {
                s.pointer_down = false;
                s.selection.current = cursor;
                s.capture_state = CaptureState::OverlayShown;
                (s.selection.active, s.hover_rect)
            });

            let hover_valid = hover_rect.right - hover_rect.left > 2
                && hover_rect.bottom - hover_rect.top > 2;

            if !was_selecting && hover_valid {
                // Короткий клик — выделяем окно под курсором.
                app::with_state(|s| {
                    s.selection.active = true;
                    s.selection.start = POINT {
                        x: hover_rect.left,
                        y: hover_rect.top,
                    };
                    s.selection.current = POINT {
                        x: hover_rect.right,
                        y: hover_rect.bottom,
                    };
                });
            } else if !was_selecting {
                app::with_state(|s| s.selection.active = false);
            }

            let _ = InvalidateRect(hwnd, None, false);
            LRESULT(0)
        }

        WM_ERASEBKGND => LRESULT(1),

        WM_PAINT => {
            paint_overlay(hwnd);
            LRESULT(0)
        }

        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn same_rect(a: &RECT, b: &RECT) -> bool {
    a.left == b.left && a.top == b.top && a.right == b.right && a.bottom == b.bottom
}

fn ctrl_down() -> bool {
    unsafe { GetKeyState(0x11) < 0 } // VK_CONTROL
}

fn virtual_screen() -> RECT {
    unsafe {
        let left = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let top = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let width = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let height = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        RECT {
            left,
            top,
            right: left + width,
            bottom: top + height,
        }
    }
}

fn select_all(hwnd: HWND) {
    if app::with_state(|s| s.pointer_down) {
        return;
    }
    let rect = virtual_screen();
    app::with_state(|s| {
        s.selection.active = true;
        s.selection.start = POINT {
            x: rect.left,
            y: rect.top,
        };
        s.selection.current = POINT {
            x: rect.right,
            y: rect.bottom,
        };
        s.annotations.clear();
        s.draft = None;
        s.selected = None;
        s.capture_state = CaptureState::Selecting;
    });
    let _ = unsafe { InvalidateRect(hwnd, None, false) };
}

fn undo(hwnd: HWND) {
    let changed = app::with_state(|s| {
        if s.draft.is_some() || s.pointer_down {
            return false;
        }
        s.selected = None;
        s.rotating = false;
        s.moving = false;
        s.annotations.pop().is_some()
    });
    if changed {
        let _ = unsafe { InvalidateRect(hwnd, None, false) };
    }
}

/// Угол поворота выделенной фигуры по положению курсора.
fn apply_rotation(cursor: POINT) {
    let snap = shift_down();
    app::with_state(|s| {
        let Some(index) = s.selected else {
            return;
        };
        let Some(annotation) = s.annotations.get(index) else {
            return;
        };
        let center = annotation.center();
        let angle = ((cursor.y - center.y) as f32).atan2((cursor.x - center.x) as f32)
            + std::f32::consts::FRAC_PI_2;
        let angle = if snap {
            snap_angle(angle, std::f32::consts::PI / 12.0)
        } else {
            angle
        };
        if let Some(annotation) = s.annotations.get_mut(index) {
            annotation.angle = angle;
        }
    });
}

fn delete_selected(hwnd: HWND) {
    let removed = app::with_state(|s| match s.selected {
        Some(index) if index < s.annotations.len() => {
            s.annotations.remove(index);
            s.selected = None;
            s.rotating = false;
            s.moving = false;
            true
        }
        _ => false,
    });
    if removed {
        let _ = unsafe { InvalidateRect(hwnd, None, false) };
    }
}

fn set_tool(hwnd: HWND, tool: Tool) {
    let changed = app::with_state(|s| {
        // До выделения области панель инструментов недоступна.
        if s.pointer_down || !s.selection.active {
            return false;
        }
        let changed = s.tool != tool || s.selected.is_some();
        s.tool = tool;
        s.hot = 0;
        s.selected = None;
        s.rotating = false;
        s.moving = false;
        changed
    });
    if changed {
        let _ = unsafe { InvalidateRect(hwnd, None, false) };
    }
}

/// Что захватывать по Enter: активное выделение, иначе окно под курсором.
fn current_target() -> Option<RECT> {
    app::with_state(|s| {
        if s.selection.active {
            let sel = app::selection_rect(&s.selection);
            if sel.right - sel.left > 4 && sel.bottom - sel.top > 4 {
                return Some(sel);
            }
        }
        let hover = s.hover_rect;
        if hover.right - hover.left > 2 && hover.bottom - hover.top > 2 {
            Some(hover)
        } else {
            None
        }
    })
}

/// Вырезать область из замороженного кадра, наложить аннотации,
/// скопировать в буфер и показать тост.
fn finish_capture(target: RECT) {
    // Аннотации забираем до hide(): hide очищает состояние.
    let annotations = app::with_state(|s| std::mem::take(&mut s.annotations));

    // Режем ровно то, что видел пользователь: без повторного захвата экрана
    // и без мигания оверлея.
    let cropped = snapshot::with_current(|snap| snap.crop(&target));

    hide();

    match cropped {
        Some(Ok(img)) => {
            let composed = compose(&img, &target, &annotations);
            let img = composed.unwrap_or(img);
            let copied = clipboard::copy_to_clipboard(&img).is_ok();
            toast::show(img, copied);
        }
        Some(Err(e)) => show_error(&format!("Ошибка захвата:\n{e}")),
        None => show_error("Снимок экрана потерян, попробуйте ещё раз."),
    }
}

/// Наложить эффекты и фигуры на итоговый снимок.
fn compose(
    base: &CapturedImage,
    target: &RECT,
    annotations: &[Annotation],
) -> Option<CapturedImage> {
    if annotations.is_empty() {
        return None;
    }

    let width = base.width as i32;
    let height = base.height as i32;
    let mut pixels = base.pixels.clone();
    capture::bgra_from_rgba(&mut pixels);

    let screen = ScreenDc::for_screen();
    let canvas = BitmapDc::from_bgra(screen.handle(), width, height, &pixels)?;

    for annotation in annotations {
        let rect = normalized(annotation.start, annotation.end);
        let local = RECT {
            left: rect.left - target.left,
            top: rect.top - target.top,
            right: rect.right - target.left,
            bottom: rect.bottom - target.top,
        };

        if let Some(effect) = annotation.effect.as_ref() {
            if annotation.tool.is_effect() {
                let w = local.right - local.left;
                let h = local.bottom - local.top;
                if w > 0 && h > 0 {
                    unsafe {
                        let _ = BitBlt(
                            canvas.handle(),
                            local.left,
                            local.top,
                            w,
                            h,
                            effect.handle(),
                            0,
                            0,
                            SRCCOPY,
                        );
                    }
                }
                continue;
            }
        }

        if annotation.tool == Tool::Text {
            paint_text(
                canvas.handle(),
                offset_point(annotation.start, target),
                offset_point(annotation.end, target),
                &annotation.text,
                annotation.font_size,
                annotation.color,
                annotation.fill,
                annotation.fill_level,
                dpi_at(POINT {
                    x: annotation.start.x,
                    y: annotation.start.y,
                }),
            );
            continue;
        }

        if annotation.tool.is_shape() {
            let from = offset_point(annotation.start, target);
            let to = offset_point(annotation.end, target);
            if annotation.fill {
                paint_shape_fill(
                    canvas.handle(),
                    annotation.tool,
                    from,
                    to,
                    annotation.angle,
                    annotation.color,
                    annotation.fill_level,
                );
            }
            paint_shape(
                canvas.handle(),
                annotation.tool,
                from,
                to,
                annotation.angle,
                annotation.width,
                annotation.color,
            );
        }
    }

    let mut out = canvas.read_bgra(width, height)?;
    capture::rgba_from_bgra(&mut out);
    Some(CapturedImage {
        width: width as u32,
        height: height as u32,
        pixels: out,
    })
}

fn make_effect(tool: Tool, rect: &RECT, dpi: u32, strength: i32) -> Option<BitmapDc> {
    let level = strength.clamp(1, 5);
    let block = match level {
        1 => 6,
        2 => 9,
        3 => 13,
        4 => 18,
        _ => 24,
    };
    let radius = match level {
        1 => 3,
        2 => 5,
        3 => 8,
        4 => 12,
        _ => 18,
    };
    snapshot::with_current(|snap| match tool {
        Tool::Pixelate => snap.pixelate_bitmap(rect, s(block, dpi).max(3)),
        Tool::Blur => snap.blur_bitmap(rect, s(radius, dpi).max(2)),
        _ => None,
    })
    .flatten()
}

// ---------------------------------------------------------------------------
// Стиль, текст и выбор аннотаций
// ---------------------------------------------------------------------------

fn style_applies(tool: Tool) -> bool {
    tool.is_shape() || tool == Tool::Text
}

/// Заливка есть только у квадрата, круга и текста.
fn fill_applies(tool: Tool) -> bool {
    matches!(tool, Tool::Rect | Tool::Ellipse | Tool::Text)
}

/// Какую настройку регулирует общая шкала: силу эффекта или интенсивность фона.
fn control_target() -> StyleTarget {
    app::with_state(|s| {
        if let Some(annotation) = s.selected.and_then(|index| s.annotations.get(index)) {
            if fill_applies(annotation.tool) {
                StyleTarget::Fill
            } else {
                StyleTarget::Effect
            }
        } else if fill_applies(s.tool) {
            StyleTarget::Fill
        } else {
            StyleTarget::Effect
        }
    })
}

/// Выбрать аннотацию и подтянуть панель к её текущему стилю.
fn select_annotation(index: usize) {
    app::with_state(|s| {
        let style = s.annotations.get(index).map(|a| {
            (
                style_applies(a.tool),
                a.color,
                a.fill,
                a.tool.is_effect(),
                a.strength,
                a.fill_level,
            )
        });
        if let Some((styled, color, fill, effect, strength, fill_level)) = style {
            if styled {
                s.color = color;
                s.fill = fill;
                s.fill_level = fill_level;
            }
            if effect {
                s.intensity = strength;
            }
        }
        s.selected = Some(index);
        s.rotating = false;
        s.moving = false;
    });
}

fn set_color(hwnd: HWND, index: usize) {
    let Some(&color) = PALETTE.get(index) else {
        return;
    };
    app::with_state(|s| {
        s.color = color;
        if let Some(annotation) = s.selected.and_then(|i| s.annotations.get_mut(i)) {
            if style_applies(annotation.tool) {
                annotation.color = color;
            }
        }
    });
    let _ = unsafe { InvalidateRect(hwnd, None, false) };
}

fn cycle_color(hwnd: HWND) {
    let index = app::with_state(|s| {
        PALETTE
            .iter()
            .position(|color| *color == s.color)
            .unwrap_or(0)
    });
    set_color(hwnd, (index + 1) % PALETTE.len());
}

fn toggle_fill(hwnd: HWND) {
    app::with_state(|s| {
        let fill = !s.fill;
        s.fill = fill;
        if let Some(annotation) = s.selected.and_then(|i| s.annotations.get_mut(i)) {
            if style_applies(annotation.tool) {
                annotation.fill = fill;
            }
        }
    });
    let _ = unsafe { InvalidateRect(hwnd, None, false) };
}

/// Поменять силу эффекта. Если выделен готовый эффект — сразу пересобрать его.
fn set_intensity(hwnd: HWND, value: i32, cursor: POINT) {
    let value = value.clamp(1, 5);
    let target = app::with_state(|s| {
        s.intensity = value;
        s.selected.and_then(|index| {
            s.annotations.get(index).and_then(|annotation| {
                if annotation.tool.is_effect() {
                    Some((
                        index,
                        annotation.tool,
                        normalized(annotation.start, annotation.end),
                    ))
                } else {
                    None
                }
            })
        })
    });

    if let Some((index, tool, rect)) = target {
        let effect = make_effect(tool, &rect, dpi_at(cursor), value);
        app::with_state(|s| {
            if let Some(annotation) = s.annotations.get_mut(index) {
                annotation.strength = value;
                if let Some(effect) = effect {
                    annotation.effect = Some(effect);
                }
            }
        });
    }

    let _ = unsafe { InvalidateRect(hwnd, None, false) };
}

/// Поменять интенсивность заливки; сразу включаем её, чтобы результат был виден.
fn set_fill_level(hwnd: HWND, value: i32) {
    let value = value.clamp(1, 5);
    app::with_state(|s| {
        s.fill_level = value;
        s.fill = true;
        if let Some(annotation) = s.selected.and_then(|i| s.annotations.get_mut(i)) {
            if fill_applies(annotation.tool) {
                annotation.fill = true;
                annotation.fill_level = value;
            }
        }
    });
    let _ = unsafe { InvalidateRect(hwnd, None, false) };
}

/// Общая шкала: сила эффекта или интенсивность заливки — зависит от выделения.
fn set_control_value(hwnd: HWND, value: i32, cursor: POINT) {
    match control_target() {
        StyleTarget::Effect => set_intensity(hwnd, value, cursor),
        StyleTarget::Fill => set_fill_level(hwnd, value),
    }
}

fn change_intensity(hwnd: HWND, delta: i32) {
    let mut cursor = POINT::default();
    let _ = unsafe { GetCursorPos(&mut cursor) };
    let target = control_target();
    let current = app::with_state(|s| match target {
        StyleTarget::Effect => s.intensity,
        StyleTarget::Fill => s.fill_level,
    });
    set_control_value(hwnd, current + delta, cursor);
}

/// Открыть поле ввода текста в точке `origin` (экранные координаты).
fn start_text(hwnd: HWND, origin: POINT, selection: &RECT, dpi: u32) {
    cancel_text();
    let edit = app::with_state(|s| s.text_edit);
    if edit.0 == 0 {
        return;
    }

    let font = ui::make_font(s(24, dpi), 600);
    let max_width = (selection.right - origin.x - s(8, dpi)).max(s(80, dpi));
    let width = max_width.min(s(360, dpi));
    let height = s(32, dpi);

    let mut window = RECT::default();
    let _ = unsafe { GetWindowRect(hwnd, &mut window) };
    let local_x = origin.x - window.left;
    let local_y = origin.y - window.top;

    unsafe {
        let _ = SendMessageW(edit, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
        let _ = SetWindowTextW(edit, w!(""));
        let _ = SetWindowPos(
            edit,
            HWND_TOP,
            local_x,
            local_y,
            width,
            height,
            SWP_SHOWWINDOW,
        );
        let _ = SendMessageW(edit, EM_SETSEL, WPARAM(0), LPARAM(0));
        let _ = SetFocus(edit);
    }

    app::with_state(|s| {
        s.text_active = true;
        s.text_origin = origin;
        s.text_dpi = dpi;
        s.text_font = font;
        s.selected = None;
        s.rotating = false;
        s.moving = false;
        s.draft = None;
    });
    let _ = unsafe { InvalidateRect(hwnd, None, false) };
}

/// Завершить ввод текста и превратить его в аннотацию.
fn commit_text() {
    let (active, edit, font, origin, dpi, color, fill, selection, hwnd) = app::with_state(|s| {
        (
            s.text_active,
            s.text_edit,
            s.text_font,
            s.text_origin,
            s.text_dpi,
            s.color,
            s.fill,
            s.selection,
            s.overlay_hwnd,
        )
    });
    if !active || edit.0 == 0 {
        return;
    }

    let length = unsafe { GetWindowTextLengthW(edit) };
    let mut buffer = vec![0u16; length as usize + 1];
    let copied = unsafe { GetWindowTextW(edit, &mut buffer) };
    let text = String::from_utf16_lossy(&buffer[..copied as usize]);
    let text = text.trim().to_string();

    let font_size = s(24, dpi);
    let measured = if text.is_empty() {
        SIZE::default()
    } else {
        let screen = ScreenDc::for_screen();
        ui::measure_text(screen.handle(), &text, font)
    };

    unsafe {
        let _ = ShowWindow(edit, SW_HIDE);
        if font.0 != 0 {
            let _ = DeleteObject(font);
        }
        if hwnd.0 != 0 {
            let _ = SetFocus(hwnd);
        }
    }
    app::with_state(|s| {
        s.text_active = false;
        s.text_font = HFONT(0);
    });

    if text.is_empty() {
        return;
    }

    let pad = s(6, dpi);
    let width = measured.cx + pad * 2;
    let height = measured.cy + pad * 2;
    let sel_rect = app::selection_rect(&selection);
    let max_x = (sel_rect.right - width).max(sel_rect.left);
    let max_y = (sel_rect.bottom - height).max(sel_rect.top);
    let start = POINT {
        x: origin.x.clamp(sel_rect.left, max_x),
        y: origin.y.clamp(sel_rect.top, max_y),
    };
    let end = POINT {
        x: start.x + width,
        y: start.y + height,
    };

    app::with_state(|s| {
        s.annotations.push(Annotation {
            tool: Tool::Text,
            start,
            end,
            angle: 0.0,
            width: 0,
            effect: None,
            color,
            fill,
            fill_level: s.fill_level,
            text,
            font_size,
            strength: 0,
        });
        s.selected = Some(s.annotations.len() - 1);
        s.rotating = false;
        s.moving = false;
    });
}

/// Скрыть поле ввода и освободить его шрифт (текст при этом не добавляется).
fn cancel_text() {
    let (active, edit, font, hwnd) =
        app::with_state(|s| (s.text_active, s.text_edit, s.text_font, s.overlay_hwnd));
    if edit.0 != 0 {
        let _ = unsafe { ShowWindow(edit, SW_HIDE) };
    }
    if font.0 != 0 {
        let _ = unsafe { DeleteObject(font) };
    }
    app::with_state(|s| {
        s.text_active = false;
        s.text_font = HFONT(0);
    });
    if active && hwnd.0 != 0 {
        let _ = unsafe { SetFocus(hwnd) };
    }
}

// ---------------------------------------------------------------------------
// Отрисовка оверлея
// ---------------------------------------------------------------------------

/// Кадр оверлея рисуется в постоянный bitmap, а затем одной операцией
/// копируется в окно. Без этого GDI показывает промежуточные состояния
/// (фон, затем панели, затем текст), и при движении мыши экран мерцает.
struct BackBuffer {
    width: i32,
    height: i32,
    bitmap: BitmapDc,
}

static BACK_BUFFER: OnceLock<Mutex<Option<BackBuffer>>> = OnceLock::new();

fn back_buffer() -> &'static Mutex<Option<BackBuffer>> {
    BACK_BUFFER.get_or_init(|| Mutex::new(None))
}

fn clear_back_buffer() {
    if let Some(mutex) = BACK_BUFFER.get() {
        if let Ok(mut guard) = mutex.lock() {
            // Отпускаем bitmap (и память под него) при закрытии оверлея.
            *guard = None;
        }
    }
}

fn paint_overlay(hwnd: HWND) {
    let dc = PaintDc::begin(hwnd);
    let hdc = dc.handle();

    let mut window_rect = RECT::default();
    let mut client = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut window_rect);
        let _ = GetClientRect(hwnd, &mut client);
    }

    let width = client.right - client.left;
    let height = client.bottom - client.top;
    if width < 1 || height < 1 {
        return;
    }

    let screen = ScreenDc::for_screen();
    let mut guard = back_buffer().lock().unwrap();

    let stale = guard
        .as_ref()
        .map_or(true, |buffer| buffer.width != width || buffer.height != height);
    if stale {
        *guard = BitmapDc::compatible(screen.handle(), width, height).map(|bitmap| BackBuffer {
            width,
            height,
            bitmap,
        });
    }

    match guard.as_ref() {
        Some(buffer) => {
            paint_scene(buffer.bitmap.handle(), &window_rect, &client);
            unsafe {
                let _ = BitBlt(
                    hdc,
                    0,
                    0,
                    width,
                    height,
                    buffer.bitmap.handle(),
                    0,
                    0,
                    SRCCOPY,
                );
            }
        }
        // Не удалось создать буфер — рисуем напрямую, как раньше.
        None => paint_scene(hdc, &window_rect, &client),
    }
}

fn paint_scene(hdc: HDC, window_rect: &RECT, client: &RECT) {
    let (selection, hover_rect, pointer_down, has_draft, rotating, moving, text_active) =
        app::with_state(|s| {
            (
                s.selection,
                s.hover_rect,
                s.pointer_down,
                s.draft.is_some(),
                s.rotating,
                s.moving,
                s.text_active,
            )
        });

    // Пока пользователь тянет новую рамку, панель не показываем: она бы
    // прыгала под курсором. После отпускания — показываем. Перетаскивание и
    // поворот фигуры панель не прячут.
    let dragging_selection = pointer_down && !has_draft && !moving && !rotating;
    let editing = selection.active && !dragging_selection;

    let has_hover = hover_rect.right - hover_rect.left > 2
        && hover_rect.bottom - hover_rect.top > 2;

    let mut cursor_screen = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut cursor_screen);
    }
    let dpi = dpi_at(cursor_screen);
    let cursor = POINT {
        x: cursor_screen.x - window_rect.left,
        y: cursor_screen.y - window_rect.top,
    };

    let painted = snapshot::with_current(|snap| {
        snap.paint_background(hdc);

        if selection.active {
            snap.paint_bright(hdc, &app::selection_rect(&selection));
        } else if has_hover {
            snap.paint_bright(hdc, &hover_rect);
        }
    });

    if painted.is_none() {
        // Аварийный фон, если снимок недоступен.
        let brush = GdiBrush::solid(PANEL_BG);
        unsafe {
            let _ = FillRect(hdc, client, brush.handle());
        }
    }

    let sel_local = if selection.active {
        Some(screen_to_local(&app::selection_rect(&selection), window_rect))
    } else {
        None
    };

    if let Some(local) = &sel_local {
        // Аннотации не должны выходить за пределы выделения.
        app::with_state(|s| {
            unsafe {
                let saved = SaveDC(hdc);
                let _ = IntersectClipRect(hdc, local.left, local.top, local.right, local.bottom);
                paint_annotations(
                    hdc,
                    window_rect,
                    &s.annotations,
                    s.draft.as_ref(),
                    s.tool,
                    s.color,
                    s.fill,
                    s.fill_level,
                    dpi,
                );
                let _ = RestoreDC(hdc, saved);
            }
        });

        draw_highlight(hdc, local, dpi);
        draw_size_badge(
            hdc,
            local,
            selection_rect_width(&selection),
            selection_rect_height(&selection),
            client,
            dpi,
        );
    } else if has_hover && !pointer_down {
        let local = screen_to_local(&hover_rect, window_rect);
        draw_highlight(hdc, &local, dpi);
        draw_size_badge(
            hdc,
            &local,
            hover_rect.right - hover_rect.left,
            hover_rect.bottom - hover_rect.top,
            client,
            dpi,
        );
    }

    // Рамка и ручка поворота выделенной фигуры. Рисуем вне клипа выделения,
    // чтобы ручку не срезало у верхней границы области.
    app::with_state(|state| {
        if let Some(annotation) = state.selected.and_then(|index| state.annotations.get(index)) {
            let handle = rotate_handle_center(annotation, dpi);
            let hot = (cursor_screen.x - handle.x).abs() <= s(12, dpi)
                && (cursor_screen.y - handle.y).abs() <= s(12, dpi);
            draw_annotation_selection(hdc, window_rect, annotation, dpi, hot);
        }
    });

    // Координаты курсора — только во время протяжки, чтобы не гонять
    // перерисовку на каждое движение мыши.
    if pointer_down && !rotating {
        let coord_text = format!("{}, {}", cursor_screen.x, cursor_screen.y);
        draw_coord_badge(hdc, cursor, &coord_text, client, dpi);
    }

    // Подсказка сверху по центру монитора, на котором находится курсор.
    let hint = if text_active {
        HINT_TEXT
    } else if editing {
        HINT_EDIT
    } else {
        HINT_SELECT
    };
    let monitor = monitor_local_rect(cursor_screen, window_rect);
    let hint_size = measure_chip(hdc, hint, s(13, dpi), dpi);
    let hint_x = (monitor.left + monitor.right - hint_size.cx) / 2;
    let hint_y = monitor.top + s(14, dpi);
    draw_chip(
        hdc,
        hint_x,
        hint_y,
        hint,
        s(13, dpi),
        TEXT_MAIN,
        PANEL_BG_BGRA,
        180,
        GLASS_BORDER,
        dpi,
    );

    // Панель инструментов — последней, чтобы она была поверх всего,
    // и только когда область уже выделена.
    if editing {
        let (active_tool, hot) = app::with_state(|s| (s.tool, s.hot));
        let layout = toolbar_layout(hdc, window_rect, cursor_screen, &selection, dpi);
        draw_toolbar(hdc, &layout, active_tool, hot, dpi);
    }
}

fn selection_rect_width(selection: &Selection) -> i32 {
    (selection.start.x - selection.current.x).abs()
}

fn selection_rect_height(selection: &Selection) -> i32 {
    (selection.start.y - selection.current.y).abs()
}

fn paint_annotations(
    hdc: HDC,
    window: &RECT,
    annotations: &[Annotation],
    draft: Option<&Draft>,
    tool: Tool,
    color: u32,
    fill: bool,
    fill_level: i32,
    dpi: u32,
) {
    for annotation in annotations {
        let rect = normalized(annotation.start, annotation.end);
        let local = screen_to_local(&rect, window);

        if annotation.tool.is_effect() {
            if let Some(effect) = annotation.effect.as_ref() {
                let w = local.right - local.left;
                let h = local.bottom - local.top;
                if w > 0 && h > 0 {
                    unsafe {
                        let _ = BitBlt(
                            hdc,
                            local.left,
                            local.top,
                            w,
                            h,
                            effect.handle(),
                            0,
                            0,
                            SRCCOPY,
                        );
                    }
                }
            }
            continue;
        }

        if annotation.tool == Tool::Text {
            paint_text(
                hdc,
                screen_to_local_point(annotation.start, window),
                screen_to_local_point(annotation.end, window),
                &annotation.text,
                annotation.font_size,
                annotation.color,
                annotation.fill,
                annotation.fill_level,
                dpi,
            );
            continue;
        }

        if annotation.tool.is_shape() {
            let from = screen_to_local_point(annotation.start, window);
            let to = screen_to_local_point(annotation.end, window);
            if annotation.fill {
                paint_shape_fill(hdc, annotation.tool, from, to, annotation.angle, annotation.color, annotation.fill_level);
            }
            paint_shape(
                hdc,
                annotation.tool,
                from,
                to,
                annotation.angle,
                annotation.width,
                annotation.color,
            );
        }
    }

    let Some(draft) = draft else {
        return;
    };

    if tool.is_effect() {
        // Живое превью эффекта, если оно уже готово, иначе акцентная рамка.
        let preview_rect = draft.preview_rect.unwrap_or_else(|| {
            normalized(draft.start, draft.end)
        });
        let local = screen_to_local(&preview_rect, window);
        if let Some(preview) = draft.preview.as_ref() {
            let w = local.right - local.left;
            let h = local.bottom - local.top;
            if w > 0 && h > 0 {
                unsafe {
                    let _ = BitBlt(
                        hdc,
                        local.left,
                        local.top,
                        w,
                        h,
                        preview.handle(),
                        0,
                        0,
                        SRCCOPY,
                    );
                }
            }
        } else {
            ui::stroke_rect(hdc, &local, s(2, dpi).max(2), ACCENT);
        }
    } else if tool.is_shape() {
        let from = screen_to_local_point(draft.start, window);
        let to = screen_to_local_point(draft.end, window);
        if fill {
            paint_shape_fill(hdc, tool, from, to, 0.0, color, fill_level);
        }
        paint_shape(
            hdc,
            tool,
            from,
            to,
            0.0,
            s(3, dpi).max(2),
            color,
        );
    }
}

// ---------------------------------------------------------------------------
// Геометрия фигур: поворот, Shift-ограничения и попадание курсора
// ---------------------------------------------------------------------------

fn shift_down() -> bool {
    unsafe { GetKeyState(0x10) < 0 } // VK_SHIFT
}

fn left_button_down() -> bool {
    unsafe { GetKeyState(0x01) < 0 } // VK_LBUTTON
}

/// Поворот точки вокруг центра (радианы; ось Y направлена вниз).
fn rotate_point(point: POINT, center: POINT, angle: f32) -> POINT {
    if angle == 0.0 {
        return point;
    }
    let (sin, cos) = angle.sin_cos();
    let dx = (point.x - center.x) as f32;
    let dy = (point.y - center.y) as f32;
    POINT {
        x: center.x + (dx * cos - dy * sin).round() as i32,
        y: center.y + (dx * sin + dy * cos).round() as i32,
    }
}

fn rotate_xy(x: f32, y: f32, cx: f32, cy: f32, sin: f32, cos: f32) -> (f32, f32) {
    let dx = x - cx;
    let dy = y - cy;
    (cx + dx * cos - dy * sin, cy + dx * sin + dy * cos)
}

fn rect_center(a: POINT, b: POINT) -> POINT {
    POINT {
        x: (a.x + b.x) / 2,
        y: (a.y + b.y) / 2,
    }
}

fn snap_angle(angle: f32, step: f32) -> f32 {
    (angle / step).round() * step
}

/// Shift во время рисования: квадрат/круг и линия под 0/45/90/135/180°.
fn constrain_end(tool: Tool, start: POINT, end: POINT) -> POINT {
    let dx = (end.x - start.x) as f32;
    let dy = (end.y - start.y) as f32;

    match tool {
        Tool::Rect | Tool::Ellipse => {
            let side = dx.abs().max(dy.abs()).round();
            let sx = if dx < 0.0 { -1.0 } else { 1.0 };
            let sy = if dy < 0.0 { -1.0 } else { 1.0 };
            POINT {
                x: start.x + (side * sx) as i32,
                y: start.y + (side * sy) as i32,
            }
        }
        Tool::Line | Tool::Arrow => {
            let length = (dx * dx + dy * dy).sqrt();
            if length < 1.0 {
                return end;
            }
            let angle = snap_angle(dy.atan2(dx), std::f32::consts::FRAC_PI_4);
            POINT {
                x: start.x + (angle.cos() * length).round() as i32,
                y: start.y + (angle.sin() * length).round() as i32,
            }
        }
        _ => end,
    }
}

fn rotated_corners(rect: &RECT, center: POINT, angle: f32) -> [POINT; 4] {
    [
        POINT {
            x: rect.left,
            y: rect.top,
        },
        POINT {
            x: rect.right,
            y: rect.top,
        },
        POINT {
            x: rect.right,
            y: rect.bottom,
        },
        POINT {
            x: rect.left,
            y: rect.bottom,
        },
    ]
    .map(|point| rotate_point(point, center, angle))
}

/// Углы рамки фигуры с учётом поворота.
fn shape_corners(from: POINT, to: POINT, angle: f32) -> [POINT; 4] {
    let rect = normalized(from, to);
    let center = POINT {
        x: (rect.left + rect.right) / 2,
        y: (rect.top + rect.bottom) / 2,
    };
    rotated_corners(&rect, center, angle)
}

fn ellipse_points(from: POINT, to: POINT, angle: f32, segments: usize) -> Vec<POINT> {
    let rect = normalized(from, to);
    let cx = (rect.left + rect.right) as f32 / 2.0;
    let cy = (rect.top + rect.bottom) as f32 / 2.0;
    let rx = (rect.right - rect.left) as f32 / 2.0;
    let ry = (rect.bottom - rect.top) as f32 / 2.0;
    let (sin, cos) = angle.sin_cos();

    (0..=segments)
        .map(|i| {
            let t = i as f32 / segments as f32 * std::f32::consts::TAU;
            let (x, y) = rotate_xy(cx + rx * t.cos(), cy + ry * t.sin(), cx, cy, sin, cos);
            POINT {
                x: x.round() as i32,
                y: y.round() as i32,
            }
        })
        .collect()
}

fn distance_to_segment(point: POINT, a: POINT, b: POINT) -> f32 {
    let (px, py) = (point.x as f32, point.y as f32);
    let (ax, ay) = (a.x as f32, a.y as f32);
    let (bx, by) = (b.x as f32, b.y as f32);
    let dx = bx - ax;
    let dy = by - ay;
    let len2 = dx * dx + dy * dy;
    if len2 < 0.001 {
        return ((px - ax).powi(2) + (py - ay).powi(2)).sqrt();
    }
    let t = (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0);
    ((px - (ax + dx * t)).powi(2) + (py - (ay + dy * t)).powi(2)).sqrt()
}

fn near_rect_outline(point: POINT, rect: &RECT, tolerance: f32) -> bool {
    let corners = [
        POINT {
            x: rect.left,
            y: rect.top,
        },
        POINT {
            x: rect.right,
            y: rect.top,
        },
        POINT {
            x: rect.right,
            y: rect.bottom,
        },
        POINT {
            x: rect.left,
            y: rect.bottom,
        },
    ];
    (0..4).any(|i| distance_to_segment(point, corners[i], corners[(i + 1) % 4]) <= tolerance)
}

fn near_ellipse_outline(point: POINT, rect: &RECT, tolerance: f32) -> bool {
    let rx = (rect.right - rect.left) as f32 / 2.0;
    let ry = (rect.bottom - rect.top) as f32 / 2.0;
    if rx < 1.0 || ry < 1.0 {
        return false;
    }
    let cx = (rect.left + rect.right) as f32 / 2.0;
    let cy = (rect.top + rect.bottom) as f32 / 2.0;
    let nx = (point.x as f32 - cx) / rx;
    let ny = (point.y as f32 - cy) / ry;
    let radius = (nx * nx + ny * ny).sqrt();
    (radius - 1.0).abs() * rx.min(ry) <= tolerance
}

/// Ближайшая к курсору фигура (только контур, чтобы клики внутри рамки
/// не мешали новому выделению).
fn hit_annotation(annotations: &[Annotation], point: POINT, dpi: u32) -> Option<usize> {
    let base = s(7, dpi).max(5) as f32;
    for (index, annotation) in annotations.iter().enumerate().rev() {
        if !annotation.tool.is_shape() {
            continue;
        }
        let tolerance = base + annotation.width as f32;
        let local = rotate_point(point, annotation.center(), -annotation.angle);
        let rect = annotation.bounds();
        let hit = match annotation.tool {
            Tool::Line | Tool::Arrow => {
                distance_to_segment(local, annotation.start, annotation.end) <= tolerance
            }
            Tool::Rect => near_rect_outline(local, &rect, tolerance),
            Tool::Ellipse => near_ellipse_outline(local, &rect, tolerance),
            _ => false,
        };
        if hit {
            return Some(index);
        }
    }
    None
}

fn hit_annotation_at(point: POINT, dpi: u32) -> Option<usize> {
    app::with_state(|s| hit_annotation(&s.annotations, point, dpi))
}

/// Аннотации, которые выбираются кликом внутри: текст и эффекты.
fn hit_interior_at(point: POINT) -> Option<usize> {
    app::with_state(|s| {
        s.annotations
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, annotation)| match annotation.tool {
                Tool::Text | Tool::Pixelate | Tool::Blur => {
                    if point_in_rect(point, &annotation.bounds()) {
                        Some(index)
                    } else {
                        None
                    }
                }
                _ => None,
            })
    })
}

/// Центр ручки поворота: над верхней стороной фигуры, с учётом её поворота.
fn rotate_handle_center(annotation: &Annotation, dpi: u32) -> POINT {
    let rect = annotation.bounds();
    let anchor = POINT {
        x: (rect.left + rect.right) / 2,
        y: rect.top - s(26, dpi),
    };
    rotate_point(anchor, annotation.center(), annotation.angle)
}

fn rotation_handle_at(point: POINT, dpi: u32) -> Option<usize> {
    let selected = app::with_state(|s| s.selected)?;
    app::with_state(|state| {
        let annotation = state.annotations.get(selected)?;
        // Текст и эффекты не вращаются — у них нет ручки.
        if !annotation.tool.is_shape() {
            return None;
        }
        let handle = rotate_handle_center(annotation, dpi);
        let radius = s(12, dpi).max(8);
        if (point.x - handle.x).abs() <= radius && (point.y - handle.y).abs() <= radius {
            Some(selected)
        } else {
            None
        }
    })
}

/// Попадание в «тело» фигуры — отсюда её можно перетаскивать целиком.
fn hit_move(annotation: &Annotation, point: POINT, dpi: u32) -> bool {
    let local = rotate_point(point, annotation.center(), -annotation.angle);
    let rect = annotation.bounds();
    let tolerance = s(8, dpi).max(6) as f32;

    match annotation.tool {
        Tool::Rect => {
            let pad = tolerance as i32;
            local.x >= rect.left - pad
                && local.x <= rect.right + pad
                && local.y >= rect.top - pad
                && local.y <= rect.bottom + pad
        }
        Tool::Ellipse => {
            let rx = (rect.right - rect.left) as f32 / 2.0 + tolerance;
            let ry = (rect.bottom - rect.top) as f32 / 2.0 + tolerance;
            if rx <= 0.0 || ry <= 0.0 {
                return false;
            }
            let cx = (rect.left + rect.right) as f32 / 2.0;
            let cy = (rect.top + rect.bottom) as f32 / 2.0;
            let nx = (local.x as f32 - cx) / rx;
            let ny = (local.y as f32 - cy) / ry;
            nx * nx + ny * ny <= 1.0
        }
        Tool::Line | Tool::Arrow => {
            let local_start = rotate_point(annotation.start, annotation.center(), -annotation.angle);
            let local_end = rotate_point(annotation.end, annotation.center(), -annotation.angle);
            distance_to_segment(local, local_start, local_end)
                <= tolerance + annotation.width as f32
        }
        Tool::Text => {
            let pad = tolerance as i32;
            local.x >= rect.left - pad
                && local.x <= rect.right + pad
                && local.y >= rect.top - pad
                && local.y <= rect.bottom + pad
        }
        Tool::Pixelate | Tool::Blur => {
            let pad = tolerance as i32;
            point.x >= rect.left - pad
                && point.x <= rect.right + pad
                && point.y >= rect.top - pad
                && point.y <= rect.bottom + pad
        }
        _ => false,
    }
}

fn move_hit_at(point: POINT, dpi: u32) -> Option<usize> {
    let selected = app::with_state(|s| s.selected)?;
    app::with_state(|state| {
        let annotation = state.annotations.get(selected)?;
        if hit_move(annotation, point, dpi) {
            Some(selected)
        } else {
            None
        }
    })
}

/// Габариты фигуры с учётом поворота — чтобы не утащить её за выделение.
fn rotated_bounds(annotation: &Annotation) -> RECT {
    shape_corners(annotation.start, annotation.end, annotation.angle)
        .iter()
        .fold(
            RECT {
                left: i32::MAX,
                top: i32::MAX,
                right: i32::MIN,
                bottom: i32::MIN,
            },
            |mut acc, point| {
                acc.left = acc.left.min(point.x);
                acc.top = acc.top.min(point.y);
                acc.right = acc.right.max(point.x);
                acc.bottom = acc.bottom.max(point.y);
                acc
            },
        )
}

/// Перетащить выделенную фигуру за курсором (в пределах выделения).
fn apply_move(cursor: POINT) {
    app::with_state(|s| {
        let Some(index) = s.selected else {
            return;
        };
        let Some(annotation) = s.annotations.get(index) else {
            return;
        };

        let bounds = rotated_bounds(annotation);
        let sel = app::selection_rect(&s.selection);
        let mut dx = cursor.x - s.move_last.x;
        let mut dy = cursor.y - s.move_last.y;

        if sel.right - sel.left > 4 && sel.bottom - sel.top > 4 {
            let lo_x = (sel.left - bounds.left).min(sel.right - bounds.right);
            let hi_x = (sel.left - bounds.left).max(sel.right - bounds.right);
            let lo_y = (sel.top - bounds.top).min(sel.bottom - bounds.bottom);
            let hi_y = (sel.top - bounds.top).max(sel.bottom - bounds.bottom);
            dx = dx.clamp(lo_x, hi_x.max(lo_x));
            dy = dy.clamp(lo_y, hi_y.max(lo_y));
        }

        let (start, end) = (annotation.start, annotation.end);
        if let Some(annotation) = s.annotations.get_mut(index) {
            annotation.start = POINT {
                x: start.x + dx,
                y: start.y + dy,
            };
            annotation.end = POINT {
                x: end.x + dx,
                y: end.y + dy,
            };
        }
        // Курсор может упереться в границу выделения — запоминаем фактический
        // сдвиг, чтобы фигура не «отставала» от мыши потом.
        s.move_last = POINT {
            x: s.move_last.x + dx,
            y: s.move_last.y + dy,
        };
    });
}

/// Рамка выделенной фигуры и круглая ручка поворота.
fn draw_annotation_selection(
    hdc: HDC,
    window: &RECT,
    annotation: &Annotation,
    dpi: u32,
    hot: bool,
) {
    let rect = annotation.bounds();
    let center = annotation.center();

    // Рамку слегка отодвигаем от контура, чтобы она читалась и у фигур,
    // у которых контур совпадает с рамкой (прямоугольник).
    let frame = inflate(&rect, s(4, dpi));
    let corners = rotated_corners(&frame, center, annotation.angle)
        .map(|point| screen_to_local_point(point, window));
    let outline = [
        corners[0],
        corners[1],
        corners[2],
        corners[3],
        corners[0],
    ];
    let dotted = GdiPen::dotted(ACCENT);
    unsafe {
        let old_pen = SelectObject(hdc, dotted.handle());
        let _ = Polyline(hdc, &outline);
        SelectObject(hdc, old_pen);
    }

    // Текст и эффекты не вращаются: показываем только рамку.
    if !annotation.tool.is_shape() {
        return;
    }

    // Поводок от верхней стороны рамки к ручке.
    let anchor = POINT {
        x: (rect.left + rect.right) / 2,
        y: frame.top,
    };
    let anchor = rotate_point(anchor, center, annotation.angle);
    let handle = rotate_handle_center(annotation, dpi);
    let anchor = screen_to_local_point(anchor, window);
    let handle = screen_to_local_point(handle, window);

    let lead = GdiPen::solid(s(1, dpi).max(1), ACCENT);
    unsafe {
        let old_pen = SelectObject(hdc, lead.handle());
        let _ = MoveToEx(hdc, anchor.x, anchor.y, None);
        let _ = LineTo(hdc, handle.x, handle.y);
        SelectObject(hdc, old_pen);
    }

    let radius = s(9, dpi).max(7);
    let fill = GdiBrush::solid(if hot { ACCENT_HOT } else { ACCENT });
    unsafe {
        let old_brush = SelectObject(hdc, fill.handle());
        let old_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
        let _ = Ellipse(
            hdc,
            handle.x - radius,
            handle.y - radius,
            handle.x + radius,
            handle.y + radius,
        );
        SelectObject(hdc, old_pen);
        SelectObject(hdc, old_brush);
    }

    // Белая дуга со стрелкой — значок «повернуть».
    let inner = (radius - s(3, dpi).max(2)).max(3) as f32;
    let step = std::f32::consts::FRAC_PI_2;
    let start = -0.9f32;
    let end = start + step * 2.4;
    let arc_pen = GdiPen::solid(s(1, dpi).max(1), WHITE);
    unsafe {
        let old_pen = SelectObject(hdc, arc_pen.handle());
        let _ = Arc(
            hdc,
            handle.x - inner as i32,
            handle.y - inner as i32,
            handle.x + inner as i32,
            handle.y + inner as i32,
            handle.x + (start.cos() * inner) as i32,
            handle.y + (start.sin() * inner) as i32,
            handle.x + (end.cos() * inner) as i32,
            handle.y + (end.sin() * inner) as i32,
        );
        SelectObject(hdc, old_pen);
    }

    let tip = POINT {
        x: handle.x + (end.cos() * inner) as i32,
        y: handle.y + (end.sin() * inner) as i32,
    };
    let tangent = end + std::f32::consts::FRAC_PI_2;
    let head = s(4, dpi).max(3) as f32;
    let arrow = [
        tip,
        POINT {
            x: tip.x - (tangent.cos() * head + 0.5 * head * end.cos()) as i32,
            y: tip.y - (tangent.sin() * head + 0.5 * head * end.sin()) as i32,
        },
        POINT {
            x: tip.x - (tangent.cos() * head - 0.5 * head * end.cos()) as i32,
            y: tip.y - (tangent.sin() * head - 0.5 * head * end.sin()) as i32,
        },
    ];
    let brush = GdiBrush::solid(WHITE);
    unsafe {
        let old_brush = SelectObject(hdc, brush.handle());
        let old_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
        let _ = Polygon(hdc, &arrow);
        SelectObject(hdc, old_pen);
        SelectObject(hdc, old_brush);
    }
}

/// Полупрозрачная заливка внутри контура фигуры.
fn paint_shape_fill(
    hdc: HDC,
    tool: Tool,
    from: POINT,
    to: POINT,
    angle: f32,
    color: u32,
    fill_level: i32,
) {
    let points: Vec<POINT> = match tool {
        Tool::Rect => shape_corners(from, to, angle).to_vec(),
        Tool::Ellipse => ellipse_points(from, to, angle, 96),
        _ => return,
    };
    if points.len() < 3 {
        return;
    }
    let region = unsafe { CreatePolygonRgn(&points, WINDING) };
    if region.is_invalid() {
        return;
    }
    let bounds = bounds_of(&points);
    fill_region(hdc, region, &bounds, color, fill_shape_alpha(fill_level));
}

/// Подложка под текстом — тёмное полупрозрачное стекло.
fn paint_text(
    hdc: HDC,
    from: POINT,
    to: POINT,
    text: &str,
    font_size: i32,
    color: u32,
    fill: bool,
    fill_level: i32,
    dpi: u32,
) {
    let rect = normalized(from, to);
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    if width < 2 || height < 2 || text.is_empty() {
        return;
    }

    if fill {
        let radius = s(4, dpi).max(2);
        let region = unsafe {
            CreateRoundRectRgn(
                rect.left,
                rect.top,
                rect.right + 1,
                rect.bottom + 1,
                radius * 2,
                radius * 2,
            )
        };
        if !region.is_invalid() {
            fill_region(hdc, region, &rect, PANEL_BG, fill_text_alpha(fill_level));
        }
    }

    let font = ui::make_font(font_size.max(8), 600);
    let size = ui::measure_text(hdc, text, font);
    let x = rect.left + (width - size.cx) / 2;
    let y = rect.top + (height - size.cy) / 2;
    ui::draw_text(hdc, x, y, text, font, color);
    unsafe {
        let _ = DeleteObject(font);
    }
}

/// Залить область, ограниченную регионом, цветом с прозрачностью.
fn fill_region(hdc: HDC, region: HRGN, bounds: &RECT, color: u32, alpha: u8) {
    let width = bounds.right - bounds.left;
    let height = bounds.bottom - bounds.top;
    if width < 1 || height < 1 {
        let _ = unsafe { DeleteObject(region) };
        return;
    }

    let blue = ((color >> 16) & 0xFF) as u8;
    let green = ((color >> 8) & 0xFF) as u8;
    let red = (color & 0xFF) as u8;
    let Some(source) = BitmapDc::from_bgra(hdc, 1, 1, &[blue, green, red, 255]) else {
        let _ = unsafe { DeleteObject(region) };
        return;
    };
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: alpha,
        AlphaFormat: 0,
    };

    unsafe {
        let saved = SaveDC(hdc);
        let _ = SelectClipRgn(hdc, region);
        let _ = AlphaBlend(
            hdc,
            bounds.left,
            bounds.top,
            width,
            height,
            source.handle(),
            0,
            0,
            1,
            1,
            blend,
        );
        let _ = RestoreDC(hdc, saved);
        let _ = DeleteObject(region);
    }
}

fn bounds_of(points: &[POINT]) -> RECT {
    points.iter().fold(
        RECT {
            left: i32::MAX,
            top: i32::MAX,
            right: i32::MIN,
            bottom: i32::MIN,
        },
        |mut acc, point| {
            acc.left = acc.left.min(point.x);
            acc.top = acc.top.min(point.y);
            acc.right = acc.right.max(point.x);
            acc.bottom = acc.bottom.max(point.y);
            acc
        },
    )
}

/// Нарисовать фигуру инструментом GDI (используется и в оверлее, и в снимке).
fn paint_shape(hdc: HDC, tool: Tool, from: POINT, to: POINT, angle: f32, width: i32, color: u32) {
    let pen = GdiPen::solid(width.max(1), color);
    let center = rect_center(from, to);

    unsafe {
        let old_pen = SelectObject(hdc, pen.handle());

        match tool {
            Tool::Line => {
                let a = rotate_point(from, center, angle);
                let b = rotate_point(to, center, angle);
                let _ = MoveToEx(hdc, a.x, a.y, None);
                let _ = LineTo(hdc, b.x, b.y);
            }
            Tool::Arrow => {
                draw_arrow(
                    hdc,
                    rotate_point(from, center, angle),
                    rotate_point(to, center, angle),
                    width,
                    color,
                );
            }
            Tool::Rect => {
                let corners = shape_corners(from, to, angle);
                let old_brush = SelectObject(hdc, GetStockObject(NULL_BRUSH));
                let _ = Polygon(hdc, &corners);
                SelectObject(hdc, old_brush);
            }
            Tool::Ellipse => {
                let points = ellipse_points(from, to, angle, 96);
                let _ = Polyline(hdc, &points);
            }
            _ => {}
        }

        SelectObject(hdc, old_pen);
    }
}

fn draw_arrow(hdc: HDC, from: POINT, to: POINT, width: i32, color: u32) {
    let dx = (to.x - from.x) as f32;
    let dy = (to.y - from.y) as f32;
    let length = (dx * dx + dy * dy).sqrt();
    if length < 1.0 {
        return;
    }

    unsafe {
        let _ = MoveToEx(hdc, from.x, from.y, None);
        let _ = LineTo(hdc, to.x, to.y);

        let angle = dy.atan2(dx);
        let head = ((width as f32) * 4.5).max(12.0).min(length * 0.6);
        let a1 = angle + std::f32::consts::PI - 0.45;
        let a2 = angle - std::f32::consts::PI + 0.45;

        let points = [
            to,
            POINT {
                x: to.x + (a1.cos() * head) as i32,
                y: to.y + (a1.sin() * head) as i32,
            },
            POINT {
                x: to.x + (a2.cos() * head) as i32,
                y: to.y + (a2.sin() * head) as i32,
            },
        ];

        let brush = GdiBrush::solid(color);
        let old_brush = SelectObject(hdc, brush.handle());
        let _ = Polygon(hdc, &points);
        SelectObject(hdc, old_brush);
    }
}

// ---------------------------------------------------------------------------
// Панель инструментов
// ---------------------------------------------------------------------------

fn toolbar_layout(
    hdc: HDC,
    window: &RECT,
    cursor_screen: POINT,
    selection: &Selection,
    dpi: u32,
) -> ToolbarLayout {
    let font = ui::make_font(s(12, dpi), 600);
    let label_font = ui::make_font(s(11, dpi), 600);
    let pad_x = s(9, dpi);
    let pad = s(7, dpi);
    let gap = s(5, dpi);
    let btn_h = s(34, dpi);
    let opt_h = s(26, dpi);
    let opt_btn_h = s(22, dpi);
    let row_gap = s(7, dpi);
    let icon = s(15, dpi);
    let icon_gap = s(5, dpi);
    let sep_w = s(13, dpi);

    // --- Первый ряд: инструменты и «Копировать» ---
    let mut specs: Vec<(&str, ToolbarHit)> = Tool::ALL
        .iter()
        .map(|tool| (tool.label(), ToolbarHit::Tool(*tool)))
        .collect();
    specs.push(("Копировать", ToolbarHit::Copy));

    let mut widths = Vec::with_capacity(specs.len());
    let mut tools_w = 0;
    for (label, _) in &specs {
        let width = ui::measure_text(hdc, label, font).cx + pad_x * 2 + icon + icon_gap;
        widths.push(width);
        tools_w += width;
    }
    tools_w += gap * (specs.len() as i32 - 1) + sep_w;

    // --- Второй ряд: цвет, заливка, сила эффекта ---
    let swatch = s(20, dpi);
    let swatch_gap = s(4, dpi);
    let group_gap = s(14, dpi);
    let label_gap = s(6, dpi);
    let step_w = s(22, dpi);
    let pip_w = s(12, dpi);
    let pip_gap = s(4, dpi);

    let color_label_w = ui::measure_text(hdc, "Цвет", label_font).cx;
    let strength_label_w = ui::measure_text(hdc, "Сила", label_font)
        .cx
        .max(ui::measure_text(hdc, "Фон", label_font).cx);
    let control_label = if control_target() == StyleTarget::Fill {
        "Фон"
    } else {
        "Сила"
    };
    let fill_text_w = ui::measure_text(hdc, "Заливка", label_font).cx;
    let fill_w = fill_text_w + s(26, dpi) + pad_x;

    let mut options_w = color_label_w
        + label_gap
        + PALETTE.len() as i32 * swatch
        + (PALETTE.len() as i32 - 1) * swatch_gap
        + group_gap
        + fill_w
        + group_gap
        + strength_label_w
        + label_gap
        + step_w
        + pip_gap
        + 5 * pip_w
        + 4 * pip_gap
        + pip_gap
        + step_w;

    let panel_w = tools_w.max(options_w) + pad * 2;
    let panel_h = pad * 2 + btn_h + row_gap + opt_h;
    options_w = options_w.min(panel_w - pad * 2);

    unsafe {
        let _ = DeleteObject(font);
        let _ = DeleteObject(label_font);
    }

    let monitor = monitor_local_rect(cursor_screen, window);
    let margin = s(12, dpi);
    let min_left = monitor.left + margin;
    let max_left = (monitor.right - panel_w - margin).max(min_left);
    let min_top = monitor.top + margin;
    let max_top = (monitor.bottom - panel_h - margin).max(min_top);

    let mut left;
    let mut top;

    if selection.active {
        let sel = screen_to_local(&app::selection_rect(selection), window);
        left = (sel.left + sel.right - panel_w) / 2;
        top = sel.bottom + s(10, dpi);

        // Снизу не помещается — ставим над областью, иначе внутри неё.
        if top + panel_h > monitor.bottom - margin {
            top = sel.top - panel_h - s(10, dpi);
        }
        if top < min_top {
            top = (sel.bottom - panel_h - s(8, dpi)).max(min_top);
        }
    } else {
        left = (monitor.left + monitor.right - panel_w) / 2;
        top = monitor.bottom - panel_h - s(18, dpi);
    }

    left = left.clamp(min_left, max_left);
    top = top.clamp(min_top, max_top);

    let panel = RECT {
        left,
        top,
        right: left + panel_w,
        bottom: top + panel_h,
    };

    let mut buttons = Vec::with_capacity(specs.len());
    let mut separator = None;
    let mut x = left + pad;
    for ((_, hit), width) in specs.into_iter().zip(widths) {
        if matches!(hit, ToolbarHit::Copy) {
            let sep_x = x + gap / 2;
            separator = Some(RECT {
                left: sep_x,
                top: top + pad + s(7, dpi),
                right: sep_x + s(1, dpi).max(1),
                bottom: top + pad + btn_h - s(7, dpi),
            });
            x += sep_w;
        }

        let rect = RECT {
            left: x,
            top: top + pad,
            right: x + width,
            bottom: top + pad + btn_h,
        };
        buttons.push((rect, hit));
        x += width + gap;
    }

    // --- Раскладка второго ряда по центру панели ---
    let options_top = top + pad + btn_h + row_gap;
    let opt_y = options_top + (opt_h - opt_btn_h) / 2;
    let mut x = left + (panel_w - options_w) / 2;
    let mut options = Vec::new();
    let mut labels = Vec::new();
    let label_h = s(14, dpi);
    let label_y = options_top + (opt_h - label_h) / 2;

    labels.push((
        RECT {
            left: x,
            top: label_y,
            right: x + color_label_w,
            bottom: label_y + label_h,
        },
        "Цвет",
    ));
    x += color_label_w + label_gap;

    for (index, _) in PALETTE.iter().enumerate() {
        let rect = RECT {
            left: x,
            top: opt_y,
            right: x + swatch,
            bottom: opt_y + opt_btn_h,
        };
        options.push((rect, ToolbarHit::Color(index)));
        x += swatch + swatch_gap;
    }
    x += group_gap - swatch_gap;

    let fill_rect = RECT {
        left: x,
        top: opt_y,
        right: x + fill_w,
        bottom: opt_y + opt_btn_h,
    };
    options.push((fill_rect, ToolbarHit::Fill));
    x += fill_w + group_gap;

    labels.push((
        RECT {
            left: x,
            top: label_y,
            right: x + strength_label_w,
            bottom: label_y + label_h,
        },
        control_label,
    ));
    x += strength_label_w + label_gap;

    let minus = RECT {
        left: x,
        top: opt_y,
        right: x + step_w,
        bottom: opt_y + opt_btn_h,
    };
    options.push((minus, ToolbarHit::IntensityStep(-1)));
    x += step_w + pip_gap;

    for level in 1..=5 {
        let rect = RECT {
            left: x,
            top: opt_y,
            right: x + pip_w,
            bottom: opt_y + opt_btn_h,
        };
        options.push((rect, ToolbarHit::IntensitySet(level)));
        x += pip_w + pip_gap;
    }

    let plus = RECT {
        left: x,
        top: opt_y,
        right: x + step_w,
        bottom: opt_y + opt_btn_h,
    };
    options.push((plus, ToolbarHit::IntensityStep(1)));

    let layout = ToolbarLayout {
        panel,
        buttons,
        options,
        labels,
        separator,
    };
    dump_layout(&layout);
    layout
}

/// Диагностика раскладки для автотестов (включается переменной окружения).
fn dump_layout(layout: &ToolbarLayout) {
    if std::env::var("COPYX_DUMP_LAYOUT").is_err() {
        return;
    }
    let mut text = String::new();
    let mut add = |name: String, rect: &RECT| {
        text.push_str(&format!(
            "{} {} {} {} {}\n",
            name, rect.left, rect.top, rect.right, rect.bottom
        ));
    };
    add("panel".to_string(), &layout.panel);
    for (rect, hit) in &layout.buttons {
        add(format!("{:?}", hit), rect);
    }
    for (rect, hit) in &layout.options {
        add(format!("{:?}", hit), rect);
    }
    let path = std::env::temp_dir().join("copyx_layout.txt");
    let mut text = text;
    app::with_state(|s| {
        for (index, annotation) in s.annotations.iter().enumerate() {
            text.push_str(&format!(
                "ann#{} {:?} fill={} level={} color={:06X} strength={} text={:?} rect={} {} {} {}\n",
                index,
                annotation.tool,
                annotation.fill,
                annotation.fill_level,
                annotation.color,
                annotation.strength,
                annotation.text,
                annotation.start.x,
                annotation.start.y,
                annotation.end.x,
                annotation.end.y
            ));
        }
        text.push_str(&format!(
            "state fill={} level={} color={:06X} intensity={} tool={:?} sel_active={} selected={:?}\n",
            s.fill, s.fill_level, s.color, s.intensity, s.tool, s.selection.active, s.selected
        ));
    });
    let _ = std::fs::write(path, text);
}

fn toolbar_hit(hwnd: HWND, cursor: POINT) -> Option<ToolbarHit> {
    let mut window = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut window);
        if window.right <= window.left {
            return None;
        }
    }

    // Панель существует только после выделения и не мешает тянуть новую рамку.
    let (selection, dragging_selection) =
        app::with_state(|s| (s.selection, s.pointer_down && s.draft.is_none()));
    if !selection.active || dragging_selection {
        return None;
    }

    let dpi = dpi_at(cursor);
    let local = POINT {
        x: cursor.x - window.left,
        y: cursor.y - window.top,
    };

    let screen = ScreenDc::for_screen();
    let layout = toolbar_layout(screen.handle(), &window, cursor, &selection, dpi);
    layout
        .buttons
        .iter()
        .chain(layout.options.iter())
        .find(|(rect, _)| point_in_rect(local, rect))
        .map(|(_, hit)| *hit)
}

/// Стабильный идентификатор кнопки для подсветки (не зависит от раскладки).
fn hit_id(hit: ToolbarHit) -> i32 {
    match hit {
        ToolbarHit::Tool(tool) => Tool::ALL
            .iter()
            .position(|item| *item == tool)
            .map_or(0, |index| index as i32 + 1),
        ToolbarHit::Copy => 100,
        ToolbarHit::Color(index) => 200 + index as i32,
        ToolbarHit::Fill => 300,
        ToolbarHit::IntensitySet(level) => 400 + level,
        ToolbarHit::IntensityStep(delta) => 420 + i32::from(delta > 0),
    }
}

fn hot_key(hit: Option<ToolbarHit>) -> i32 {
    hit.map(hit_id).unwrap_or(0)
}

fn draw_toolbar(hdc: HDC, layout: &ToolbarLayout, active: Tool, hot: i32, dpi: u32) {
    // Мягкая тень под парящей панелью.
    let shadow = RECT {
        left: layout.panel.left - s(4, dpi),
        top: layout.panel.top + s(3, dpi),
        right: layout.panel.right + s(4, dpi),
        bottom: layout.panel.bottom + s(10, dpi),
    };
    ui::glass_glow(hdc, &shadow, s(20, dpi), [0, 0, 0, 255], 60);

    // Сама панель: матовое стекло с бликом сверху.
    ui::frosted_round_rect(
        hdc,
        &layout.panel,
        s(16, dpi),
        PANEL_BG_BGRA,
        186,
        GLASS_BORDER,
        48,
    );

    let font = ui::make_font(s(12, dpi), 600);
    let icon_size = s(15, dpi);
    let icon_gap = s(5, dpi);

    if let Some(separator) = &layout.separator {
        let brush = GdiBrush::solid(GLASS_BORDER);
        unsafe {
            let _ = FillRect(hdc, separator, brush.handle());
        }
    }

    for (rect, hit) in layout.buttons.iter() {
        let is_copy = matches!(hit, ToolbarHit::Copy);
        let is_active = matches!(hit, ToolbarHit::Tool(tool) if *tool == active);
        let is_hot = hot == hit_id(*hit);

        // Цветное свечение вокруг активной и главной кнопки.
        if is_copy || is_active {
            let glow = inflate(rect, s(2, dpi));
            ui::glass_glow(
                hdc,
                &glow,
                s(12, dpi),
                ACCENT_BGRA,
                if is_copy { 120 } else { 80 },
            );
        }

        let (tint, alpha, border, sheen_top, fg) = if is_copy {
            (ACCENT_BGRA, 215, COPY_BORDER, 70, WHITE)
        } else if is_active {
            (ACCENT_BGRA, 120, ACCENT, 46, WHITE)
        } else if is_hot {
            ([0xFF, 0xFF, 0xFF, 255], 30, ACCENT, 30, WHITE)
        } else {
            ([0xFF, 0xFF, 0xFF, 255], 14, GLASS_BORDER, 16, TEXT_MAIN)
        };

        // Кнопки лежат на уже размытой панели — повторное размытие не нужно.
        ui::tint_round_rect(hdc, rect, s(10, dpi), tint, alpha, border, sheen_top);

        let label = match hit {
            ToolbarHit::Tool(tool) => tool.label(),
            ToolbarHit::Copy => "Копировать",
            _ => "",
        };
        let text = ui::measure_text(hdc, label, font);
        let content_w = icon_size + icon_gap + text.cx;
        let start_x = rect.left + (rect.right - rect.left - content_w) / 2;
        let mid_y = (rect.top + rect.bottom) / 2;

        let icon_rect = RECT {
            left: start_x,
            top: mid_y - icon_size / 2,
            right: start_x + icon_size,
            bottom: mid_y - icon_size / 2 + icon_size,
        };
        draw_tool_icon(hdc, &icon_rect, *hit, fg, dpi);

        let text_x = start_x + icon_size + icon_gap;
        let text_y = mid_y - text.cy / 2;
        ui::draw_text(hdc, text_x, text_y, label, font, fg);
    }

    draw_toolbar_options(hdc, layout, hot, dpi);

    unsafe {
        let _ = DeleteObject(font);
    }
}

/// Второй ряд панели: палитра, заливка и сила эффекта.
fn draw_toolbar_options(hdc: HDC, layout: &ToolbarLayout, hot: i32, dpi: u32) {
    let (active_color, fill_on, intensity, fill_level) =
        app::with_state(|s| (s.color, s.fill, s.intensity, s.fill_level));
    let control_value = match control_target() {
        StyleTarget::Effect => intensity,
        StyleTarget::Fill => fill_level,
    };
    let font = ui::make_font(s(11, dpi), 600);

    for (rect, label) in &layout.labels {
        ui::draw_text(hdc, rect.left, rect.top, label, font, TEXT_MUTED);
    }

    for (rect, hit) in &layout.options {
        let is_hot = hot == hit_id(*hit);
        match hit {
            ToolbarHit::Color(index) => {
                let color = PALETTE.get(*index).copied().unwrap_or(WHITE);
                let selected = color == active_color;
                let border = if selected {
                    WHITE
                } else if is_hot {
                    ACCENT
                } else {
                    GLASS_BORDER
                };
                ui::tint_round_rect(
                    hdc,
                    rect,
                    s(6, dpi),
                    color_bgra(color),
                    255,
                    border,
                    0,
                );
                if selected {
                    let ring = inflate(rect, s(1, dpi));
                    ui::stroke_round(hdc, &ring, s(6, dpi), WHITE);
                }
            }
            ToolbarHit::Fill => {
                let (tint, alpha, border, fg) = if fill_on {
                    (ACCENT_BGRA, 120, ACCENT, WHITE)
                } else if is_hot {
                    ([0xFF, 0xFF, 0xFF, 255], 30, ACCENT, WHITE)
                } else {
                    ([0xFF, 0xFF, 0xFF, 255], 14, GLASS_BORDER, TEXT_MAIN)
                };
                ui::tint_round_rect(hdc, rect, s(7, dpi), tint, alpha, border, 16);

                let dot = s(10, dpi).max(6);
                let icon = RECT {
                    left: rect.left + s(8, dpi),
                    top: (rect.top + rect.bottom - dot) / 2,
                    right: rect.left + s(8, dpi) + dot,
                    bottom: (rect.top + rect.bottom - dot) / 2 + dot,
                };
                let brush = GdiBrush::solid(if fill_on { WHITE } else { active_color });
                unsafe {
                    let _ = FillRect(hdc, &icon, brush.handle());
                }

                let text = ui::measure_text(hdc, "Заливка", font);
                let x = icon.right + s(5, dpi);
                let y = (rect.top + rect.bottom - text.cy) / 2;
                ui::draw_text(hdc, x, y, "Заливка", font, fg);
            }
            ToolbarHit::IntensitySet(level) => {
                let (tint, alpha, border) = if *level == control_value {
                    (ACCENT_BGRA, 170, ACCENT)
                } else if *level < control_value {
                    (ACCENT_BGRA, 80, GLASS_BORDER)
                } else if is_hot {
                    ([0xFF, 0xFF, 0xFF, 255], 26, ACCENT)
                } else {
                    ([0xFF, 0xFF, 0xFF, 255], 12, GLASS_BORDER)
                };
                ui::tint_round_rect(hdc, rect, s(4, dpi), tint, alpha, border, 0);
            }
            ToolbarHit::IntensityStep(delta) => {
                let (tint, alpha, border, fg) = if is_hot {
                    ([0xFF, 0xFF, 0xFF, 255], 30, ACCENT, WHITE)
                } else {
                    ([0xFF, 0xFF, 0xFF, 255], 14, GLASS_BORDER, TEXT_MAIN)
                };
                ui::tint_round_rect(hdc, rect, s(6, dpi), tint, alpha, border, 12);
                let label = if *delta < 0 { "−" } else { "+" };
                let text = ui::measure_text(hdc, label, font);
                let x = (rect.left + rect.right - text.cx) / 2;
                let y = (rect.top + rect.bottom - text.cy) / 2;
                ui::draw_text(hdc, x, y, label, font, fg);
            }
            _ => {}
        }
    }

    unsafe {
        let _ = DeleteObject(font);
    }
}

fn color_bgra(color: u32) -> [u8; 4] {
    // COLORREF — 0x00BBGGRR, а AlphaBlend ждёт BGRA.
    [
        ((color >> 16) & 0xFF) as u8,
        ((color >> 8) & 0xFF) as u8,
        (color & 0xFF) as u8,
        255,
    ]
}

/// Иконки инструментов рисуются простыми GDI-примитивами: без внешних
/// ресурсов, зато в цвет активной кнопки и с учётом DPI.
fn draw_tool_icon(hdc: HDC, area: &RECT, hit: ToolbarHit, color: u32, dpi: u32) {
    let x = area.left;
    let y = area.top;
    let w = area.right - area.left;
    let h = area.bottom - area.top;
    if w < 8 || h < 8 {
        return;
    }

    let t = s(2, dpi).max(1);
    let brush = GdiBrush::solid(color);

    match hit {
        ToolbarHit::Tool(Tool::Select) => {
            // Уголки рамки выделения.
            let len = (w / 2).max(t * 2);
            let fill = |left, top, right, bottom| {
                let rect = RECT {
                    left,
                    top,
                    right,
                    bottom,
                };
                unsafe {
                    let _ = FillRect(hdc, &rect, brush.handle());
                }
            };
            fill(x, y, x + len, y + t);
            fill(x, y, x + t, y + len);
            fill(x + w - len, y, x + w, y + t);
            fill(x + w - t, y, x + w, y + len);
            fill(x, y + h - t, x + len, y + h);
            fill(x, y + h - len, x + t, y + h);
            fill(x + w - len, y + h - t, x + w, y + h);
            fill(x + w - t, y + h - len, x + w, y + h);
        }
        ToolbarHit::Tool(Tool::Pixelate) => {
            // Мозаика 3×3: чередование крупных и мелких квадратов.
            let gap = s(1, dpi).max(1);
            let cell = ((w - gap * 2) / 3).max(2);
            for row in 0..3 {
                for col in 0..3 {
                    let left = x + col * (cell + gap);
                    let top = y + row * (cell + gap);
                    let shrink = if (row + col) % 2 == 0 { 0 } else { cell / 3 };
                    let rect = RECT {
                        left,
                        top,
                        right: (left + cell - shrink).min(x + w),
                        bottom: (top + cell - shrink).min(y + h),
                    };
                    unsafe {
                        let _ = FillRect(hdc, &rect, brush.handle());
                    }
                }
            }
        }
        ToolbarHit::Tool(Tool::Blur) => {
            // Концентрические окружности — «мягкий фокус».
            let pen = GdiPen::solid(1, color);
            unsafe {
                let old_pen = SelectObject(hdc, pen.handle());
                let old_brush = SelectObject(hdc, GetStockObject(NULL_BRUSH));
                let _ = Ellipse(hdc, x, y, x + w, y + h);
                let inset = s(3, dpi).max(2);
                if w - inset * 2 > 2 && h - inset * 2 > 2 {
                    let _ = Ellipse(hdc, x + inset, y + inset, x + w - inset, y + h - inset);
                }
                SelectObject(hdc, old_brush);
                SelectObject(hdc, old_pen);
            }
            let dot = s(3, dpi).max(2);
            let rect = RECT {
                left: x + (w - dot) / 2,
                top: y + (h - dot) / 2,
                right: x + (w - dot) / 2 + dot,
                bottom: y + (h - dot) / 2 + dot,
            };
            unsafe {
                let _ = FillRect(hdc, &rect, brush.handle());
            }
        }
        ToolbarHit::Tool(Tool::Arrow) => {
            let from = POINT {
                x: x + s(1, dpi),
                y: y + h - s(1, dpi),
            };
            let to = POINT {
                x: x + w - s(1, dpi),
                y: y + s(1, dpi),
            };
            paint_shape(hdc, Tool::Arrow, from, to, 0.0, t, color);
        }
        ToolbarHit::Tool(Tool::Line) => {
            let from = POINT {
                x: x + s(2, dpi),
                y: y + h - s(2, dpi),
            };
            let to = POINT {
                x: x + w - s(2, dpi),
                y: y + s(2, dpi),
            };
            paint_shape(hdc, Tool::Line, from, to, 0.0, t, color);
        }
        ToolbarHit::Tool(Tool::Rect) => {
            let rect = RECT {
                left: x,
                top: y + s(1, dpi),
                right: x + w,
                bottom: y + h - s(1, dpi),
            };
            ui::stroke_rect(hdc, &rect, t, color);
        }
        ToolbarHit::Tool(Tool::Ellipse) => {
            let pen = GdiPen::solid(t, color);
            unsafe {
                let old_pen = SelectObject(hdc, pen.handle());
                let old_brush = SelectObject(hdc, GetStockObject(NULL_BRUSH));
                let _ = Ellipse(hdc, x, y + s(1, dpi), x + w, y + h - s(1, dpi));
                SelectObject(hdc, old_brush);
                SelectObject(hdc, old_pen);
            }
        }
        ToolbarHit::Tool(Tool::Text) => {
            // Буква «T» из двух полос.
            let top = RECT {
                left: x,
                top: y,
                right: x + w,
                bottom: y + t,
            };
            let stem = RECT {
                left: x + (w - t) / 2,
                top: y,
                right: x + (w - t) / 2 + t,
                bottom: y + h,
            };
            unsafe {
                let _ = FillRect(hdc, &top, brush.handle());
                let _ = FillRect(hdc, &stem, brush.handle());
            }
        }
        ToolbarHit::Copy => {
            // Два листа: задний — контуром, передний — заливкой.
            let offset = s(3, dpi).max(2);
            let back = RECT {
                left: x + offset,
                top: y,
                right: x + w,
                bottom: y + h - offset,
            };
            ui::stroke_rect(hdc, &back, s(1, dpi).max(1), color);
            let front = RECT {
                left: x,
                top: y + offset,
                right: x + w - offset,
                bottom: y + h,
            };
            unsafe {
                let _ = FillRect(hdc, &front, brush.handle());
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Плашки и вспомогательные преобразования
// ---------------------------------------------------------------------------

fn draw_highlight(hdc: HDC, local: &RECT, dpi: u32) {
    // Белая внешняя кромка + акцентная рамка внутри, чтобы граница читалась
    // и на светлых, и на тёмных окнах.
    let outer = RECT {
        left: local.left - 1,
        top: local.top - 1,
        right: local.right + 1,
        bottom: local.bottom + 1,
    };
    ui::stroke_rect(hdc, &outer, 1, GLASS_BORDER);
    ui::stroke_rect(hdc, local, s(2, dpi).max(2), ACCENT);
    draw_corner_ticks(hdc, local, dpi);
}

/// Белые «уголки» поверх рамки — как ручки выделения.
fn draw_corner_ticks(hdc: HDC, local: &RECT, dpi: u32) {
    if local.right - local.left < s(24, dpi) || local.bottom - local.top < s(24, dpi) {
        return;
    }

    let t = s(3, dpi).max(2);
    let len = s(16, dpi).max(6);
    let brush = GdiBrush::solid(WHITE);
    let strips = [
        RECT {
            left: local.left,
            top: local.top,
            right: local.left + len,
            bottom: local.top + t,
        },
        RECT {
            left: local.left,
            top: local.top,
            right: local.left + t,
            bottom: local.top + len,
        },
        RECT {
            left: local.right - len,
            top: local.top,
            right: local.right,
            bottom: local.top + t,
        },
        RECT {
            left: local.right - t,
            top: local.top,
            right: local.right,
            bottom: local.top + len,
        },
        RECT {
            left: local.left,
            top: local.bottom - t,
            right: local.left + len,
            bottom: local.bottom,
        },
        RECT {
            left: local.left,
            top: local.bottom - len,
            right: local.left + t,
            bottom: local.bottom,
        },
        RECT {
            left: local.right - len,
            top: local.bottom - t,
            right: local.right,
            bottom: local.bottom,
        },
        RECT {
            left: local.right - t,
            top: local.bottom - len,
            right: local.right,
            bottom: local.bottom,
        },
    ];

    unsafe {
        for strip in &strips {
            let _ = FillRect(hdc, strip, brush.handle());
        }
    }
}

fn draw_size_badge(
    hdc: HDC,
    local: &RECT,
    width: i32,
    height: i32,
    client: &RECT,
    dpi: u32,
) {
    if width < 1 || height < 1 {
        return;
    }

    let text = format!("{width} × {height}");
    let size = measure_chip(hdc, &text, s(14, dpi), dpi);
    let gap = s(8, dpi);
    let margin = s(6, dpi);

    let mut x = local.left;
    let mut y = local.top - size.cy - gap;

    // Если сверху не помещается — внутрь области.
    if y < margin {
        y = local.top + gap;
        x = local.left + gap;
    }

    let max_x = (client.right - size.cx - margin).max(margin);
    let max_y = (client.bottom - size.cy - margin).max(margin);
    x = x.clamp(margin, max_x);
    y = y.clamp(margin, max_y);

    draw_chip(
        hdc,
        x,
        y,
        &text,
        s(14, dpi),
        TEXT_MAIN,
        SIZE_BG_BGRA,
        190,
        ACCENT,
        dpi,
    );
}

fn draw_coord_badge(
    hdc: HDC,
    cursor: POINT,
    text: &str,
    client: &RECT,
    dpi: u32,
) {
    if cursor.x < client.left
        || cursor.x >= client.right
        || cursor.y < client.top
        || cursor.y >= client.bottom
    {
        return;
    }

    let size = measure_chip(hdc, text, s(12, dpi), dpi);
    let gap = s(18, dpi);
    let margin = s(4, dpi);

    let mut x = cursor.x + gap;
    let mut y = cursor.y + gap;
    if x + size.cx > client.right - margin {
        x = cursor.x - size.cx - s(10, dpi);
    }
    if y + size.cy > client.bottom - margin {
        y = cursor.y - size.cy - s(10, dpi);
    }
    x = x.clamp(margin, (client.right - size.cx - margin).max(margin));
    y = y.clamp(margin, (client.bottom - size.cy - margin).max(margin));

    draw_chip(
        hdc,
        x,
        y,
        text,
        s(12, dpi),
        TEXT_MUTED,
        PANEL_BG_BGRA,
        200,
        PANEL_BORDER,
        dpi,
    );
}

fn dpi_at(point: POINT) -> u32 {
    ui::dpi_for_point(point).max(96)
}

fn point_in_rect(point: POINT, rect: &RECT) -> bool {
    point.x >= rect.left
        && point.x < rect.right
        && point.y >= rect.top
        && point.y < rect.bottom
}

/// Увеличить прямоугольник на `by` со всех сторон.
fn inflate(rect: &RECT, by: i32) -> RECT {
    RECT {
        left: rect.left - by,
        top: rect.top - by,
        right: rect.right + by,
        bottom: rect.bottom + by,
    }
}

fn clamp_point(point: POINT, rect: &RECT) -> POINT {
    POINT {
        x: point.x.clamp(rect.left, rect.right),
        y: point.y.clamp(rect.top, rect.bottom),
    }
}

/// Нормализованный прямоугольник (left ≤ right, top ≤ bottom).
fn normalized(a: POINT, b: POINT) -> RECT {
    RECT {
        left: a.x.min(b.x),
        top: a.y.min(b.y),
        right: a.x.max(b.x),
        bottom: a.y.max(b.y),
    }
}

fn offset_point(point: POINT, target: &RECT) -> POINT {
    POINT {
        x: point.x - target.left,
        y: point.y - target.top,
    }
}

fn screen_to_local(screen: &RECT, window: &RECT) -> RECT {
    RECT {
        left: screen.left - window.left,
        top: screen.top - window.top,
        right: screen.right - window.left,
        bottom: screen.bottom - window.top,
    }
}

fn screen_to_local_point(point: POINT, window: &RECT) -> POINT {
    POINT {
        x: point.x - window.left,
        y: point.y - window.top,
    }
}

fn monitor_local_rect(cursor_screen: POINT, window: &RECT) -> RECT {
    unsafe {
        let monitor = MonitorFromPoint(cursor_screen, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            return screen_to_local(&info.rcMonitor, window);
        }
    }
    RECT {
        left: 0,
        top: 0,
        right: window.right - window.left,
        bottom: window.bottom - window.top,
    }
}

// ---------------------------------------------------------------------------
// Мелкая типографика / GDI-помощники
// ---------------------------------------------------------------------------

/// Scale a 96-DPI metric to the current DPI.
fn s(value: i32, dpi: u32) -> i32 {
    ui::scale(value, dpi)
}

/// Размер «стеклянной» плашки с текстом (вместе с внутренними отступами).
fn measure_chip(hdc: HDC, text: &str, font_size: i32, dpi: u32) -> SIZE {
    let font = ui::make_font(font_size, 600);
    let text_size = ui::measure_text(hdc, text, font);
    unsafe {
        let _ = DeleteObject(font);
    }
    SIZE {
        cx: text_size.cx + s(20, dpi),
        cy: text_size.cy + s(10, dpi),
    }
}

/// Полупрозрачная стеклянная плашка с текстом.
#[allow(clippy::too_many_arguments)]
fn draw_chip(
    hdc: HDC,
    left: i32,
    top: i32,
    text: &str,
    font_size: i32,
    fg: u32,
    tint: [u8; 4],
    alpha: u8,
    border: u32,
    dpi: u32,
) -> SIZE {
    let pad_x = s(10, dpi);
    let pad_y = s(5, dpi);

    let font = ui::make_font(font_size, 600);
    let text_size = ui::measure_text(hdc, text, font);

    let rect = RECT {
        left,
        top,
        right: left + text_size.cx + pad_x * 2,
        bottom: top + text_size.cy + pad_y * 2,
    };

    ui::frosted_round_rect(hdc, &rect, s(9, dpi), tint, alpha, border, 30);
    ui::draw_text(hdc, left + pad_x, top + pad_y, text, font, fg);

    unsafe {
        let _ = DeleteObject(font);
    }

    SIZE {
        cx: rect.right - rect.left,
        cy: rect.bottom - rect.top,
    }
}

fn show_error(message: &str) {
    let text: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    unsafe {
        MessageBoxW(
            None,
            windows::core::PCWSTR(text.as_ptr()),
            w!("CopyX"),
            MB_OK | MB_ICONERROR | MB_TOPMOST,
        );
    }
}
