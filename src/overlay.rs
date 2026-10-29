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
    BOOL, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::Graphics::Gdi::{
    BitBlt, DeleteObject, Ellipse, FillRect, GetMonitorInfoW, GetStockObject, IntersectClipRect,
    InvalidateRect, LineTo, MonitorFromPoint, MoveToEx, Polygon, Rectangle, RestoreDC, SaveDC,
    SelectObject, UpdateWindow, HDC, MONITORINFO, MONITOR_DEFAULTTONEAREST, NULL_BRUSH, SRCCOPY,
};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;

// --- Палитра (COLORREF = 0x00BBGGRR) ---
const ACCENT: u32 = 0x00FFB86B; // RGB(107, 184, 255) — мягкий голубой
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
const ANNOTATION: u32 = 0x004D5AFF; // RGB(255, 90, 77) — тёплый красный

/// Подсказка, пока область ещё не выделена.
const HINT_SELECT: &str =
    "Зажмите ЛКМ и выделите область · короткий клик — окно · Esc/ПКМ — отмена";

/// Подсказка редактора: панель инструментов уже доступна.
const HINT_EDIT: &str =
    "1–7 — инструменты · Enter/Ctrl+C — копировать · Ctrl+Z — шаг назад · Esc/ПКМ — закрыть";

/// Кнопки панели инструментов.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ToolbarHit {
    Tool(Tool),
    Copy,
}

struct ToolbarLayout {
    panel: RECT,
    buttons: Vec<(RECT, ToolbarHit)>,
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

        app::with_state(|s| s.overlay_hwnd = hwnd);
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
                // Esc — отмена всего снимка.
                0x1B => hide(),
                // Enter — скопировать текущее выделение (или окно под курсором).
                0x0D => {
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
                // Ctrl+Z — отменить последнюю аннотацию.
                0x5A if ctrl => undo(hwnd),
                // 1–7 — выбор инструмента (Ctrl+1…7 тоже работает).
                0x31..=0x37 => {
                    let tool = Tool::ALL[(key - 0x31) as usize];
                    set_tool(hwnd, tool);
                }
                _ => {}
            }
            LRESULT(0)
        }

        WM_RBUTTONUP => {
            hide();
            LRESULT(0)
        }

        WM_SETCURSOR => {
            let mut cursor = POINT::default();
            let _ = GetCursorPos(&mut cursor);
            let arrow = toolbar_hit(hwnd, cursor).is_some();
            let shape = if arrow { IDC_ARROW } else { IDC_CROSS };
            if let Ok(handle) = LoadCursorW(None, shape) {
                SetCursor(handle);
            }
            LRESULT(1)
        }

        WM_LBUTTONDOWN => {
            let mut cursor = POINT::default();
            let _ = GetCursorPos(&mut cursor);

            // Клик по панели инструментов не начинает выделение.
            if let Some(hit) = toolbar_hit(hwnd, cursor) {
                match hit {
                    ToolbarHit::Tool(tool) => set_tool(hwnd, tool),
                    ToolbarHit::Copy => {
                        if let Some(target) = current_target() {
                            finish_capture(target);
                        }
                    }
                }
                return LRESULT(0);
            }

            let (selection, tool) = app::with_state(|s| (s.selection, s.tool));
            let sel_rect = app::selection_rect(&selection);
            let inside_selection = selection.active
                && sel_rect.right - sel_rect.left > 4
                && sel_rect.bottom - sel_rect.top > 4
                && point_in_rect(cursor, &sel_rect);

            if inside_selection && tool != Tool::Select {
                // Рисуем аннотацию внутри уже выбранной области.
                let start = clamp_point(cursor, &sel_rect);
                app::with_state(|s| {
                    s.pointer_down = true;
                    s.capture_state = CaptureState::Selecting;
                    s.draft = Some(Draft::new(start));
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
                s.capture_state = CaptureState::OverlayShown;
            });
            let _ = InvalidateRect(hwnd, None, false);
            LRESULT(0)
        }

        WM_MOUSEMOVE => {
            let mut cursor = POINT::default();
            let _ = GetCursorPos(&mut cursor);
            let dpi = dpi_at(cursor);

            let (pointer_down, has_draft, selection) =
                app::with_state(|s| (s.pointer_down, s.draft.is_some(), s.selection));

            if pointer_down {
                if has_draft {
                    // Тянем аннотацию внутри выделения.
                    let sel_rect = app::selection_rect(&selection);
                    let tool = app::with_state(|s| s.tool);
                    let (preview_request, moved) = app::with_state(|s| {
                        let end = clamp_point(cursor, &sel_rect);
                        let mut request = None;
                        let mut moved = false;

                        if let Some(draft) = s.draft.as_mut() {
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
                                    request = Some((tool, rect));
                                }
                            }
                        }

                        (request, moved)
                    });

                    if let Some((tool, rect)) = preview_request {
                        let preview = make_effect(tool, &rect, dpi);
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
                    draft_ref.end = clamp_point(cursor, &sel_rect);
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
                        if let Some(effect) = make_effect(tool, &rect, dpi) {
                            app::with_state(|state| {
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
                                    width: 0,
                                    effect: Some(effect),
                                });
                            });
                        }
                    } else if big_enough && tool.is_shape() {
                        let stroke = s(3, dpi).max(2);
                        app::with_state(|state| {
                            state.annotations.push(Annotation {
                                tool,
                                start: draft.start,
                                end: draft.end,
                                width: stroke,
                                effect: None,
                            });
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
        s.capture_state = CaptureState::Selecting;
    });
    let _ = unsafe { InvalidateRect(hwnd, None, false) };
}

fn undo(hwnd: HWND) {
    let changed = app::with_state(|s| {
        if s.draft.is_some() || s.pointer_down {
            return false;
        }
        s.annotations.pop().is_some()
    });
    if changed {
        let _ = unsafe { InvalidateRect(hwnd, None, false) };
    }
}

fn set_tool(hwnd: HWND, tool: Tool) {
    let changed = app::with_state(|s| {
        // До выделения области панель инструментов недоступна.
        if s.pointer_down || !s.selection.active {
            return false;
        }
        let changed = s.tool != tool;
        s.tool = tool;
        s.hot = 0;
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

        if annotation.tool.is_shape() {
            paint_shape(
                canvas.handle(),
                annotation.tool,
                offset_point(annotation.start, target),
                offset_point(annotation.end, target),
                annotation.width,
                ANNOTATION,
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

fn make_effect(tool: Tool, rect: &RECT, dpi: u32) -> Option<BitmapDc> {
    snapshot::with_current(|snap| match tool {
        Tool::Pixelate => snap.pixelate_bitmap(rect, s(10, dpi).clamp(6, 24)),
        Tool::Blur => snap.blur_bitmap(rect, s(8, dpi).clamp(4, 16)),
        _ => None,
    })
    .flatten()
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
    let (selection, hover_rect, pointer_down, has_draft) = app::with_state(|s| {
        (s.selection, s.hover_rect, s.pointer_down, s.draft.is_some())
    });

    // Пока пользователь тянет новую рамку, панель не показываем: она бы
    // прыгала под курсором. После отпускания — показываем.
    let dragging_selection = pointer_down && !has_draft;
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

    // Координаты курсора — только во время протяжки, чтобы не гонять
    // перерисовку на каждое движение мыши.
    if pointer_down {
        let coord_text = format!("{}, {}", cursor_screen.x, cursor_screen.y);
        draw_coord_badge(hdc, cursor, &coord_text, client, dpi);
    }

    // Подсказка сверху по центру монитора, на котором находится курсор.
    let hint = if editing { HINT_EDIT } else { HINT_SELECT };
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

        if annotation.tool.is_shape() {
            paint_shape(
                hdc,
                annotation.tool,
                screen_to_local_point(annotation.start, window),
                screen_to_local_point(annotation.end, window),
                annotation.width,
                ANNOTATION,
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
        paint_shape(
            hdc,
            tool,
            screen_to_local_point(draft.start, window),
            screen_to_local_point(draft.end, window),
            s(3, dpi).max(2),
            ANNOTATION,
        );
    }
}

/// Нарисовать фигуру инструментом GDI (используется и в оверлее, и в снимке).
fn paint_shape(hdc: HDC, tool: Tool, from: POINT, to: POINT, width: i32, color: u32) {
    let pen = GdiPen::solid(width.max(1), color);
    unsafe {
        let old_pen = SelectObject(hdc, pen.handle());

        match tool {
            Tool::Line => {
                let _ = MoveToEx(hdc, from.x, from.y, None);
                let _ = LineTo(hdc, to.x, to.y);
            }
            Tool::Arrow => {
                draw_arrow(hdc, from, to, width, color);
            }
            Tool::Rect => {
                let old_brush = SelectObject(hdc, GetStockObject(NULL_BRUSH));
                let _ = Rectangle(
                    hdc,
                    from.x.min(to.x),
                    from.y.min(to.y),
                    from.x.max(to.x),
                    from.y.max(to.y),
                );
                SelectObject(hdc, old_brush);
            }
            Tool::Ellipse => {
                let old_brush = SelectObject(hdc, GetStockObject(NULL_BRUSH));
                let _ = Ellipse(
                    hdc,
                    from.x.min(to.x),
                    from.y.min(to.y),
                    from.x.max(to.x),
                    from.y.max(to.y),
                );
                SelectObject(hdc, old_brush);
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
    let pad_x = s(9, dpi);
    let pad = s(7, dpi);
    let gap = s(5, dpi);
    let btn_h = s(34, dpi);
    let icon = s(15, dpi);
    let icon_gap = s(5, dpi);
    let sep_w = s(13, dpi);

    let mut specs: Vec<(&str, ToolbarHit)> = Tool::ALL
        .iter()
        .map(|tool| (tool.label(), ToolbarHit::Tool(*tool)))
        .collect();
    specs.push(("Копировать", ToolbarHit::Copy));

    let mut widths = Vec::with_capacity(specs.len());
    let mut content_w = 0;
    for (label, _) in &specs {
        let width = ui::measure_text(hdc, label, font).cx + pad_x * 2 + icon + icon_gap;
        widths.push(width);
        content_w += width;
    }
    content_w += gap * (specs.len() as i32 - 1) + sep_w;

    let panel_w = content_w + pad * 2;
    let panel_h = btn_h + pad * 2;

    unsafe {
        let _ = DeleteObject(font);
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

    ToolbarLayout {
        panel,
        buttons,
        separator,
    }
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
        .find(|(rect, _)| point_in_rect(local, rect))
        .map(|(_, hit)| *hit)
}

fn hot_key(hit: Option<ToolbarHit>) -> i32 {
    match hit {
        None => 0,
        Some(ToolbarHit::Tool(tool)) => {
            Tool::ALL
                .iter()
                .position(|item| *item == tool)
                .map_or(0, |index| index as i32 + 1)
        }
        Some(ToolbarHit::Copy) => Tool::ALL.len() as i32 + 1,
    }
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

    for (index, (rect, hit)) in layout.buttons.iter().enumerate() {
        let is_copy = matches!(hit, ToolbarHit::Copy);
        let is_active = matches!(hit, ToolbarHit::Tool(tool) if *tool == active);
        let is_hot = hot == index as i32 + 1;

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

    unsafe {
        let _ = DeleteObject(font);
    }
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
            paint_shape(hdc, Tool::Arrow, from, to, t, color);
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
            paint_shape(hdc, Tool::Line, from, to, t, color);
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
