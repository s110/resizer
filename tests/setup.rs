//! Tests for the "works without ffmpeg installed" path: the GUI must start,
//! serve its setup screen, offer install options, and refuse media work with
//! a clear message instead of dying before the user sees anything.
//!
//! These cover the failure that made the app look broken on Windows: a
//! missing ffmpeg used to exit immediately, which for a double-clicked
//! program means a console window that flashes and vanishes.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

fn cli() -> &'static str {
    env!("CARGO_BIN_EXE_resizer-cli")
}

fn gui() -> &'static str {
    env!("CARGO_BIN_EXE_resizer")
}

/// An isolated HOME/LOCALAPPDATA so the test never sees a real managed
/// ffmpeg install, and never writes into the developer's own data dir.
fn sandbox(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("resizer-setup-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Start the GUI server with ffmpeg made unreachable.
fn start_without_ffmpeg(tag: &str) -> (Server, u16) {
    let home = sandbox(tag);
    let mut child = Command::new(gui())
        .args(["--port", "0", "--no-browser"])
        // Empty PATH + bogus FFMPEG_PATH + empty HOME = no ffmpeg anywhere.
        .env("PATH", "")
        .env("FFMPEG_PATH", "/definitely/not/here")
        .env("HOME", &home)
        .env("LOCALAPPDATA", &home)
        .env("XDG_DATA_HOME", &home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start resizer GUI");

    // The server prints its URL on the first stdout line. The reader must
    // stay alive for as long as the child does: dropping it closes the pipe,
    // and the next line the child prints would fail.
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    reader.read_line(&mut line).expect("read server URL");
    let port: u16 = line
        .rsplit(':')
        .next()
        .and_then(|p| p.trim().parse().ok())
        .unwrap_or_else(|| panic!("could not parse port from {line:?}"));
    (
        Server {
            child,
            _stdout: reader,
        },
        port,
    )
}

/// Owns the child process (and its stdout pipe) for the test's lifetime.
struct Server {
    child: Child,
    _stdout: BufReader<std::process::ChildStdout>,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Minimal HTTP client (no dev-dependencies): returns (status, body).
fn http(
    port: u16,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&str>,
) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    let body = body.unwrap_or("");
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n",
        body.len()
    );
    if let Some(t) = token {
        req.push_str(&format!("X-Resizer-Token: {t}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    s.write_all(req.as_bytes()).expect("write request");
    let mut raw = String::new();
    s.read_to_string(&mut raw).expect("read response");
    let status: u16 = raw
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or_else(|| panic!("unparseable response to {method} {path}: {raw:?}"));
    let body = raw
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .unwrap_or("")
        .to_string();
    (status, body)
}

fn token_from_page(page: &str) -> String {
    page.split("resizer-token\" content=\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("token in page")
        .to_string()
}

#[test]
fn gui_starts_and_serves_setup_when_ffmpeg_is_missing() {
    let (_server, port) = start_without_ffmpeg("start");

    let (status, page) = http(port, "GET", "/", None, None);
    assert_eq!(status, 200, "the page must render without ffmpeg");
    assert!(
        page.contains("Falta instalar ffmpeg"),
        "setup screen missing"
    );
    assert!(page.contains("id=\"methods\""), "install options missing");

    let (status, body) = http(port, "GET", "/api/state", None, None);
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["ffmpeg_ready"], false, "state must report ffmpeg missing");
}

#[test]
fn install_options_are_offered_with_one_recommendation() {
    let (_server, port) = start_without_ffmpeg("options");

    let (status, body) = http(port, "GET", "/api/ffmpeg/options", None, None);
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let opts = v["options"].as_array().expect("options array");
    assert!(
        !opts.is_empty(),
        "at least one install method must be offered"
    );
    assert!(
        opts.iter().any(|o| o["id"] == "download"),
        "the no-prerequisites download must always be an option: {opts:?}"
    );
    let recommended: Vec<_> = opts.iter().filter(|o| o["recommended"] == true).collect();
    assert_eq!(recommended.len(), 1, "exactly one recommendation: {opts:?}");
    // Every option explains itself before the user commits to it.
    for o in opts {
        assert!(
            o["detail"].as_str().map(|d| d.len() > 20).unwrap_or(false),
            "option without explanation: {o:?}"
        );
    }
}

#[test]
fn media_endpoints_explain_themselves_instead_of_failing_silently() {
    let (_server, port) = start_without_ffmpeg("guard");
    let (_, page) = http(port, "GET", "/", None, None);
    let token = token_from_page(&page);

    for (path, body) in [
        ("/api/convert", "{}"),
        ("/api/preview", "{\"id\":1}"),
        ("/api/folder", "{\"path\":\"/tmp\"}"),
    ] {
        let (status, body) = http(port, "POST", path, Some(&token), Some(body));
        assert_eq!(status, 503, "{path} should report the missing dependency");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(
            v["error"].as_str().unwrap_or("").contains("ffmpeg"),
            "{path} error should name ffmpeg: {body}"
        );
    }
}

#[test]
fn install_endpoint_rejects_unknown_methods() {
    let (_server, port) = start_without_ffmpeg("badmethod");
    let (_, page) = http(port, "GET", "/", None, None);
    let token = token_from_page(&page);

    let (status, _) = http(
        port,
        "POST",
        "/api/ffmpeg/install",
        Some(&token),
        Some("{\"method\":\"rm-rf\"}"),
    );
    assert_eq!(status, 400, "unknown install methods must be refused");
}

/// Pick a port that is free right now.
fn free_port() -> u16 {
    let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    l.local_addr().unwrap().port()
}

#[test]
fn server_survives_a_closed_stdout() {
    // A double-clicked GUI has no console to print to, and a launcher may
    // close the pipe at any moment. Printing must never take the app down.
    let home = sandbox("nostdout");
    let port = free_port();
    let mut child = Command::new(gui())
        .args(["--port", &port.to_string(), "--no-browser"])
        .env("PATH", "")
        .env("HOME", &home)
        .env("LOCALAPPDATA", &home)
        .env("XDG_DATA_HOME", &home)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("start GUI");
    // Close the read end immediately: the child's next print hits a dead pipe.
    drop(child.stdout.take());

    // Give it a moment, then make sure it is alive and serving.
    let mut last = None;
    for _ in 0..40 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)) {
            s.write_all(b"GET /api/state HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
                .unwrap();
            let mut raw = String::new();
            let _ = s.read_to_string(&mut raw);
            if raw.contains("200 OK") {
                last = Some(raw);
                break;
            }
        }
    }
    let alive = last.is_some();
    let _ = child.kill();
    let _ = child.wait();
    assert!(alive, "the server died when its stdout was closed");
}

/// Read the Subsystem field out of a PE executable's optional header.
/// 2 = Windows GUI (no console), 3 = console application.
#[cfg(windows)]
fn pe_subsystem(path: &str) -> u16 {
    let bytes = std::fs::read(path).expect("read executable");
    let pe_off = u32::from_le_bytes(bytes[0x3C..0x40].try_into().unwrap()) as usize;
    assert_eq!(&bytes[pe_off..pe_off + 4], b"PE\0\0", "not a PE file");
    // COFF header is 20 bytes; Subsystem sits 68 bytes into the optional
    // header (same offset for PE32 and PE32+).
    let opt = pe_off + 24;
    u16::from_le_bytes(bytes[opt + 68..opt + 70].try_into().unwrap())
}

#[cfg(windows)]
#[test]
fn gui_binary_has_no_console_window_on_windows() {
    // This is the whole point of shipping two binaries: double-clicking
    // `resizer.exe` must not open a black console window.
    const WINDOWS_GUI: u16 = 2;
    const CONSOLE: u16 = 3;
    assert_eq!(
        pe_subsystem(gui()),
        WINDOWS_GUI,
        "resizer.exe must be a GUI-subsystem binary"
    );
    assert_eq!(
        pe_subsystem(cli()),
        CONSOLE,
        "resizer-cli.exe must keep its console"
    );
}

#[test]
fn cli_lists_install_options_without_ffmpeg() {
    let home = sandbox("cli-list");
    let out = Command::new(cli())
        .args(["install-ffmpeg"])
        .env("PATH", "")
        .env("FFMPEG_PATH", "/definitely/not/here")
        .env("HOME", &home)
        .env("LOCALAPPDATA", &home)
        .env("XDG_DATA_HOME", &home)
        .output()
        .expect("run install-ffmpeg");
    assert!(out.status.success(), "listing options must not fail");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("--method download"), "got: {text}");
    assert!(text.contains("recomendado"), "must recommend one: {text}");
}

#[test]
fn cli_rejects_an_unknown_install_method() {
    let home = sandbox("cli-bad");
    let out = Command::new(cli())
        .args(["install-ffmpeg", "--method", "carrier-pigeon"])
        .env("PATH", "")
        .env("HOME", &home)
        .env("LOCALAPPDATA", &home)
        .env("XDG_DATA_HOME", &home)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("carrier-pigeon"), "got: {err}");
}

#[test]
fn ffmpeg_is_not_required_next_to_the_executable() {
    // The managed install directory must be outside the executable's folder,
    // so a copy in Downloads (or on a USB stick) works the same.
    let out = Command::new(cli())
        .args(["install-ffmpeg"])
        .env("PATH", "")
        .env("FFMPEG_PATH", "/definitely/not/here")
        .env("HOME", sandbox("layout"))
        .env("LOCALAPPDATA", sandbox("layout"))
        .env("XDG_DATA_HOME", sandbox("layout"))
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let exe_dir = PathBuf::from(cli()).parent().unwrap().display().to_string();
    assert!(
        !text.contains(&exe_dir),
        "the installer must not target the executable's own folder:\n{text}"
    );
}
