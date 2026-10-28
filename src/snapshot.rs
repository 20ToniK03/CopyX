//! Frozen screenshot of the virtual screen used as the overlay background.
//!
//! The screen is captured *before* the overlay window appears, then drawn
//! back dimmed. The area under the cursor is drawn at full brightness, so the
//! user always sees the real screen instead of a blurred/live copy.

use std::sync::{Mutex, OnceLock};

use crate::capture::{self, CapturedImage};
use crate::render::{BitmapDc, ScreenDc};
use windows::Win32::Foundation::{POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    AlphaBlend, BitBlt, AC_SRC_OVER, BLENDFUNCTION, HDC, SRCCOPY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN,
};

/// How much of the original brightness is kept outside the highlighted area.
/// 102/255 ≈ 40% black overlay → screen stays readable.
const DIM_ALPHA: u8 = 102;

pub struct Snapshot {
    origin: POINT,
    width: i32,
    height: i32,
    bright: BitmapDc,
    dim: BitmapDc,
}

static SNAPSHOT: OnceLock<Mutex<Option<Snapshot>>> = OnceLock::new();

fn store() -> &'static Mutex<Option<Snapshot>> {
    SNAPSHOT.get_or_init(|| Mutex::new(None))
}

/// Capture the whole virtual screen and keep it for the overlay.
pub fn capture() -> Result<(), String> {
    unsafe {
        let origin = POINT {
            x: GetSystemMetrics(SM_XVIRTUALSCREEN),
            y: GetSystemMetrics(SM_YVIRTUALSCREEN),
        };
        let width = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let height = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        if width < 1 || height < 1 {
            return Err("Не удалось определить размер экрана".into());
        }

        let screen = ScreenDc::for_screen();

        // Full-brightness copy of the screen.
        let bright = BitmapDc::compatible(screen.handle(), width, height)
            .ok_or("Не удалось создать bitmap экрана")?;
        BitBlt(
            bright.handle(),
            0,
            0,
            width,
            height,
            screen.handle(),
            origin.x,
            origin.y,
            SRCCOPY,
        )
        .map_err(|e| format!("BitBlt экрана не удался: {e}"))?;

        // Dimmed copy: same pixels, multiplied by 60%.
        let dim = BitmapDc::compatible(screen.handle(), width, height)
            .ok_or("Не удалось создать bitmap затемнения")?;
        let _ = BitBlt(
            dim.handle(),
            0,
            0,
            width,
            height,
            bright.handle(),
            0,
            0,
            SRCCOPY,
        );

        let black = BitmapDc::from_bgra(screen.handle(), 1, 1, &[0, 0, 0, 255])
            .ok_or("Не удалось создать чёрный пиксель")?;
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: DIM_ALPHA,
            AlphaFormat: 0,
        };
        let _ = AlphaBlend(
            dim.handle(),
            0,
            0,
            width,
            height,
            black.handle(),
            0,
            0,
            1,
            1,
            blend,
        );

        let snapshot = Snapshot {
            origin,
            width,
            height,
            bright,
            dim,
        };

        if let Ok(mut slot) = store().lock() {
            *slot = Some(snapshot);
        }
        Ok(())
    }
}

pub fn clear() {
    if let Ok(mut slot) = store().lock() {
        *slot = None;
    }
}

pub fn with_current<R>(f: impl FnOnce(&Snapshot) -> R) -> Option<R> {
    let slot = store().lock().ok()?;
    slot.as_ref().map(f)
}

impl Snapshot {
    /// Draw the dimmed screen as the overlay background.
    pub fn paint_background(&self, dst: HDC) {
        unsafe {
            let _ = BitBlt(
                dst,
                0,
                0,
                self.width,
                self.height,
                self.dim.handle(),
                0,
                0,
                SRCCOPY,
            );
        }
    }

    /// Draw a screen-space rectangle at full brightness.
    pub fn paint_bright(&self, dst: HDC, screen_rect: &RECT) {
        let clipped = self.clip(screen_rect);
        let Some(rect) = clipped else {
            return;
        };

        unsafe {
            let _ = BitBlt(
                dst,
                rect.left - self.origin.x,
                rect.top - self.origin.y,
                rect.right - rect.left,
                rect.bottom - rect.top,
                self.bright.handle(),
                rect.left - self.origin.x,
                rect.top - self.origin.y,
                SRCCOPY,
            );
        }
    }

    /// Copy a screen-space rectangle out of the frozen frame (RGBA output).
    pub fn crop(&self, screen_rect: &RECT) -> Result<CapturedImage, String> {
        let rect = self
            .clip(screen_rect)
            .ok_or("Область выделения пуста")?;

        let mut image = capture::grab_region_bgra(
            self.bright.handle(),
            rect.left - self.origin.x,
            rect.top - self.origin.y,
            rect.right - rect.left,
            rect.bottom - rect.top,
        )?;
        capture::rgba_from_bgra(&mut image.pixels);
        Ok(image)
    }

    fn clip(&self, rect: &RECT) -> Option<RECT> {
        let left = rect.left.max(self.origin.x);
        let top = rect.top.max(self.origin.y);
        let right = rect.right.min(self.origin.x + self.width);
        let bottom = rect.bottom.min(self.origin.y + self.height);
        if right - left < 1 || bottom - top < 1 {
            return None;
        }
        Some(RECT {
            left,
            top,
            right,
            bottom,
        })
    }
}
