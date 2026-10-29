//! Screen capture using Win32 API directly (BitBlt).

use crate::render::{GdiBitmap, MemoryDc, ScreenDc};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::{
    BitBlt, GetDIBits, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HDC, SRCCOPY,
};

/// RGBA image used by the clipboard and PNG writers.
pub struct CapturedImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Grab a region of the screen and return it as RGBA pixels.
pub fn capture_rect(rect: &RECT) -> Result<CapturedImage, String> {
    let mut image = grab_rect_bgra(rect)?;
    rgba_from_bgra(&mut image.pixels);
    Ok(image)
}

/// Grab a region of the screen without channel conversion (BGRA, top-down).
pub fn grab_rect_bgra(rect: &RECT) -> Result<CapturedImage, String> {
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    if width < 1 || height < 1 {
        return Err("Region too small".into());
    }

    let screen_dc = ScreenDc::for_screen();
    grab_region_bgra(screen_dc.handle(), rect.left, rect.top, width, height)
}

/// Grab a region from an arbitrary device context (BGRA, top-down).
pub fn grab_region_bgra(
    source: HDC,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> Result<CapturedImage, String> {
    if width < 1 || height < 1 {
        return Err("Region too small".into());
    }

    unsafe {
        let mem_dc = MemoryDc::compatible(source);
        let bitmap = GdiBitmap::compatible(source, width, height);
        let old = mem_dc.select(bitmap.handle());

        let _ = BitBlt(
            mem_dc.handle(),
            0,
            0,
            width,
            height,
            source,
            x,
            y,
            SRCCOPY,
        );

        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0 as u32,
                ..Default::default()
            },
            ..Default::default()
        };

        let stride = (width as usize) * 4;
        let mut bgra = vec![0u8; stride * height as usize];

        let lines = GetDIBits(
            mem_dc.handle(),
            bitmap.handle(),
            0,
            height as u32,
            Some(bgra.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        );

        let _ = windows::Win32::Graphics::Gdi::SelectObject(mem_dc.handle(), old);

        if lines == 0 {
            return Err("GetDIBits failed".into());
        }

        Ok(CapturedImage {
            width: width as u32,
            height: height as u32,
            pixels: bgra,
        })
    }
}

/// Convert BGRA pixels to RGBA in place.
pub fn rgba_from_bgra(pixels: &mut [u8]) {
    for chunk in pixels.chunks_exact_mut(4) {
        chunk.swap(0, 2);
        chunk[3] = 255;
    }
}

/// Convert RGBA pixels to BGRA in place (for GDI bitmaps).
pub fn bgra_from_rgba(pixels: &mut [u8]) {
    for chunk in pixels.chunks_exact_mut(4) {
        chunk.swap(0, 2);
        chunk[3] = 255;
    }
}

pub fn capture_full_screen() -> Result<CapturedImage, String> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
        SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    };
    unsafe {
        let rect = RECT {
            left: GetSystemMetrics(SM_XVIRTUALSCREEN),
            top: GetSystemMetrics(SM_YVIRTUALSCREEN),
            right: GetSystemMetrics(SM_XVIRTUALSCREEN) + GetSystemMetrics(SM_CXVIRTUALSCREEN),
            bottom: GetSystemMetrics(SM_YVIRTUALSCREEN) + GetSystemMetrics(SM_CYVIRTUALSCREEN),
        };
        capture_rect(&rect)
    }
}
