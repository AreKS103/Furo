//! Project Furo â€” Text Injection (cross-platform)
//!
//! Uses clipboard + paste simulation to inject text into the focused window.

#[cfg(target_os = "windows")]
#[path = "typer_win.rs"]
mod platform;

#[cfg(target_os = "macos")]
#[path = "typer_mac.rs"]
#[allow(unexpected_cfgs)] // objc 0.2 macros reference `feature = "cargo-clippy"`
mod platform;

pub use platform::*;

/// Captured target window info for text injection.
///
/// On Windows: `parent`/`child` are HWNDs (window handles).
/// On macOS: `parent` is a pid_t (process ID), `child` equals `parent`.
#[derive(Debug, Clone, Copy)]
pub struct CapturedTarget {
    pub parent: isize,
    pub child: isize,
}

