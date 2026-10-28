//! RAII wrappers for Win32 GDI objects to prevent resource leaks.

use windows::Win32::Foundation::{COLORREF, HWND};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateCompatibleBitmap, CreateCompatibleDC, CreatePen, CreateSolidBrush,
    DeleteDC, DeleteObject, EndPaint, GetDC, GetDIBits, ReleaseDC, SelectObject, SetDIBits,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    HBITMAP, HBRUSH, HDC, HGDIOBJ, HPEN, PAINTSTRUCT, PS_DOT, PS_SOLID,
};

pub struct GdiBrush(pub HBRUSH);

impl GdiBrush {
    pub fn solid(color: u32) -> Self {
        unsafe { Self(CreateSolidBrush(COLORREF(color))) }
    }
    pub fn handle(&self) -> HBRUSH {
        self.0
    }
}

impl Drop for GdiBrush {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = DeleteObject(self.0);
            }
        }
    }
}

pub struct GdiPen(pub HPEN);

impl GdiPen {
    pub fn solid(width: i32, color: u32) -> Self {
        unsafe { Self(CreatePen(PS_SOLID, width, COLORREF(color))) }
    }
    pub fn dotted(color: u32) -> Self {
        unsafe { Self(CreatePen(PS_DOT, 1, COLORREF(color))) }
    }
    pub fn handle(&self) -> HPEN {
        self.0
    }
}

impl Drop for GdiPen {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = DeleteObject(self.0);
            }
        }
    }
}

pub struct GdiBitmap(pub HBITMAP);

impl GdiBitmap {
    pub fn compatible(hdc: HDC, width: i32, height: i32) -> Self {
        unsafe { Self(CreateCompatibleBitmap(hdc, width, height)) }
    }
    pub fn handle(&self) -> HBITMAP {
        self.0
    }
}

impl Drop for GdiBitmap {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = DeleteObject(self.0);
            }
        }
    }
}

/// Memory DC with a 32-bit top-down bitmap already selected into it.
///
/// Keeps the previously selected object and restores it on drop, so the
/// bitmap can safely be deleted afterwards.
pub struct BitmapDc {
    dc: MemoryDc,
    bitmap: GdiBitmap,
    old: HGDIOBJ,
}

impl BitmapDc {
    /// Create an empty bitmap of the given size, compatible with `source`.
    pub fn compatible(source: HDC, width: i32, height: i32) -> Option<Self> {
        if width < 1 || height < 1 {
            return None;
        }
        let dc = MemoryDc::compatible(source);
        let bitmap = GdiBitmap::compatible(source, width, height);
        if bitmap.0.is_invalid() {
            return None;
        }
        let old = dc.select(bitmap.handle());
        Some(Self { dc, bitmap, old })
    }

    /// Create a bitmap and fill it with BGRA pixels (top-down rows).
    pub fn from_bgra(
        source: HDC,
        width: i32,
        height: i32,
        bgra: &[u8],
    ) -> Option<Self> {
        if bgra.len() < width as usize * height as usize * 4 {
            return None;
        }
        let this = Self::compatible(source, width, height)?;

        let bmi = BITMAPINFO {
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

        let lines = unsafe {
            SetDIBits(
                this.dc.handle(),
                this.bitmap.handle(),
                0,
                height as u32,
                bgra.as_ptr() as *const _,
                &bmi,
                DIB_RGB_COLORS,
            )
        };
        if lines == 0 {
            return None;
        }
        Some(this)
    }

    pub fn handle(&self) -> HDC {
        self.dc.handle()
    }

    /// Скопировать содержимое битмапа как BGRA-пиксели (строки сверху вниз).
    pub fn read_bgra(&self, width: i32, height: i32) -> Option<Vec<u8>> {
        if width < 1 || height < 1 {
            return None;
        }

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

        let mut buffer = vec![0u8; width as usize * height as usize * 4];
        let lines = unsafe {
            GetDIBits(
                self.dc.handle(),
                self.bitmap.handle(),
                0,
                height as u32,
                Some(buffer.as_mut_ptr() as *mut _),
                &mut bmi,
                DIB_RGB_COLORS,
            )
        };
        if lines == 0 {
            None
        } else {
            Some(buffer)
        }
    }
}

impl Drop for BitmapDc {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc.handle(), self.old);
        }
    }
}

pub struct ScreenDc {
    pub hdc: HDC,
    hwnd: HWND,
}

impl ScreenDc {
    pub fn for_screen() -> Self {
        unsafe {
            let hdc = GetDC(HWND(0));
            Self { hdc, hwnd: HWND(0) }
        }
    }
    pub fn handle(&self) -> HDC {
        self.hdc
    }
}

impl Drop for ScreenDc {
    fn drop(&mut self) {
        unsafe {
            ReleaseDC(self.hwnd, self.hdc);
        }
    }
}

pub struct MemoryDc(pub HDC);

impl MemoryDc {
    pub fn compatible(source: HDC) -> Self {
        unsafe { Self(CreateCompatibleDC(source)) }
    }
    pub fn handle(&self) -> HDC {
        self.0
    }
    pub fn select<T: Into<HGDIOBJ>>(&self, obj: T) -> HGDIOBJ {
        unsafe { SelectObject(self.0, obj.into()) }
    }
}

impl Drop for MemoryDc {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = DeleteDC(self.0);
            }
        }
    }
}

pub struct PaintDc {
    pub hdc: HDC,
    pub ps: PAINTSTRUCT,
    hwnd: HWND,
}

impl PaintDc {
    pub fn begin(hwnd: HWND) -> Self {
        let mut ps = PAINTSTRUCT::default();
        let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
        Self { hdc, ps, hwnd }
    }
    pub fn handle(&self) -> HDC {
        self.hdc
    }
}

impl Drop for PaintDc {
    fn drop(&mut self) {
        unsafe {
            let _ = EndPaint(self.hwnd, &self.ps);
        }
    }
}

pub struct SelectedObject {
    hdc: HDC,
    old: HGDIOBJ,
}

impl SelectedObject {
    pub fn new(hdc: HDC, new_obj: HGDIOBJ) -> Self {
        let old = unsafe { SelectObject(hdc, new_obj) };
        Self { hdc, old }
    }
}

impl Drop for SelectedObject {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.hdc, self.old);
        }
    }
}