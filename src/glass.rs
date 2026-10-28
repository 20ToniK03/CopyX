//! DWM-based acrylic / blur-behind effects for "Liquid Glass" look.

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{
    DwmEnableBlurBehindWindow, DwmExtendFrameIntoClientArea, DWM_BLURBEHIND,
};
use windows::Win32::UI::Controls::MARGINS;

#[repr(C)]
struct AccentPolicy {
    accent_state: u32,
    accent_flags: u32,
    gradient_color: u32,
    animation_id: u32,
}

#[repr(C)]
struct WindowCompositionAttributeData {
    attribute: u32,
    data: *mut AccentPolicy,
    size_of_data: u32,
}

const ACCENT_ENABLE_ACRYLICBLURBEHIND: u32 = 4;
const WCA_ACCENT_POLICY: u32 = 19;

type SetWindowCompositionAttributeFn =
    unsafe extern "system" fn(HWND, *mut WindowCompositionAttributeData) -> i32;

pub fn apply_acrylic(hwnd: HWND, tint_color: u32) -> bool {
    let margins = MARGINS {
        cxLeftWidth: -1,
        cxRightWidth: -1,
        cyTopHeight: -1,
        cyBottomHeight: -1,
    };
    let _ = unsafe { DwmExtendFrameIntoClientArea(hwnd, &margins) };

    let user32 = unsafe {
        windows::Win32::System::LibraryLoader::GetModuleHandleW(windows::core::w!("user32.dll"))
    };

    if let Ok(module) = user32 {
        let proc = unsafe {
            windows::Win32::System::LibraryLoader::GetProcAddress(
                module,
                windows::core::s!("SetWindowCompositionAttribute"),
            )
        };

        if let Some(func) = proc {
            let set_wca: SetWindowCompositionAttributeFn = unsafe { std::mem::transmute(func) };

            let mut policy = AccentPolicy {
                accent_state: ACCENT_ENABLE_ACRYLICBLURBEHIND,
                accent_flags: 2,
                gradient_color: tint_color,
                animation_id: 0,
            };

            let mut data = WindowCompositionAttributeData {
                attribute: WCA_ACCENT_POLICY,
                data: &mut policy,
                size_of_data: std::mem::size_of::<AccentPolicy>() as u32,
            };

            let result = unsafe { set_wca(hwnd, &mut data) };
            if result != 0 {
                return true;
            }
        }
    }

    let bb = DWM_BLURBEHIND {
        dwFlags: 0x01,
        fEnable: true.into(),
        ..Default::default()
    };
    unsafe { DwmEnableBlurBehindWindow(hwnd, &bb).is_ok() }
}