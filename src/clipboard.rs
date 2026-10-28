//! Clipboard operations using raw Win32 API.
//!
//! The capture is published as both `CF_DIB` (classic apps: Paint, Word,
//! Explorer) and a registered `PNG` format (Telegram, Slack, browsers), so
//! pasting works everywhere without an extra image dependency.

use std::io::Cursor;
use std::time::Duration;

use image::{ImageBuffer, Rgba};
use windows::core::w;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Graphics::Gdi::{BITMAPINFOHEADER, BI_RGB};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GLOBAL_ALLOC_FLAGS};
use windows::Win32::System::Ole::CF_DIB;

use crate::capture::CapturedImage;

/// GMEM_MOVEABLE — required for clipboard data.
const GMEM_MOVEABLE: GLOBAL_ALLOC_FLAGS = GLOBAL_ALLOC_FLAGS(0x0002);

/// Copy a captured image to the system clipboard as DIB + PNG.
pub fn copy_to_clipboard(img: &CapturedImage) -> Result<(), String> {
    let dib = build_dib(img)?;
    let png = build_png(img);

    open_clipboard_retry()?;

    let result = (|| -> Result<(), String> {
        unsafe {
            let _ = EmptyClipboard();
            set_clipboard_bytes(CF_DIB.0 as u32, &dib)?;

            if let Some(png) = &png {
                let format = RegisterClipboardFormatW(w!("PNG"));
                if format != 0 {
                    let _ = set_clipboard_bytes(format, png);
                }
            }
        }
        Ok(())
    })();

    unsafe {
        let _ = CloseClipboard();
    }
    result
}

/// Чужое приложение может держать буфер обмена открытым пару миллисекунд.
fn open_clipboard_retry() -> Result<(), String> {
    for _ in 0..12 {
        if unsafe { OpenClipboard(None) }.is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    Err("Буфер обмена занят другим приложением".into())
}

/// `CF_DIB` payload: 32bpp bottom-up BGRA + `BITMAPINFOHEADER`.
///
/// Bottom-up (positive `biHeight`) is what Windows and most programs expect
/// when converting the DIB to a bitmap, so pasting works in old apps too.
fn build_dib(img: &CapturedImage) -> Result<Vec<u8>, String> {
    let w = img.width as usize;
    let h = img.height as usize;
    if w == 0 || h == 0 {
        return Err("Пустое изображение".into());
    }

    let header_size = std::mem::size_of::<BITMAPINFOHEADER>();
    let row_bytes = w.checked_mul(4).ok_or("Слишком большое изображение")?;
    let data_size = row_bytes
        .checked_mul(h)
        .ok_or("Слишком большое изображение")?;
    if img.pixels.len() < data_size {
        return Err("Повреждённые данные снимка".into());
    }

    let bih = BITMAPINFOHEADER {
        biSize: header_size as u32,
        biWidth: w as i32,
        biHeight: h as i32, // positive = bottom-up
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0 as u32,
        biSizeImage: data_size as u32,
        ..Default::default()
    };

    let mut buffer = Vec::with_capacity(header_size + data_size);
    buffer.extend_from_slice(unsafe {
        std::slice::from_raw_parts(&bih as *const _ as *const u8, header_size)
    });

    // RGBA сверху вниз -> BGRA снизу вверх.
    for y in (0..h).rev() {
        let row = &img.pixels[y * row_bytes..(y + 1) * row_bytes];
        for px in row.chunks_exact(4) {
            buffer.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
    }
    Ok(buffer)
}

fn build_png(img: &CapturedImage) -> Option<Vec<u8>> {
    let buffer: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_raw(img.width, img.height, img.pixels.clone())?;
    let mut bytes = Vec::new();
    // Быстрое сжатие: снимок должен попасть в буфер обмена мгновенно.
    let encoder = image::codecs::png::PngEncoder::new_with_quality(
        Cursor::new(&mut bytes),
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::NoFilter,
    );
    buffer.write_with_encoder(encoder).ok()?;
    Some(bytes)
}

unsafe fn set_clipboard_bytes(format: u32, bytes: &[u8]) -> Result<(), String> {
    let hmem = GlobalAlloc(GMEM_MOVEABLE, bytes.len())
        .map_err(|e| format!("GlobalAlloc failed: {e}"))?;

    let ptr = GlobalLock(hmem);
    if ptr.is_null() {
        return Err("GlobalLock failed".into());
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len());
    let _ = GlobalUnlock(hmem);

    SetClipboardData(format, HANDLE(hmem.0 as isize))
        .map(|_| ())
        .map_err(|e| format!("SetClipboardData failed: {e}"))
}
