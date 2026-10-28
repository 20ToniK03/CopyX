//! Save captured screenshots to disk as PNG files.

use crate::capture::CapturedImage;
use image::{ImageBuffer, Rgba};
use std::fs;
use std::path::PathBuf;

pub fn screenshots_dir() -> PathBuf {
    let pictures = dirs_next::picture_dir().unwrap_or_else(|| PathBuf::from("."));
    pictures.join("CopyX")
}

fn default_filename() -> String {
    let st = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!(
        "CopyX_{:04}{:02}{:02}_{:02}{:02}{:02}.png",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
    )
}

/// Сохранить по указанному пути (для диалога сохранения).
pub fn save_png_to(img: &CapturedImage, path: &std::path::Path) -> Result<PathBuf, String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Cannot create directory: {e}"))?;
    }

    let buffer: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_raw(img.width, img.height, img.pixels.clone())
            .ok_or("Failed to create image buffer")?;

    buffer
        .save(path)
        .map_err(|e| format!("Failed to save PNG: {e}"))?;

    Ok(path.to_path_buf())
}

/// Сохранить в папку по умолчанию с автоименем.
pub fn save_png(img: &CapturedImage) -> Result<PathBuf, String> {
    let path = screenshots_dir().join(default_filename());
    save_png_to(img, &path)
}

/// Показать нативный диалог выбора пути и сохранить PNG.
///
/// `Ok(None)` — пользователь отменил выбор.
pub fn save_with_dialog(img: &CapturedImage) -> Result<Option<PathBuf>, String> {
    unsafe {
        let dir = screenshots_dir();
        fs::create_dir_all(&dir).map_err(|e| format!("Не удалось создать папку: {e}"))?;

        let default_path = dir.join(default_filename());
        let mut file_buf: Vec<u16> = default_path
            .to_string_lossy()
            .encode_utf16()
            .chain(Some(0))
            .collect();
        file_buf.resize(1024, 0);

        let filter: Vec<u16> = "PNG Image\0*.png\0All Files\0*.*\0\0"
            .encode_utf16()
            .collect();

        let mut ofn = windows::Win32::UI::Controls::Dialogs::OPENFILENAMEW {
            lStructSize: std::mem::size_of::<
                windows::Win32::UI::Controls::Dialogs::OPENFILENAMEW,
            >() as u32,
            lpstrFile: windows::core::PWSTR(file_buf.as_mut_ptr()),
            nMaxFile: file_buf.len() as u32,
            lpstrFilter: windows::core::PCWSTR(filter.as_ptr()),
            lpstrDefExt: windows::core::w!("png"),
            Flags: windows::Win32::UI::Controls::Dialogs::OFN_OVERWRITEPROMPT
                | windows::Win32::UI::Controls::Dialogs::OFN_PATHMUSTEXIST,
            ..Default::default()
        };

        let ok = windows::Win32::UI::Controls::Dialogs::GetSaveFileNameW(&mut ofn);
        if !ok.as_bool() {
            return Ok(None);
        }

        let len = file_buf.iter().position(|&c| c == 0).unwrap_or(0);
        let path = PathBuf::from(String::from_utf16_lossy(&file_buf[..len]));

        save_png_to(img, &path).map(Some)
    }
}
