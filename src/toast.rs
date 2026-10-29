//! Glass toast notification shown after a capture.
//!
//! Layout: a frosted glass card in the bottom-right corner of the monitor under
//! the cursor. The desktop behind it is captured, blurred and tinted, which is
//! what gives the card its translucent look. It contains a thumbnail of the
//! capture, the "copied to clipboard" message and the Save / Hide buttons.
//! The card hides itself after 3 seconds unless the pointer is over it.

use std::sync::{Arc, Mutex};

use crate::app;
use crate::capture::CapturedImage;
use crate::render::{BitmapDc, GdiBrush, PaintDc, ScreenDc};
use crate::save;
use crate::ui;

use windows::core::w;
use windows::Win32::Foundation::{
    COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    AlphaBlend, BitBlt, DeleteObject, Ellipse, FillRect, GetStockObject, InvalidateRect, LineTo,
    MoveToEx, SelectObject, SetBkMode, SetBrushOrgEx, SetStretchBltMode, SetWindowRgn, StretchBlt,
    UpdateWindow, AC_SRC_OVER, BLENDFUNCTION, HDC, HGDIOBJ, HALFTONE, NULL_PEN, SRCCOPY,
    TRANSPARENT,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::*;

/// Цвет и плотность стеклянной тонировки (BGRA).
const GLASS_TINT: [u8; 4] = [0x30, 0x20, 0x18, 255];
const GLASS_ALPHA: u8 = 178;

// --- Glass palette (COLORREF = 0x00BBGGRR) ---
const ACCENT: u32 = 0x00FFB86B; // RGB(107, 184, 255)
const ACCENT_HOVER: u32 = 0x00FFC98A; // RGB(138, 201, 255)
const PANEL_BG: u32 = 0x00221612; // fallback if acrylic is unavailable
const BORDER: u32 = 0x00AA876E; // RGB(110, 135, 170)
const BORDER_SUBTLE: u32 = 0x005A463C; // RGB(60, 70, 90)
const SHINE: u32 = 0x00C8A88C; // RGB(140, 168, 200) — верхний блик
const TEXT_MAIN: u32 = 0x00FFFFFF;
const TEXT_MUTED: u32 = 0x00E1CDBE; // RGB(190, 205, 225)
const TEXT_DARK: u32 = 0x00221610; // RGB(16, 22, 34)

// Стеклянные тонировки кнопок (BGRA).
const ACCENT_GLASS: [u8; 4] = [0xBE, 0x56, 0x20, 255]; // RGB(32, 86, 190)
const ACCENT_GLASS_HOT: [u8; 4] = [0xE6, 0x78, 0x2E, 255]; // RGB(46, 120, 230)
const HIDE_GLASS: [u8; 4] = [0x30, 0x20, 0x18, 255]; // RGB(24, 32, 48)
const HIDE_GLASS_HOT: [u8; 4] = [0x4A, 0x33, 0x25, 255]; // RGB(37, 51, 74)

const AUTO_HIDE_MS: u32 = 3000;
const TIMER_HIDE: usize = 1;

/// windows-rs 0.52 does not export this one, so define it here.
const WM_MOUSELEAVE: u32 = 0x02A3;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Button {
    Save,
    Hide,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Уведомление о готовом снимке.
    Capture { copied: bool },
    /// Короткое служебное сообщение с иконкой приложения.
    Status,
}

struct Toast {
    image: Option<Arc<CapturedImage>>,
    thumb: Option<BitmapDc>,
    backdrop: Option<BitmapDc>,
    thumb_rect: RECT,
    save_rect: RECT,
    hide_rect: RECT,
    /// Заголовок карточки.
    title: String,
    /// Y-координата первой строки подписи.
    lines_top: i32,
    lines: Vec<String>,
    dpi: u32,
    kind: Kind,
    hot: Option<Button>,
    hovered: bool,
}

static TOAST: Mutex<Option<Toast>> = Mutex::new(None);

/// Кадр тоста тоже собирается в отдельном bitmap и выводится одним BitBlt:
/// иначе при наведении курсора карточка «дрожит» из-за пошаговой отрисовки.
struct FrameBuffer {
    width: i32,
    height: i32,
    bitmap: BitmapDc,
}

static TOAST_FRAME: Mutex<Option<FrameBuffer>> = Mutex::new(None);

/// Show a capture as a glass toast in the bottom-right corner.
pub fn show(image: CapturedImage, clipboard_ok: bool) {
    hidden();

    let mut cursor = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut cursor);
    }
    let dpi = ui::dpi_for_point(cursor).max(96);
    let sc = |v: i32| ui::scale(v, dpi);

    let width = sc(430);
    let pad = sc(16);
    let title_h = sc(20);
    let btn_h = sc(30);
    let title_y = pad;
    let row2_y = title_y + title_h + sc(6);
    let image_y = row2_y + btn_h + sc(12);

    let max_thumb_w = width - pad * 2;
    let max_thumb_h = sc(240);
    let (thumb_w, thumb_h) = fit(
        image.width as i32,
        image.height as i32,
        max_thumb_w,
        max_thumb_h,
    );

    let height = image_y + thumb_h + pad;

    let screen = ScreenDc::for_screen();
    let font_btn = ui::make_font(sc(13), 600);
    let save_text = "Сохранить";
    let hide_text = "Скрыть";
    let save_w = ui::measure_text(screen.handle(), save_text, font_btn).cx + sc(32);
    let hide_w = ui::measure_text(screen.handle(), hide_text, font_btn).cx + sc(24);
    unsafe {
        let _ = DeleteObject(font_btn);
    }

    let save_rect = RECT {
        left: width - pad - save_w,
        top: row2_y,
        right: width - pad,
        bottom: row2_y + btn_h,
    };
    let hide_rect = RECT {
        left: save_rect.left - sc(8) - hide_w,
        top: row2_y,
        right: save_rect.left - sc(8),
        bottom: row2_y + btn_h,
    };

    let thumb = create_thumbnail(&image, thumb_w, thumb_h);
    let thumb_x = (width - thumb_w) / 2;
    let thumb_rect = RECT {
        left: thumb_x,
        top: image_y,
        right: thumb_x + thumb_w,
        bottom: image_y + thumb_h,
    };

    let lines_top = save_rect.top + (btn_h - sc(15)) / 2;
    let subtitle = format!("PNG · {} × {}", image.width, image.height);
    let title = if clipboard_ok {
        "Скопировано в буфер обмена"
    } else {
        "Снимок готов"
    };
    let toast = Toast {
        image: Some(Arc::new(image)),
        thumb,
        backdrop: None,
        thumb_rect,
        save_rect,
        hide_rect,
        title: title.to_string(),
        lines_top,
        lines: vec![subtitle],
        dpi,
        kind: Kind::Capture {
            copied: clipboard_ok,
        },
        hot: None,
        hovered: false,
    };

    present(width, height, toast, false);
}

/// Короткое уведомление «программа запущена» в правом нижнем углу.
pub fn show_started() {
    show_status(
        "CopyX запущен",
        &[
            "PrtSc — снимок области",
            "Ctrl+PrtSc — окно · Shift+PrtSc — экран",
            "После выделения — пиксели, размытие и фигуры",
        ],
    );
}

/// Служебное уведомление с иконкой приложения и произвольным заголовком.
pub fn show_status(title: &str, lines: &[&str]) {
    hidden();

    let mut cursor = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut cursor);
    }
    let dpi = ui::dpi_for_point(cursor).max(96);
    let sc = |v: i32| ui::scale(v, dpi);

    let width = sc(430);
    let pad = sc(16);
    let title_h = sc(20);
    let line_h = sc(15);
    let line_gap = sc(2);
    let btn_h = sc(26);

    let lines: Vec<String> = lines.iter().map(|line| (*line).to_string()).collect();

    let lines_top = pad + title_h + sc(4);
    let height = lines_top + (line_h + line_gap) * lines.len() as i32 - line_gap + pad;

    let screen = ScreenDc::for_screen();
    let font_btn = ui::make_font(sc(13), 600);
    let hide_w = ui::measure_text(screen.handle(), "Скрыть", font_btn).cx + sc(24);
    unsafe {
        let _ = DeleteObject(font_btn);
    }

    let hide_rect = RECT {
        left: width - pad - hide_w,
        top: pad - sc(2),
        right: width - pad,
        bottom: pad - sc(2) + btn_h,
    };

    let toast = Toast {
        image: None,
        thumb: None,
        backdrop: None,
        thumb_rect: RECT::default(),
        save_rect: RECT::default(),
        hide_rect,
        title: title.to_string(),
        lines_top,
        lines,
        dpi,
        kind: Kind::Status,
        hot: None,
        hovered: false,
    };

    present(width, height, toast, true);
}

/// Показать карточку в правом нижнем углу рабочего стола.
fn present(width: i32, height: i32, mut toast: Toast, on_primary: bool) {
    let dpi = toast.dpi;
    let sc = |v: i32| ui::scale(v, dpi);

    let mut cursor = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut cursor);
    }

    let area = if on_primary {
        ui::primary_work_area()
    } else {
        ui::monitor_work_area(cursor)
    };
    let work = area.unwrap_or_else(|| unsafe {
        RECT {
            left: 0,
            top: 0,
            right: GetSystemMetrics(SM_CXVIRTUALSCREEN),
            bottom: GetSystemMetrics(SM_CYVIRTUALSCREEN),
        }
    });
    let margin = sc(16);
    let x = (work.right - width - margin)
        .clamp(work.left + margin, (work.right - width - margin).max(work.left + margin));
    let y = (work.bottom - height - margin)
        .clamp(work.top + margin, (work.bottom - height - margin).max(work.top + margin));

    unsafe {
        let instance = app::with_state(|s| s.instance);
        let class_name = w!("CopyXToast");
        let class = WNDCLASSW {
            hInstance: instance,
            lpfnWndProc: Some(toast_proc),
            lpszClassName: class_name,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        let _ = RegisterClassW(&class);

        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            class_name,
            w!(""),
            WS_POPUP,
            x,
            y,
            width,
            height,
            None,
            HMENU::default(),
            instance,
            None,
        );
        if hwnd.0 == 0 {
            return;
        }

        let radius = sc(16);
        let region = windows::Win32::Graphics::Gdi::CreateRoundRectRgn(
            0,
            0,
            width + 1,
            height + 1,
            radius * 2,
            radius * 2,
        );
        let _ = SetWindowRgn(hwnd, region, true);

        // Фростед-стекло: снимок экрана под тостом, размытый и затемнённый.
        let screen = ScreenDc::for_screen();
        toast.backdrop = create_backdrop(screen.handle(), x, y, width, height);

        if let Ok(mut slot) = TOAST.lock() {
            *slot = Some(toast);
        }

        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = SetTimer(hwnd, TIMER_HIDE, AUTO_HIDE_MS, None);
        let _ = InvalidateRect(hwnd, None, false);
        let _ = UpdateWindow(hwnd);
    }
}

/// Close the toast if it exists.
pub fn hide() {
    hidden();
}

fn hidden() {
    unsafe {
        let hwnd = FindWindowW(w!("CopyXToast"), w!(""));
        if hwnd.0 != 0 {
            let _ = DestroyWindow(hwnd);
        }
    }
}

fn fit(width: i32, height: i32, max_w: i32, max_h: i32) -> (i32, i32) {
    if width < 1 || height < 1 {
        return (max_w / 2, max_w / 4);
    }
    let scale = (max_w as f64 / width as f64)
        .min(max_h as f64 / height as f64)
        .min(1.0);
    (
        ((width as f64 * scale).round() as i32).max(1),
        ((height as f64 * scale).round() as i32).max(1),
    )
}

fn create_thumbnail(image: &CapturedImage, width: i32, height: i32) -> Option<BitmapDc> {
    if width < 1 || height < 1 {
        return None;
    }
    let screen = ScreenDc::for_screen();

    // Convert RGBA -> BGRA for GDI.
    let mut bgra = image.pixels.clone();
    for chunk in bgra.chunks_exact_mut(4) {
        chunk.swap(0, 2);
    }
    let source = BitmapDc::from_bgra(
        screen.handle(),
        image.width as i32,
        image.height as i32,
        &bgra,
    )?;
    let thumb = BitmapDc::compatible(screen.handle(), width, height)?;

    unsafe {
        SetStretchBltMode(thumb.handle(), HALFTONE);
        let _ = SetBrushOrgEx(thumb.handle(), 0, 0, None);
        let _ = StretchBlt(
            thumb.handle(),
            0,
            0,
            width,
            height,
            source.handle(),
            0,
            0,
            image.width as i32,
            image.height as i32,
            SRCCOPY,
        );
    }
    Some(thumb)
}

/// Снимок экрана под тостом: мягкое гауссово размытие и тёмно-синяя
/// стеклянная тонировка.
fn create_backdrop(
    screen: HDC,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> Option<BitmapDc> {
    if width < 4 || height < 4 {
        return None;
    }

    let mut shot = crate::capture::grab_region_bgra(screen, x, y, width, height).ok()?;

    // Радиус ~ 1/16 меньшей стороны: стекло, а не «мыльные квадраты».
    let radius = (width.min(height) / 16).clamp(6, 24) as usize;
    ui::blur_bgra(&mut shot.pixels, width as usize, height as usize, radius);

    let frosted = BitmapDc::from_bgra(screen, width, height, &shot.pixels)?;

    // Тёмно-синяя стеклянная тонировка поверх размытого экрана.
    let tint = BitmapDc::from_bgra(screen, 1, 1, &GLASS_TINT)?;
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: GLASS_ALPHA,
        AlphaFormat: 0,
    };
    unsafe {
        let _ = AlphaBlend(
            frosted.handle(),
            0,
            0,
            width,
            height,
            tint.handle(),
            0,
            0,
            1,
            1,
            blend,
        );
    }

    Some(frosted)
}

unsafe extern "system" fn toast_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint_toast(hwnd);
            LRESULT(0)
        }

        WM_ERASEBKGND => LRESULT(1),

        WM_SETCURSOR => {
            let hit = (lparam.0 as u32) & 0xFFFF;
            if hit == HTCLIENT as u32 {
                let hand = TOAST
                    .lock()
                    .ok()
                    .and_then(|g| g.as_ref().and_then(|t| t.hot))
                    .is_some();
                unsafe {
                    let _ = SetCursor(
                        LoadCursorW(None, if hand { IDC_HAND } else { IDC_ARROW })
                            .unwrap_or_default(),
                    );
                }
                LRESULT(1)
            } else {
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }

        WM_MOUSEMOVE => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;

            let (was_hovered, hot, changed) = {
                let mut guard = match TOAST.lock() {
                    Ok(g) => g,
                    Err(_) => return LRESULT(0),
                };
                let Some(toast) = guard.as_mut() else {
                    return LRESULT(0);
                };
                let was_hovered = toast.hovered;
                toast.hovered = true;
                if !was_hovered {
                    let _ = KillTimer(hwnd, TIMER_HIDE);
                }
                let hot = if toast.image.is_some() && inside(&toast.save_rect, x, y) {
                    Some(Button::Save)
                } else if inside(&toast.hide_rect, x, y) {
                    Some(Button::Hide)
                } else {
                    None
                };
                let changed = toast.hot != hot;
                if changed {
                    toast.hot = hot;
                    let _ = InvalidateRect(hwnd, None, false);
                }
                (was_hovered, hot, changed)
            };

            // Курсор меняем только в момент перехода, иначе он дёргается.
            if changed {
                unsafe {
                    let _ = SetCursor(
                        LoadCursorW(None, if hot.is_some() { IDC_HAND } else { IDC_ARROW })
                            .unwrap_or_default(),
                    );
                }
            }

            if !was_hovered {
                let mut track = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut track);
            }

            LRESULT(0)
        }

        WM_MOUSELEAVE => {
            if let Ok(mut guard) = TOAST.lock() {
                if let Some(toast) = guard.as_mut() {
                    toast.hovered = false;
                    toast.hot = None;
                    let _ = InvalidateRect(hwnd, None, false);
                }
            }
            let _ = SetTimer(hwnd, TIMER_HIDE, AUTO_HIDE_MS, None);
            LRESULT(0)
        }

        WM_LBUTTONUP => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;

            let action = {
                let guard = match TOAST.lock() {
                    Ok(g) => g,
                    Err(_) => return LRESULT(0),
                };
                let Some(toast) = guard.as_ref() else {
                    return LRESULT(0);
                };
                if toast.image.is_some() && inside(&toast.save_rect, x, y) {
                    Some(Button::Save)
                } else if inside(&toast.hide_rect, x, y) {
                    Some(Button::Hide)
                } else {
                    None
                }
            };

            match action {
                Some(Button::Save) => {
                    let image = TOAST
                        .lock()
                        .ok()
                        .and_then(|g| g.as_ref().and_then(|t| t.image.clone()));
                    if let Some(image) = image {
                        // Пока открыт диалог, тост не должен закрыться по таймеру.
                        let _ = KillTimer(hwnd, TIMER_HIDE);
                        match save::save_with_dialog(image.as_ref()) {
                            Ok(Some(_)) => {
                                let _ = DestroyWindow(hwnd);
                            }
                            Ok(None) => {
                                let _ = SetTimer(hwnd, TIMER_HIDE, AUTO_HIDE_MS, None);
                                let _ = InvalidateRect(hwnd, None, false);
                            }
                            Err(e) => {
                                show_error(&format!("Не удалось сохранить файл:\n{e}"));
                                let _ = SetTimer(hwnd, TIMER_HIDE, AUTO_HIDE_MS, None);
                                let _ = InvalidateRect(hwnd, None, false);
                            }
                        }
                    }
                }
                Some(Button::Hide) => {
                    let _ = DestroyWindow(hwnd);
                }
                None => {}
            }
            LRESULT(0)
        }

        WM_TIMER if wparam.0 == TIMER_HIDE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }

        WM_DESTROY => {
            let _ = KillTimer(hwnd, TIMER_HIDE);
            if let Ok(mut slot) = TOAST.lock() {
                *slot = None;
            }
            if let Ok(mut frame) = TOAST_FRAME.lock() {
                *frame = None;
            }
            LRESULT(0)
        }

        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn paint_toast(hwnd: HWND) {
    let dc = PaintDc::begin(hwnd);
    let hdc = dc.handle();

    let mut client = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut client);
    }
    let width = client.right - client.left;
    let height = client.bottom - client.top;
    if width < 1 || height < 1 {
        return;
    }

    let screen = ScreenDc::for_screen();
    let mut frame = match TOAST_FRAME.lock() {
        Ok(guard) => guard,
        Err(_) => {
            paint_toast_scene(hdc, &client);
            return;
        }
    };

    let stale = frame
        .as_ref()
        .map_or(true, |buffer| buffer.width != width || buffer.height != height);
    if stale {
        *frame = BitmapDc::compatible(screen.handle(), width, height).map(|bitmap| FrameBuffer {
            width,
            height,
            bitmap,
        });
    }

    match frame.as_ref() {
        Some(buffer) => {
            paint_toast_scene(buffer.bitmap.handle(), &client);
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
        None => paint_toast_scene(hdc, &client),
    }
}

fn paint_toast_scene(hdc: HDC, client: &RECT) {
    let guard = match TOAST.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let Some(toast) = guard.as_ref() else {
        return;
    };

    let dpi = toast.dpi;
    let sc = |v: i32| ui::scale(v, dpi);

    // Фростед-стекло: размытый и затемнённый снимок экрана под тостом.
    if let Some(backdrop) = &toast.backdrop {
        unsafe {
            let _ = BitBlt(
                hdc,
                0,
                0,
                client.right,
                client.bottom,
                backdrop.handle(),
                0,
                0,
                SRCCOPY,
            );
        }
    } else {
        let brush = GdiBrush::solid(PANEL_BG);
        unsafe {
            let _ = FillRect(hdc, client, brush.handle());
        }
    }

    // Мягкий блик сверху — «стеклянная» поверхность карточки.
    ui::sheen(hdc, client, 44, 0);

    let radius = sc(16);
    let border_rect = RECT {
        left: 0,
        top: 0,
        right: client.right - 1,
        bottom: client.bottom - 1,
    };
    ui::stroke_round(hdc, &border_rect, radius, BORDER);

    // Верхний блик: тонкая светлая кромка, как отражение на стекле.
    let shine = RECT {
        left: radius / 2,
        top: 1,
        right: client.right - radius / 2,
        bottom: 2,
    };
    let shine_brush = GdiBrush::solid(SHINE);
    unsafe {
        let _ = FillRect(hdc, &shine, shine_brush.handle());
    }

    let pad = sc(16);
    let icon_size = sc(18);
    let icon_rect = RECT {
        left: pad,
        top: pad + sc(1),
        right: pad + icon_size,
        bottom: pad + sc(1) + icon_size,
    };
    let is_status = toast.kind == Kind::Status;
    if is_status {
        draw_app_icon(hdc, &icon_rect, dpi);
    } else {
        draw_check_icon(hdc, &icon_rect, dpi);
    }

    let title = toast.title.as_str();
    let title_font = ui::make_font(sc(14), 600);
    ui::draw_text(
        hdc,
        icon_rect.right + sc(8),
        pad,
        title,
        title_font,
        TEXT_MAIN,
    );
    unsafe {
        let _ = DeleteObject(title_font);
    }

    let sub_font = ui::make_font(sc(12), 400);
    let mut line_y = toast.lines_top;
    for line in &toast.lines {
        ui::draw_text(hdc, pad, line_y, line, sub_font, TEXT_MUTED);
        line_y += sc(15) + sc(2);
    }
    unsafe {
        let _ = DeleteObject(sub_font);
    }

    draw_button(
        hdc,
        &toast.hide_rect,
        "Скрыть",
        false,
        toast.hot == Some(Button::Hide),
        dpi,
    );
    if toast.image.is_some() {
        draw_button(
            hdc,
            &toast.save_rect,
            "Сохранить",
            true,
            toast.hot == Some(Button::Save),
            dpi,
        );
    }

    if let Some(thumb) = &toast.thumb {
        unsafe {
            let _ = BitBlt(
                hdc,
                toast.thumb_rect.left,
                toast.thumb_rect.top,
                toast.thumb_rect.right - toast.thumb_rect.left,
                toast.thumb_rect.bottom - toast.thumb_rect.top,
                thumb.handle(),
                0,
                0,
                SRCCOPY,
            );
        }
        ui::stroke_rect(hdc, &toast.thumb_rect, 1, BORDER_SUBTLE);
    }

    // Subtle divider between the action row and the preview.
    if toast.image.is_some() {
        let divider_y = toast.thumb_rect.top - sc(6);
        let divider = RECT {
            left: pad,
            top: divider_y,
            right: client.right - pad,
            bottom: divider_y + 1,
        };
        let line = GdiBrush::solid(BORDER_SUBTLE);
        unsafe {
            let _ = FillRect(hdc, &divider, line.handle());
        }
    }
}

fn draw_button(hdc: HDC, rect: &RECT, text: &str, primary: bool, hot: bool, dpi: u32) {
    let radius = ui::scale(10, dpi);

    // Кнопка — та же стеклянная плёнка, что и карточка, только с акцентом.
    let (tint, alpha, border) = if primary {
        let tint = if hot { ACCENT_GLASS_HOT } else { ACCENT_GLASS };
        (tint, if hot { 205 } else { 180 }, ACCENT_HOVER)
    } else {
        let tint = if hot { HIDE_GLASS_HOT } else { HIDE_GLASS };
        (tint, if hot { 170 } else { 135 }, BORDER)
    };
    // Фон под тостом уже размыт — кнопке достаточно тонировки, без
    // повторного размытия на каждую перерисовку.
    ui::tint_round_rect(hdc, rect, radius, tint, alpha, border, 52);

    let font = ui::make_font(ui::scale(13, dpi), 600);
    let size = ui::measure_text(hdc, text, font);
    let x = rect.left + ((rect.right - rect.left) - size.cx) / 2;
    let y = rect.top + ((rect.bottom - rect.top) - size.cy) / 2;
    let color = TEXT_MAIN;
    ui::draw_text(hdc, x, y, text, font, color);
    unsafe {
        let _ = DeleteObject(font);
    }
}

fn draw_app_icon(hdc: HDC, rect: &RECT, _dpi: u32) {
    let size = rect.right - rect.left;
    if size < 1 {
        return;
    }
    let icon = crate::tray::app_icon_for(size);
    unsafe {
        let _ = DrawIconEx(hdc, rect.left, rect.top, icon, size, size, 0, None, DI_NORMAL);
    }
}

fn draw_check_icon(hdc: HDC, rect: &RECT, dpi: u32) {
    let brush = GdiBrush::solid(ACCENT);
    unsafe {
        let old_brush = SelectObject(hdc, brush.handle());
        let old_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
        let _ = Ellipse(
            hdc,
            rect.left,
            rect.top,
            rect.right,
            rect.bottom,
        );
        SelectObject(hdc, old_pen);
        SelectObject(hdc, old_brush);
    }

    let pen = unsafe {
        windows::Win32::Graphics::Gdi::CreatePen(
            windows::Win32::Graphics::Gdi::PS_SOLID,
            ui::scale(2, dpi).max(1),
            COLORREF(TEXT_DARK),
        )
    };
    unsafe {
        let old = SelectObject(hdc, HGDIOBJ(pen.0));
        SetBkMode(hdc, TRANSPARENT);
        let w = rect.right - rect.left;
        let h = rect.bottom - rect.top;
        let _ = MoveToEx(
            hdc,
            rect.left + w * 28 / 100,
            rect.top + h * 52 / 100,
            None,
        );
        let _ = LineTo(
            hdc,
            rect.left + w * 44 / 100,
            rect.top + h * 68 / 100,
        );
        let _ = LineTo(
            hdc,
            rect.left + w * 74 / 100,
            rect.top + h * 34 / 100,
        );
        SelectObject(hdc, old);
        let _ = DeleteObject(pen);
    }
}

fn inside(rect: &RECT, x: i32, y: i32) -> bool {
    x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom
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
