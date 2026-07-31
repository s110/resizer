//! The double-clickable program: opens the graphical interface in the
//! browser. On Windows it is built for the GUI subsystem, so no console
//! window appears — neither for this program nor for the ffmpeg processes it
//! spawns (see `NoWindow`).
//!
//! ffmpeg is *not* required to start: when it is missing, the interface shows
//! a setup screen that asks how the user wants to install it.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::PathBuf;

fn main() {
    // Minimal flag handling: this binary intentionally has no CLI surface
    // (that is resizer-cli's job), but these help when debugging.
    let mut port: u16 = 0;
    let mut no_browser = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--port" => port = args.next().and_then(|p| p.parse().ok()).unwrap_or(0),
            "--no-browser" => no_browser = true,
            "--version" | "-V" => {
                println!("resizer {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            _ => {}
        }
    }

    // A missing ffmpeg is handled inside the GUI, not with an error the user
    // would never see (a console window would just flash and vanish).
    let tools = resizer::ffmpeg::find_tools(None).ok();

    if let Err(e) = resizer::server::run(tools, port, no_browser) {
        // Last-resort reporting for a failure before the UI exists: write a
        // log next to the app's data, and try to open it.
        let dir = resizer::install::data_dir();
        let _ = std::fs::create_dir_all(&dir);
        let log: PathBuf = dir.join("resizer-error.log");
        let _ = std::fs::write(&log, format!("resizer no pudo iniciar:\n{e}\n"));
        resizer::server::open_path(&log);
    }
}
