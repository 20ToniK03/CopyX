//! Автозапуск CopyX вместе с Windows.
//!
//! Используется ветка `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`:
//! права администратора не нужны, запись видна в «Диспетчере задач →
//! Автозагрузка» и удаляется простым удалением значения при отключении.

use windows::core::{w, Error};
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ,
};

fn run_key() -> windows::core::PCWSTR {
    w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run")
}

fn value_name() -> windows::core::PCWSTR {
    w!("CopyX")
}

/// Включён ли автозапуск для текущего пользователя.
pub fn is_enabled() -> bool {
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, run_key(), 0, KEY_QUERY_VALUE, &mut key).is_err() {
            return false;
        }
        let exists = RegQueryValueExW(key, value_name(), None, None, None, None).is_ok();
        let _ = RegCloseKey(key);
        exists
    }
}

/// Включить или выключить автозапуск. При включении записывается путь к
/// текущему exe, поэтому после переноса папки настройку нужно включить заново.
pub fn set_enabled(enabled: bool) -> Result<(), String> {
    if enabled {
        enable()
    } else {
        disable()
    }
}

fn enable() -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("Не удалось определить путь к CopyX: {e}"))?;
    // Кавычки нужны, если путь содержит пробелы.
    let command = format!("\"{}\"", exe.display());
    let wide: Vec<u16> = command.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = unsafe { std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2) };

    unsafe {
        let mut key = HKEY::default();
        RegOpenKeyExW(HKEY_CURRENT_USER, run_key(), 0, KEY_SET_VALUE, &mut key)
            .map_err(|e| format!("Не удалось открыть реестр: {}", describe(&e)))?;

        let result = RegSetValueExW(key, value_name(), 0, REG_SZ, Some(bytes))
            .map_err(|e| format!("Не удалось записать автозапуск: {}", describe(&e)));
        let _ = RegCloseKey(key);
        result
    }
}

fn disable() -> Result<(), String> {
    unsafe {
        let mut key = HKEY::default();
        RegOpenKeyExW(HKEY_CURRENT_USER, run_key(), 0, KEY_SET_VALUE, &mut key)
            .map_err(|e| format!("Не удалось открыть реестр: {}", describe(&e)))?;

        // Отсутствующее значение — не ошибка: автозапуск уже выключен.
        let _ = RegDeleteValueW(key, value_name());
        let _ = RegCloseKey(key);
    }

    if is_enabled() {
        Err("Не удалось отключить автозапуск".to_string())
    } else {
        Ok(())
    }
}

fn describe(err: &Error) -> String {
    let message = err.message();
    if message.is_empty() {
        format!("код {:#x}", err.code().0)
    } else {
        message.to_string()
    }
}
