//! Shared GDI drawing helpers for the CopyX UI.
//!
//! Everything is drawn with plain GDI so the app stays dependency-free.
//! "Glass" panels are made by alpha-blending a rounded, tinted rectangle over
//! the already rendered background (frozen screen or acrylic).

use crate::render::{BitmapDc, GdiBrush, GdiPen};
use windows::Win32::Foundation::{COLORREF, POINT, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    AlphaBlend, BitBlt, CreateFontW, CreateRoundRectRgn, DeleteObject, GetMonitorInfoW,
    GetStockObject, GetTextExtentPoint32W, MonitorFromPoint, RestoreDC, RoundRect, SaveDC,
    SelectClipRgn, SelectObject, SetBkMode, SetTextColor, TextOutW, AC_SRC_OVER, BLENDFUNCTION,
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH, HDC, HFONT,
    MONITORINFO, MONITOR_DEFAULTTONEAREST, NULL_BRUSH, OUT_DEFAULT_PRECIS, SRCCOPY, TRANSPARENT,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForSystem, MDT_EFFECTIVE_DPI};

/// Scale a 96-DPI metric to the given DPI.
pub fn scale(value: i32, dpi: u32) -> i32 {
    (value as f64 * dpi as f64 / 96.0).round() as i32
}

/// Effective DPI of the monitor under `point`.
pub fn dpi_for_point(point: POINT) -> u32 {
    unsafe {
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
        let mut dpi_x = 96u32;
        let mut dpi_y = 96u32;
        if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y).is_ok() {
            dpi_x.max(96)
        } else {
            GetDpiForSystem().max(96)
        }
    }
}

/// Work area (taskbar excluded) of the monitor under `point`.
pub fn monitor_work_area(point: POINT) -> Option<RECT> {
    unsafe {
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
        work_area_of(monitor)
    }
}

/// Work area of the primary monitor.
pub fn primary_work_area() -> Option<RECT> {
    unsafe {
        let monitor = MonitorFromPoint(
            POINT { x: 0, y: 0 },
            windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTOPRIMARY,
        );
        work_area_of(monitor)
    }
}

unsafe fn work_area_of(monitor: windows::Win32::Graphics::Gdi::HMONITOR) -> Option<RECT> {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if GetMonitorInfoW(monitor, &mut info).as_bool() {
        Some(info.rcWork)
    } else {
        None
    }
}

pub fn make_font(size: i32, weight: i32) -> HFONT {
    unsafe {
        CreateFontW(
            size,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET.0 as u32,
            OUT_DEFAULT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            CLEARTYPE_QUALITY.0 as u32,
            DEFAULT_PITCH.0 as u32,
            windows::core::w!("Segoe UI"),
        )
    }
}

/// Measure a string with the given font.
pub fn measure_text(hdc: HDC, text: &str, font: HFONT) -> SIZE {
    let utf: Vec<u16> = text.encode_utf16().collect();
    unsafe {
        let old = SelectObject(hdc, font);
        let mut size = SIZE::default();
        let _ = GetTextExtentPoint32W(hdc, &utf, &mut size);
        SelectObject(hdc, old);
        size
    }
}

/// Draw text at (x, y) with transparent background. Returns its pixel size.
pub fn draw_text(hdc: HDC, x: i32, y: i32, text: &str, font: HFONT, color: u32) -> SIZE {
    let utf: Vec<u16> = text.encode_utf16().collect();
    unsafe {
        let old = SelectObject(hdc, font);
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, COLORREF(color));
        let mut size = SIZE::default();
        let _ = GetTextExtentPoint32W(hdc, &utf, &mut size);
        let _ = TextOutW(hdc, x, y, &utf);
        SelectObject(hdc, old);
        size
    }
}

/// 1px rounded outline.
pub fn stroke_round(hdc: HDC, rect: &RECT, radius: i32, color: u32) {
    let pen = GdiPen::solid(1, color);
    unsafe {
        let old_pen = SelectObject(hdc, pen.handle());
        let old_brush = SelectObject(hdc, GetStockObject(NULL_BRUSH));
        let _ = RoundRect(
            hdc,
            rect.left,
            rect.top,
            rect.right,
            rect.bottom,
            radius * 2,
            radius * 2,
        );
        SelectObject(hdc, old_brush);
        SelectObject(hdc, old_pen);
    }
}

/// Настоящее матовое стекло: размывает уже нарисованный под панелью фон,
/// тонирует его, добавляет верхний блик и 1px рамку со скруглением.
#[allow(clippy::too_many_arguments)]
pub fn frosted_round_rect(
    hdc: HDC,
    rect: &RECT,
    radius: i32,
    tint: [u8; 4],
    alpha: u8,
    border: u32,
    sheen_top: u8,
) {
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    if width < 4 || height < 4 {
        return;
    }

    let blurred = sample_blurred(hdc, rect);
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: 0,
    };

    unsafe {
        let saved = SaveDC(hdc);
        let region = CreateRoundRectRgn(
            rect.left,
            rect.top,
            rect.right + 1,
            rect.bottom + 1,
            radius * 2,
            radius * 2,
        );
        let _ = SelectClipRgn(hdc, region);

        if let Some(blurred) = &blurred {
            let _ = AlphaBlend(
                hdc,
                rect.left,
                rect.top,
                width,
                height,
                blurred.handle(),
                0,
                0,
                width,
                height,
                blend,
            );
        }

        if let Some(source) = BitmapDc::from_bgra(hdc, 1, 1, &tint) {
            let tint_blend = BLENDFUNCTION {
                SourceConstantAlpha: alpha,
                ..blend
            };
            let _ = AlphaBlend(
                hdc,
                rect.left,
                rect.top,
                width,
                height,
                source.handle(),
                0,
                0,
                1,
                1,
                tint_blend,
            );
        }

        sheen(hdc, rect, sheen_top, 0);
        let pen = GdiPen::solid(1, border);
        let old_pen = SelectObject(hdc, pen.handle());
        let old_brush = SelectObject(hdc, GetStockObject(NULL_BRUSH));
        let _ = RoundRect(
            hdc,
            rect.left,
            rect.top,
            rect.right,
            rect.bottom,
            radius * 2,
            radius * 2,
        );
        SelectObject(hdc, old_brush);
        SelectObject(hdc, old_pen);

        let _ = RestoreDC(hdc, saved);
        let _ = DeleteObject(region);
    }
}

/// Снять область с уже нарисованного фона и вернуть её размытой.
fn sample_blurred(hdc: HDC, rect: &RECT) -> Option<BitmapDc> {
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;

    let mem = BitmapDc::compatible(hdc, width, height)?;
    unsafe {
        BitBlt(
            mem.handle(),
            0,
            0,
            width,
            height,
            hdc,
            rect.left,
            rect.top,
            SRCCOPY,
        )
        .ok()?;
    }

    let mut pixels = mem.read_bgra(width, height)?;
    let radius = (width.min(height) / 5).clamp(2, 10) as usize;
    blur_bgra(&mut pixels, width as usize, height as usize, radius);
    BitmapDc::from_bgra(hdc, width, height, &pixels)
}

/// Вертикальный блик: белый сверху, прозрачный книзу.
pub fn sheen(hdc: HDC, rect: &RECT, top_alpha: u8, bottom_alpha: u8) {
    const STEPS: i32 = 48;

    let mut pixels = Vec::with_capacity((STEPS * 4) as usize);
    for i in 0..STEPS {
        let t = i as f32 / (STEPS - 1) as f32;
        let alpha =
            (top_alpha as f32 + (bottom_alpha as f32 - top_alpha as f32) * t).clamp(0.0, 255.0);
        let a = alpha as u8;
        // Premultiplied white для AlphaBlend с AC_SRC_ALPHA.
        pixels.extend_from_slice(&[a, a, a, a]);
    }

    let Some(gradient) = BitmapDc::from_bgra(hdc, 1, STEPS, &pixels) else {
        return;
    };
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: 1, // AC_SRC_ALPHA
    };
    unsafe {
        let _ = AlphaBlend(
            hdc,
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            gradient.handle(),
            0,
            0,
            1,
            STEPS,
            blend,
        );
    }
}

/// Гауссово размытие BGRA-пикселей: три прохода box-фильтра подряд.
pub fn blur_bgra(pixels: &mut [u8], width: usize, height: usize, radius: usize) {
    if width == 0 || height == 0 || radius == 0 || pixels.len() < width * height * 4 {
        return;
    }

    let mut scratch = vec![0u8; width * height * 4];
    for _ in 0..3 {
        blur_horizontal(pixels, &mut scratch, width, height, radius);
        blur_vertical(&scratch, pixels, width, height, radius);
    }
}

/// Горизонтальный box-фильтр со скользящей суммой и повтором крайних пикселей.
fn blur_horizontal(src: &[u8], dst: &mut [u8], width: usize, height: usize, radius: usize) {
    let count = (2 * radius + 1) as u32;
    let last = width as isize - 1;

    for y in 0..height {
        let row = y * width * 4;
        let mut acc = [0u32; 4];

        for k in 0..(2 * radius + 1) {
            let x = (k as isize - radius as isize).clamp(0, last) as usize;
            let o = row + x * 4;
            for c in 0..4 {
                acc[c] += src[o + c] as u32;
            }
        }

        for x in 0..width {
            let o = row + x * 4;
            for c in 0..4 {
                dst[o + c] = (acc[c] / count) as u8;
            }

            let add = ((x + radius + 1) as isize).clamp(0, last) as usize;
            let sub = (x as isize - radius as isize).clamp(0, last) as usize;
            let ao = row + add * 4;
            let so = row + sub * 4;
            for c in 0..4 {
                acc[c] = acc[c] + src[ao + c] as u32 - src[so + c] as u32;
            }
        }
    }
}

/// Вертикальный box-фильтр.
fn blur_vertical(src: &[u8], dst: &mut [u8], width: usize, height: usize, radius: usize) {
    let stride = width * 4;
    let count = (2 * radius + 1) as u32;
    let last = height as isize - 1;

    for x in 0..width {
        let col = x * 4;
        let mut acc = [0u32; 4];

        for k in 0..(2 * radius + 1) {
            let y = (k as isize - radius as isize).clamp(0, last) as usize;
            let o = y * stride + col;
            for c in 0..4 {
                acc[c] += src[o + c] as u32;
            }
        }

        for y in 0..height {
            let o = y * stride + col;
            for c in 0..4 {
                dst[o + c] = (acc[c] / count) as u8;
            }

            let add = ((y + radius + 1) as isize).clamp(0, last) as usize;
            let sub = (y as isize - radius as isize).clamp(0, last) as usize;
            let ao = add * stride + col;
            let so = sub * stride + col;
            for c in 0..4 {
                acc[c] = acc[c] + src[ao + c] as u32 - src[so + c] as u32;
            }
        }
    }
}

/// Opaque rectangle border made of four filled strips.
pub fn stroke_rect(hdc: HDC, rect: &RECT, thickness: i32, color: u32) {
    if rect.right - rect.left < 1 || rect.bottom - rect.top < 1 {
        return;
    }
    let t = thickness.max(1);
    let brush = GdiBrush::solid(color);
    let strips = [
        RECT {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.top + t,
        },
        RECT {
            left: rect.left,
            top: rect.bottom - t,
            right: rect.right,
            bottom: rect.bottom,
        },
        RECT {
            left: rect.left,
            top: rect.top + t,
            right: rect.left + t,
            bottom: rect.bottom - t,
        },
        RECT {
            left: rect.right - t,
            top: rect.top + t,
            right: rect.right,
            bottom: rect.bottom - t,
        },
    ];
    unsafe {
        for strip in &strips {
            let _ = windows::Win32::Graphics::Gdi::FillRect(hdc, strip, brush.handle());
        }
    }
}
