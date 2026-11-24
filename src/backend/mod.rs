//! Platform capture backends.
//!
//! Every backend exposes the same free functions and an `ActiveRecording`
//! type with the same methods, so the GUI and CLI are platform agnostic:
//!
//! * `capabilities()`, `list_displays()`, `list_windows()`, `list_cameras()`
//! * `start(options)`, `screenshot(target, region, path)`, `grab(target)`
//! * `set_dpi_awareness()`

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::*;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use self::unix::*;

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
compile_error!("Scroder supports Windows, macOS and Linux.");
