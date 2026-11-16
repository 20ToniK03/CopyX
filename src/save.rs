//! Save captured screenshots to disk as PNG files.

use std::fs;
use std::path::{Path, PathBuf};

use crate::capture::CapturedImage;
use image::{ImageBuffer, Rgba};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, IBindCtx,
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FileSaveDialog, IFileSaveDialog, IShellItem, SHCreateItemFromParsingName,
    FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT, FOS_PATHMUSTEXIST, SIGDN_FILESYSPATH,
};

/// Папка со снимками по умолчанию: «Изображения\CopyX».
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

/// Крошечный файл с последней папкой сохранения. Пара десятков байт — заметно
/// дешевле реестра и не держит ничего в памяти после закрытия диалога.
fn config_file() -> Option<PathBuf> {
    dirs_next::config_dir().map(|dir| dir.join("CopyX").join("last_save_dir.txt"))
}

fn remembered_dir() -> Option<PathBuf> {
    let text = fs::read_to_string(config_file()?).ok()?;
    let dir = PathBuf::from(text.trim());
    // Если папку удалили или перенесли — молча возвращаемся к «Изображения\CopyX».
    dir.is_dir().then_some(dir)
}

fn remember_dir(dir: &Path) {
    let Some(path) = config_file() else {
        return;
    };
    if remembered_dir().as_deref() == Some(dir) {
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, dir.to_string_lossy().as_bytes());
}

fn initial_dir() -> PathBuf {
    remembered_dir().unwrap_or_else(screenshots_dir)
}

fn wide(path: &Path) -> Vec<u16> {
    path.to_string_lossy()
        .encode_utf16()
        .chain(Some(0))
        .collect()
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
    // Папку запоминаем только после подтверждения в диалоге, чтобы отмена не
    // меняла «папку по умолчанию».
    let dir = initial_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("Не удалось создать папку: {e}"))?;

    let Some(path) = pick_path(&dir, &default_filename())? else {
        return Ok(None);
    };

    if let Some(parent) = path.parent() {
        remember_dir(parent);
    }

    save_png_to(img, &path).map(Some)
}

/// Нативный диалог «Сохранить как». `IFileDialog::SetFolder` открывает нужную
/// папку всегда — в отличие от старого `GetSaveFileNameW`, который подменяет её
/// последней папкой из общего MRU (из-за него и открывался OneDrive).
fn pick_path(dir: &Path, filename: &str) -> Result<Option<PathBuf>, String> {
    unsafe {
        let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let result = show_dialog(dir, filename);
        if initialized {
            CoUninitialize();
        }
        result
    }
}

fn show_dialog(dir: &Path, filename: &str) -> Result<Option<PathBuf>, String> {
    // Ошибка «пользователь нажал Отмена» — не ошибка.
    const CANCELLED: i32 = 0x800704C7u32 as i32;

    unsafe {
        let dialog: IFileSaveDialog =
            CoCreateInstance(&FileSaveDialog, None::<&windows::core::IUnknown>, CLSCTX_INPROC_SERVER)
                .map_err(|e| format!("Не удалось открыть диалог сохранения: {e}"))?;

        dialog
            .SetOptions(FOS_OVERWRITEPROMPT | FOS_PATHMUSTEXIST | FOS_FORCEFILESYSTEM)
            .map_err(|e| format!("Не удалось настроить диалог сохранения: {e}"))?;
        let _ = dialog.SetTitle(w!("Сохранить снимок"));

        let dir_wide = wide(dir);
        let folder: IShellItem =
            SHCreateItemFromParsingName(PCWSTR(dir_wide.as_ptr()), None::<&IBindCtx>)
                .map_err(|e| format!("Не удалось открыть папку «{}»: {e}", dir.display()))?;
        dialog
            .SetFolder(&folder)
            .map_err(|e| format!("Не удалось открыть папку «{}»: {e}", dir.display()))?;

        let filters = [
            COMDLG_FILTERSPEC {
                pszName: w!("PNG-изображение"),
                pszSpec: w!("*.png"),
            },
            COMDLG_FILTERSPEC {
                pszName: w!("Все файлы"),
                pszSpec: w!("*.*"),
            },
        ];
        let _ = dialog.SetFileTypes(&filters);
        let _ = dialog.SetFileTypeIndex(1);
        let _ = dialog.SetDefaultExtension(w!("png"));
        dialog
            .SetFileName(&HSTRING::from(filename))
            .map_err(|e| format!("Не удалось задать имя файла: {e}"))?;

        if let Err(e) = dialog.Show(None::<&HWND>) {
            if e.code().0 == CANCELLED {
                return Ok(None);
            }
            return Err(format!("Не удалось показать диалог сохранения: {e}"));
        }

        let item = dialog
            .GetResult()
            .map_err(|e| format!("Не удалось получить выбранный файл: {e}"))?;
        let name = item
            .GetDisplayName(SIGDN_FILESYSPATH)
            .map_err(|e| format!("Не удалось получить путь файла: {e}"))?;
        let text = name.to_string().ok();
        CoTaskMemFree(Some(name.0 as *const _));

        text.map(PathBuf::from)
            .map(Some)
            .ok_or_else(|| "Не удалось прочитать путь файла".to_string())
    }
}
