//! resizer — prepares images and videos for kamiru.art by driving ffmpeg.
//!
//! Two binaries share this library: `resizer` (double-clickable GUI, no
//! console window on Windows) and `resizer-cli` (terminal interface).

pub mod ffmpeg;
pub mod install;
pub mod jobs;
pub mod plan;
pub mod server;

/// Extension trait so every child process we spawn stays invisible on
/// Windows. Without this, each ffmpeg call flashes a console window.
pub trait NoWindow {
    fn no_window(&mut self) -> &mut Self;
}

impl NoWindow for std::process::Command {
    #[cfg(windows)]
    fn no_window(&mut self) -> &mut Self {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        self.creation_flags(CREATE_NO_WINDOW)
    }

    #[cfg(not(windows))]
    fn no_window(&mut self) -> &mut Self {
        self
    }
}
