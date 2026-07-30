//! End-to-end tests that drive the real binary against real ffmpeg.
//! They are skipped (with a notice) when ffmpeg is not installed, so local
//! `cargo test` still passes everywhere; CI installs ffmpeg and runs them.

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
    env!("CARGO_BIN_EXE_resizer")
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

fn make_test_image(path: &Path, w: u32, h: u32) {
    let out = Command::new("ffmpeg")
        .args(["-y", "-f", "lavfi", "-i"])
        .arg(format!("testsrc2=size={w}x{h}:rate=1:duration=1"))
        .args(["-frames:v", "1"])
        .arg(path)
        .output()
        .expect("run ffmpeg");
    assert!(out.status.success());
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

macro_rules! require_ffmpeg {
    () => {
        if !ffmpeg_available() {
            eprintln!("SKIP: ffmpeg not installed");
            return;
        }
    };
}

#[test]
fn hover_preset_produces_4_5_muted_video_under_budget() {
    require_ffmpeg!();
    let dir = tmp_dir("hover");
    let src = dir.join("landscape.mp4");
    make_test_video(&src, 4, 640, 360, true);

    let out_dir = dir.join("out");
    let status = Command::new(bin())
        .arg("convert")
        .arg(&src)
        .args(["--preset", "hover", "--max-mb", "2", "--speed", "veryfast"])
        .arg("--out")
        .arg(&out_dir)
        .status()
        .expect("run resizer");
    assert!(status.success());

    let out = out_dir.join("landscape-web.mp4");
    assert!(out.is_file(), "missing output");
    let (w, h, has_audio) = probe_dims(&out);
    // 4:5 aspect, within rounding.
    assert!(
        ((w as f64 / h as f64) - 0.8).abs() < 0.02,
        "expected 4:5, got {w}x{h}"
    );
    assert!(!has_audio, "hover preset must strip audio");
    let size = std::fs::metadata(&out).unwrap().len();
    assert!(
        size <= 2 * 1024 * 1024,
        "auto-tune busted the 2MB budget: {size} bytes"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn keep_audio_flag_overrides_hover_mute() {
    require_ffmpeg!();
    let dir = tmp_dir("audio");
    let src = dir.join("clip.mp4");
    make_test_video(&src, 2, 320, 240, true);

    let out_dir = dir.join("out");
    let status = Command::new(bin())
        .arg("convert")
        .arg(&src)
        .args([
            "--preset",
            "hover",
            "--keep-audio",
            "--max-mb",
            "2",
            "--speed",
            "veryfast",
        ])
        .arg("--out")
        .arg(&out_dir)
        .status()
        .unwrap();
    assert!(status.success());
    let (_, _, has_audio) = probe_dims(&out_dir.join("clip-web.mp4"));
    assert!(has_audio, "--keep-audio should preserve the track");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn folder_bulk_convert_handles_mixed_media() {
    require_ffmpeg!();
    let dir = tmp_dir("bulk");
    let media = dir.join("media");
    std::fs::create_dir_all(&media).unwrap();
    make_test_video(&media.join("a.mp4"), 2, 320, 240, false);
    make_test_video(&media.join("b.mp4"), 2, 426, 240, false);
    make_test_image(&media.join("c.png"), 800, 600);
    std::fs::write(media.join("notes.txt"), "not media").unwrap();

    let out_dir = dir.join("out");
    let status = Command::new(bin())
        .arg("convert")
        .arg(&media)
        .args([
            "--preset", "original", "--crf", "30", "--speed", "veryfast", "--jobs", "2",
        ])
        .arg("--out")
        .arg(&out_dir)
        .status()
        .unwrap();
    assert!(status.success());

    assert!(out_dir.join("a-web.mp4").is_file());
    assert!(out_dir.join("b-web.mp4").is_file());
    assert!(out_dir.join("c-web.png").is_file());
    assert_eq!(std::fs::read_dir(&out_dir).unwrap().count(), 3);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn image_autotune_fits_size_budget() {
    require_ffmpeg!();
    let dir = tmp_dir("img");
    let src = dir.join("big.png");
    make_test_image(&src, 1920, 1080);

    let out_dir = dir.join("out");
    let status = Command::new(bin())
        .arg("convert")
        .arg(&src)
        .args(["--image-format", "jpg", "--max-mb", "0.2"])
        .arg("--out")
        .arg(&out_dir)
        .status()
        .unwrap();
    assert!(status.success());
    let out = out_dir.join("big-web.jpg");
    let size = std::fs::metadata(&out).unwrap().len();
    assert!(size <= 210 * 1024, "image over budget: {size}");
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
    // Point --ffmpeg at a non-existent binary: the error must be the install
    // help, not a panic. (Runs even without ffmpeg installed.)
    let out = Command::new(bin())
        .args(["--ffmpeg", "/definitely/not/here/ffmpeg", "probe", "x.mp4"])
        .env("FFMPEG_PATH", "/definitely/not/here/either")
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("ffmpeg was not found"), "got: {err}");
}
