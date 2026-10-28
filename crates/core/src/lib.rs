/// Core types for CopyX — a lightweight ShareX alternative in Rust.

/// A rectangular region on screen, in absolute (virtual-screen) coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Region {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// How the user wants to capture the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureMode {
    /// User draws a rectangle on the overlay.
    Region,
    /// Capture the window under the cursor.
    Window,
    /// Capture the entire virtual screen (all monitors).
    FullScreen,
}

/// Current state of the capture workflow.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaptureState {
    /// App sits in the tray, doing nothing.
    #[default]
    Idle,
    /// Overlay is visible, waiting for user action.
    OverlayShown,
    /// User is dragging a selection rectangle.
    Selecting,
    /// Selection complete, ready to capture.
    Selected,
}

/// Supported output formats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveFormat {
    Png,
}
