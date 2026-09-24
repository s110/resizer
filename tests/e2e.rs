//! End-to-end tests that drive the real binary against real ffmpeg.
//! They are skipped (with a notice) when ffmpeg is not installed, so local
//! `cargo test` still passes everywhere; in CI (`CI` set) a missing ffmpeg is
//! a failure, never a silent skip. The main batch scenario lives in
//! `tests/e2e_shoot_batch.rs`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_resizer-cli")
}

fn tmp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("resizer-e2e-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Synthesize a test video with ffmpeg's built-in generators.
fn make_test_video(path: &Path, seconds: u32, w: u32, h: u32, with_audio: bool) {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-y", "-f", "lavfi", "-i"]);
    cmd.arg(format!("testsrc2=size={w}x{h}:rate=30:duration={seconds}"));
    if with_audio {
        cmd.args(["-f", "lavfi", "-i"]);
        cmd.arg(format!("sine=frequency=440:duration={seconds}"));
        cmd.args(["-c:a", "aac", "-shortest"]);
    }
    cmd.args(["-c:v", "libx264", "-pix_fmt", "yuv420p"]);
    cmd.arg(path);
    let out = cmd.output().expect("run ffmpeg");
    assert!(
        out.status.success(),
        "test video generation failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// ffprobe helper: returns (width, height, has_audio).
fn probe_dims(path: &Path) -> (u32, u32, bool) {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-print_format", "json", "-show_streams"])
        .arg(path)
        .output()
        .expect("run ffprobe");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let streams = v["streams"].as_array().unwrap();
    let video = streams.iter().find(|s| s["codec_type"] == "video").unwrap();
    let audio = streams.iter().any(|s| s["codec_type"] == "audio");
    (
        video["width"].as_u64().unwrap() as u32,
        video["height"].as_u64().unwrap() as u32,
        audio,
    )
}

/// A HOME with no managed ffmpeg install, so lookup really does fail.
fn no_ffmpeg_home() -> PathBuf {
    let d = std::env::temp_dir().join(format!("resizer-nohome-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    d
}

macro_rules! require_ffmpeg {
    () => {
        if !ffmpeg_available() {
            assert!(
                std::env::var_os("CI").is_none(),
                "ffmpeg missing in CI: e2e tests must not silently skip"
            );
            eprintln!("SKIP: ffmpeg not installed");
            return;
        }
    };
}

#[test]
fn keep_audio_overrides_hover_mute_and_still_fits_the_budget() {
    require_ffmpeg!();
    let dir = tmp_dir("audio");
    let src = dir.join("clip.mp4");
    make_test_video(&src, 6, 640, 480, true);

    // A budget tight enough that the audio track must be paid for out of it:
    // if the planner forgot to reserve those bits, the file would overshoot.
    let out_dir = dir.join("out");
    let status = Command::new(bin())
        .arg("convert")
        .arg(&src)
        .args(["--preset", "hover", "--keep-audio", "--max-mb", "0.5"])
        .args(["--speed", "veryfast"])
        .arg("--out")
        .arg(&out_dir)
        .status()
        .unwrap();
    assert!(status.success());
    let out = out_dir.join("clip-web.mp4");
    let (w, h, has_audio) = probe_dims(&out);
    assert!(has_audio, "--keep-audio should preserve the track");
    assert_eq!((w, h), (384, 480), "hover crop still applies");
    let size = std::fs::metadata(&out).unwrap().len();
    assert!(
        size <= 512 * 1024,
        "video + audio overshot the 0.5 MB budget: {size} bytes"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn probe_subcommand_reports_media_info() {
    require_ffmpeg!();
    let dir = tmp_dir("probe");
    let src = dir.join("p.mp4");
    make_test_video(&src, 2, 640, 480, true);

    let out = Command::new(bin()).arg("probe").arg(&src).output().unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["width"], 640);
    assert_eq!(v["height"], 480);
    assert_eq!(v["has_audio"], true);
    assert_eq!(v["is_video"], true);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_ffmpeg_gives_friendly_error() {
    // Point --ffmpeg at a non-existent binary: the error must point at the
    // installer, not panic. (Runs even without ffmpeg installed.)
    let out = Command::new(bin())
        .args(["--ffmpeg", "/definitely/not/here/ffmpeg", "probe", "x.mp4"])
        .env("FFMPEG_PATH", "/definitely/not/here/either")
        .env("PATH", "")
        .env("HOME", no_ffmpeg_home())
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("ffmpeg was not found"), "got: {err}");
    assert!(
        err.contains("install-ffmpeg"),
        "must point at the installer: {err}"
    );
}
