//! Fullscreen overlay window for region/window selection.
//!
//! Visual model (ShareX-like):
//! * the screen is captured **before** the overlay appears;
//! * the frozen frame is painted back dimmed;
//! * the hovered window / dragged region is painted at full brightness;
//! * hints and coordinate/size badges are drawn on top as frosted glass.
//!
//! This keeps the screen readable instead of blurring it with acrylic.

use crate::app::{self, Selection};
use crate::clipboard;
use crate::render::*;
use crate::snapshot;
use crate::toast;
use crate::ui;
use copyx_core::CaptureState;
use std::mem::size_of;

use windows::core::w;
use windows::Win32::Foundation::{
    BOOL, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::Graphics::Gdi::{
    DeleteObject, FillRect, GetMonitorInfoW, InvalidateRect, MonitorFromPoint, UpdateWindow, HDC,
    MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;

// --- Палитра (COLORREF = 0x00BBGGRR) ---
const ACCENT: u32 = 0x00FFB86B; // RGB(107, 184, 255) — мягкий голубой
const PANEL_BG: u32 = 0x00221612; // тёмное "стекло" RGB(18, 22, 34)
const PANEL_BG_BGRA: [u8; 4] = [0x12, 0x16, 0x22, 255]; // тот же цвет в BGRA
const SIZE_BG_BGRA: [u8; 4] = [0x6E, 0x34, 0x18, 255]; // тёмно-синее стекло для размера
const PANEL_BORDER: u32 = 0x00785A46; // RGB(70, 90, 120)
const TEXT_MAIN: u32 = 0x00FFFFFF;
const TEXT_MUTED: u32 = 0x00D8C8B4; // RGB(180, 200, 216)
const WHITE: u32 = 0x00FFFFFF;

const HINT_TEXT: &str =
    "ЛКМ — область · клик — окно · Ctrl+A — весь экран · Esc/ПКМ — отмена";

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
        s.overlay_hwnd
    });
    if hwnd.0 != 0 {
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
    snapshot::clear();
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
            match wparam.0 as u32 {
                // Esc — отмена.
                0x1B => hide(),
                // Enter — захватить текущее выделение / окно под курсором.
                0x0D => {
                    if let Some(target) = current_target() {
                        finish_capture(target);
                    }
                }
                // Ctrl+A — весь экран.
                0x41 if ctrl_down() => finish_capture(virtual_screen()),
                _ => {}
            }
            LRESULT(0)
        }

        WM_RBUTTONUP => {
            hide();
            LRESULT(0)
        }

        WM_LBUTTONDOWN => {
            let mut cursor = POINT::default();
            let _ = GetCursorPos(&mut cursor);

            // Запоминаем окно под курсором: если пользователь просто кликнет,
            // захватим именно его.
            let (candidate, bounds) = window_at_point(cursor, hwnd);

            app::with_state(|s| {
                s.pointer_down = true;
                s.selection.active = false;
                s.selection.start = cursor;
                s.selection.current = cursor;
                s.hover_window = candidate;
                s.hover_rect = bounds;
                s.capture_state = CaptureState::OverlayShown;
            });
            let _ = InvalidateRect(hwnd, None, false);
            LRESULT(0)
        }

        WM_MOUSEMOVE => {
            let mut cursor = POINT::default();
            let _ = GetCursorPos(&mut cursor);

            let pointer_down = app::with_state(|s| s.pointer_down);

            if pointer_down {
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
            } else {
                let (candidate, bounds) = window_at_point(cursor, hwnd);
                let changed = app::with_state(|s| {
                    let changed = s.hover_window != candidate || !same_rect(&s.hover_rect, &bounds);
                    s.hover_window = candidate;
                    s.hover_rect = bounds;
                    changed
                });
                if changed {
                    let _ = InvalidateRect(hwnd, None, false);
                }
            }

            LRESULT(0)
        }

        WM_LBUTTONUP => {
            let mut cursor = POINT::default();
            let _ = GetCursorPos(&mut cursor);

            let (was_selecting, sel_rect, hover_rect) = app::with_state(|s| {
                s.pointer_down = false;
                s.selection.current = cursor;
                let selecting = s.selection.active;
                let sr = app::selection_rect(&s.selection);
                (selecting, sr, s.hover_rect)
            });

            let target = if was_selecting
                && (sel_rect.right - sel_rect.left) > 4
                && (sel_rect.bottom - sel_rect.top) > 4
            {
                Some(sel_rect)
            } else if hover_rect.right - hover_rect.left > 2
                && hover_rect.bottom - hover_rect.top > 2
            {
                Some(hover_rect)
            } else {
                None
            };

            let Some(target) = target else {
                hide();
                return LRESULT(0);
            };

            finish_capture(target);

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

/// Вырезать область из замороженного кадра, скопировать в буфер и показать тост.
fn finish_capture(target: RECT) {
    // Режем ровно то, что видел пользователь: без повторного захвата экрана
    // и без мигания оверлея.
    let cropped = snapshot::with_current(|snap| snap.crop(&target));

    hide();

    match cropped {
        Some(Ok(img)) => {
            let copied = clipboard::copy_to_clipboard(&img).is_ok();
            toast::show(img, copied);
        }
        Some(Err(e)) => show_error(&format!("Ошибка захвата:\n{e}")),
        None => show_error("Снимок экрана потерян, попробуйте ещё раз."),
    }
}

// ---------------------------------------------------------------------------
// Отрисовка оверлея
// ---------------------------------------------------------------------------

fn paint_overlay(hwnd: HWND) {
    let dc = PaintDc::begin(hwnd);
    let hdc = dc.handle();

    let mut window_rect = RECT::default();
    let mut client = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut window_rect);
        let _ = GetClientRect(hwnd, &mut client);
    }

    let (selection, hover_rect, is_selecting, pointer_down) = app::with_state(|s| {
        (
            s.selection,
            s.hover_rect,
            s.selection.active,
            s.pointer_down,
        )
    });

    let has_hover = hover_rect.right - hover_rect.left > 2
        && hover_rect.bottom - hover_rect.top > 2;

    let mut cursor_screen = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut cursor_screen);
    }
    let dpi = ui::dpi_for_point(cursor_screen).max(96);
    let cursor = POINT {
        x: cursor_screen.x - window_rect.left,
        y: cursor_screen.y - window_rect.top,
    };

    let painted = snapshot::with_current(|snap| {
        snap.paint_background(hdc);

        if is_selecting {
            let sel = app::selection_rect(&selection);
            snap.paint_bright(hdc, &sel);
        } else if has_hover {
            snap.paint_bright(hdc, &hover_rect);
        }
    });

    if painted.is_none() {
        // Аварийный фон, если снимок недоступен.
        let brush = GdiBrush::solid(PANEL_BG);
        unsafe {
            let _ = FillRect(hdc, &client, brush.handle());
        }
    }

    // Перекрестие не рисуем: координаты и размер уже показываются плашками.

    if is_selecting {
        let screen_rect = app::selection_rect(&selection);
        let local = screen_to_local(&screen_rect, &window_rect);
        draw_highlight(hdc, &local, dpi);
        draw_size_badge(
            hdc,
            &local,
            screen_rect.right - screen_rect.left,
            screen_rect.bottom - screen_rect.top,
            &client,
            dpi,
        );
    } else if has_hover && !pointer_down {
        let local = screen_to_local(&hover_rect, &window_rect);
        draw_highlight(hdc, &local, dpi);
        draw_size_badge(
            hdc,
            &local,
            hover_rect.right - hover_rect.left,
            hover_rect.bottom - hover_rect.top,
            &client,
            dpi,
        );
    }

    // Координаты курсора рядом с ним.
    let coord_text = format!("{}, {}", cursor_screen.x, cursor_screen.y);
    draw_coord_badge(hdc, cursor, &coord_text, &client, dpi);

    // Подсказка сверху по центру монитора, на котором находится курсор.
    let monitor = monitor_local_rect(cursor_screen, &window_rect);
    let hint_size = measure_chip(hdc, HINT_TEXT, s(14, dpi), dpi);
    let hint_x = (monitor.left + monitor.right - hint_size.cx) / 2;
    let hint_y = monitor.top + s(14, dpi);
    draw_chip(
        hdc,
        hint_x,
        hint_y,
        HINT_TEXT,
        s(14, dpi),
        TEXT_MAIN,
        PANEL_BG_BGRA,
        200,
        PANEL_BORDER,
        dpi,
    );
}

fn draw_highlight(hdc: windows::Win32::Graphics::Gdi::HDC, local: &RECT, dpi: u32) {
    // Белая внешняя кромка + акцентная рамка внутри, чтобы граница читалась
    // и на светлых, и на тёмных окнах.
    let outer = RECT {
        left: local.left - 1,
        top: local.top - 1,
        right: local.right + 1,
        bottom: local.bottom + 1,
    };
    ui::stroke_rect(hdc, &outer, 1, WHITE);
    ui::stroke_rect(hdc, local, s(2, dpi).max(2), ACCENT);
}

fn draw_size_badge(
    hdc: windows::Win32::Graphics::Gdi::HDC,
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
    hdc: windows::Win32::Graphics::Gdi::HDC,
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


fn screen_to_local(screen: &RECT, window: &RECT) -> RECT {
    RECT {
        left: screen.left - window.left,
        top: screen.top - window.top,
        right: screen.right - window.left,
        bottom: screen.bottom - window.top,
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
