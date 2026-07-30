//! Thin wrapper around the ffmpeg / ffprobe executables: locate them, probe
//! sources, build argument lists, and run encodes while reporting progress.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::plan::{EncodePlan, MediaInfo, RateControl};

#[derive(Debug, Clone)]
pub struct Tools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

/// Human-friendly install instructions shown when ffmpeg is missing.
pub const INSTALL_HELP: &str = "\
ffmpeg was not found. Install it and try again:
  - Windows:  winget install Gyan.FFmpeg   (or download the \"essentials\" build from https://www.gyan.dev/ffmpeg/builds/)
  - macOS:    brew install ffmpeg
  - Linux:    sudo apt install ffmpeg
You can also place ffmpeg/ffprobe next to this program, or set FFMPEG_PATH.";

/// Locate ffmpeg + ffprobe: explicit path, FFMPEG_PATH, next to our own
/// executable, then PATH.
pub fn find_tools(explicit: Option<&Path>) -> Result<Tools, String> {
    let exe_name = |base: &str| {
        if cfg!(windows) {
            format!("{base}.exe")
        } else {
            base.to_string()
        }
    };

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(p) = explicit {
        candidates.push(p.to_path_buf());
    }
    if let Ok(p) = std::env::var("FFMPEG_PATH") {
        candidates.push(PathBuf::from(p));
    }
    if let Ok(me) = std::env::current_exe() {
        if let Some(dir) = me.parent() {
            candidates.push(dir.join(exe_name("ffmpeg")));
        }
    }
    candidates.push(PathBuf::from(exe_name("ffmpeg")));

    for c in candidates {
        let probe = sibling_ffprobe(&c);
        if runs(&c) && runs(&probe) {
            return Ok(Tools {
                ffmpeg: c,
                ffprobe: probe,
            });
        }
    }
    Err(INSTALL_HELP.to_string())
}

/// ffprobe living next to a given ffmpeg path (or bare name for PATH lookup).
fn sibling_ffprobe(ffmpeg: &Path) -> PathBuf {
    let name = if cfg!(windows) {
        "ffprobe.exe"
    } else {
        "ffprobe"
    };
    match ffmpeg.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join(name),
        _ => PathBuf::from(name),
    }
}

fn runs(bin: &Path) -> bool {
    Command::new(bin)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn version(tools: &Tools) -> String {
    Command::new(&tools.ffmpeg)
        .arg("-version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.lines().next().map(str::to_string))
        .unwrap_or_else(|| "unknown".into())
}

/// Probe a media file with ffprobe (JSON output).
pub fn probe(tools: &Tools, path: &Path) -> Result<MediaInfo, String> {
    let out = Command::new(&tools.ffprobe)
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_streams",
            "-show_format",
        ])
        .arg(path)
        .output()
        .map_err(|e| format!("failed to run ffprobe: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "ffprobe could not read {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("bad ffprobe output: {e}"))?;
    parse_probe(&v, std::fs::metadata(path).map(|m| m.len()).unwrap_or(0))
}

/// Parse ffprobe's JSON into MediaInfo (pure, unit-testable).
pub fn parse_probe(v: &serde_json::Value, size_bytes: u64) -> Result<MediaInfo, String> {
    let empty = Vec::new();
    let streams = v["streams"].as_array().unwrap_or(&empty);
    let video = streams
        .iter()
        .find(|s| s["codec_type"] == "video")
        .ok_or("no video/image stream found")?;
    let has_audio = streams.iter().any(|s| s["codec_type"] == "audio");

    let width = video["width"].as_u64().unwrap_or(0) as u32;
    let height = video["height"].as_u64().unwrap_or(0) as u32;
    if width == 0 || height == 0 {
        return Err("stream has no dimensions".into());
    }

    let rotation = video["side_data_list"]
        .as_array()
        .and_then(|sd| {
            sd.iter().find_map(|d| {
                d["rotation"]
                    .as_i64()
                    .or_else(|| d["rotation"].as_f64().map(|f| f as i64))
            })
        })
        .unwrap_or(0) as i32;

    let duration_s = video["duration"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .or_else(|| {
            v["format"]["duration"]
                .as_str()
                .and_then(|s| s.parse::<f64>().ok())
        })
        .unwrap_or(0.0);

    let fps = parse_rate(video["avg_frame_rate"].as_str().unwrap_or(""))
        .or_else(|| parse_rate(video["r_frame_rate"].as_str().unwrap_or("")))
        .unwrap_or(0.0);

    // Single-frame streams (or streams with no meaningful duration) are images.
    let nb_frames = video["nb_frames"]
        .as_str()
        .and_then(|s| s.parse::<u64>().ok());
    let is_video = duration_s > 0.05 && nb_frames != Some(1);

    Ok(MediaInfo {
        width,
        height,
        rotation,
        duration_s,
        fps,
        has_audio,
        is_video,
        size_bytes,
    })
}

fn parse_rate(s: &str) -> Option<f64> {
    let (num, den) = s.split_once('/')?;
    let (num, den) = (num.parse::<f64>().ok()?, den.parse::<f64>().ok()?);
    if den == 0.0 || num == 0.0 {
        None
    } else {
        Some(num / den)
    }
}

/// Common video-encode arguments (input through codecs), shared by both passes.
fn base_video_args(
    input: &Path,
    plan: &EncodePlan,
    x264_preset: &str,
    trim_s: Option<f64>,
) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "-hide_banner".into(),
        "-y".into(),
        "-i".into(),
        input.display().to_string(),
    ];
    if let Some(t) = trim_s {
        a.extend(["-t".into(), format!("{t}")]);
    }
    a.extend(["-vf".into(), crate::plan::filter_chain(plan)]);
    if let Some(fps) = plan.fps {
        a.extend(["-r".into(), format!("{fps}")]);
    }
    a.extend([
        "-c:v".into(),
        "libx264".into(),
        "-preset".into(),
        x264_preset.into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
    ]);
    a
}

fn audio_args(plan: &EncodePlan) -> Vec<String> {
    match plan.audio_kbps {
        Some(kbps) => vec![
            "-c:a".into(),
            "aac".into(),
            "-b:a".into(),
            format!("{kbps}k"),
        ],
        None => vec!["-an".into()],
    }
}

/// Single-pass encode used for previews: same filters and rate control as the
/// real plan (one ABR pass approximates the two-pass result), always the
/// veryfast preset. Sharing base_video_args keeps previews honest when encode
/// flags change.
pub fn preview_command(
    input: &Path,
    output: &Path,
    plan: &EncodePlan,
    trim_s: Option<f64>,
) -> Vec<String> {
    let mut a = base_video_args(input, plan, "veryfast", trim_s);
    match &plan.rate {
        RateControl::Crf(crf) => a.extend(["-crf".into(), format!("{crf}")]),
        RateControl::TwoPass { video_kbps } => a.extend(["-b:v".into(), format!("{video_kbps}k")]),
    }
    a.extend(audio_args(plan));
    a.extend(["-movflags".into(), "+faststart".into()]);
    a.push(output.display().to_string());
    a
}

/// Build the ffmpeg invocation(s) for a video encode. Returns one arg list
/// per pass (CRF = 1, two-pass ABR = 2). `passlog` is the x264 stats prefix.
pub fn video_commands(
    input: &Path,
    output: &Path,
    plan: &EncodePlan,
    x264_preset: &str,
    passlog: &Path,
    trim_s: Option<f64>,
) -> Vec<Vec<String>> {
    let audio = audio_args(plan);
    let progress: Vec<String> = vec!["-nostats".into(), "-progress".into(), "pipe:1".into()];

    match &plan.rate {
        RateControl::Crf(crf) => {
            let mut a = base_video_args(input, plan, x264_preset, trim_s);
            a.extend(["-crf".into(), format!("{crf}")]);
            a.extend(audio);
            a.extend(["-movflags".into(), "+faststart".into()]);
            a.extend(progress);
            a.push(output.display().to_string());
            vec![a]
        }
        RateControl::TwoPass { video_kbps } => {
            let rate: Vec<String> = vec![
                "-b:v".into(),
                format!("{video_kbps}k"),
                "-maxrate".into(),
                format!("{}k", video_kbps * 3 / 2),
                "-bufsize".into(),
                format!("{}k", video_kbps * 3),
            ];
            let mut p1 = base_video_args(input, plan, x264_preset, trim_s);
            p1.extend(rate.clone());
            p1.extend([
                "-pass".into(),
                "1".into(),
                "-passlogfile".into(),
                passlog.display().to_string(),
                "-an".into(),
                "-f".into(),
                "null".into(),
            ]);
            p1.extend(progress.clone());
            p1.push(if cfg!(windows) {
                "NUL".into()
            } else {
                "/dev/null".into()
            });

            let mut p2 = base_video_args(input, plan, x264_preset, trim_s);
            p2.extend(rate);
            p2.extend([
                "-pass".into(),
                "2".into(),
                "-passlogfile".into(),
                passlog.display().to_string(),
            ]);
            p2.extend(audio);
            p2.extend(["-movflags".into(), "+faststart".into()]);
            p2.extend(progress);
            p2.push(output.display().to_string());
            vec![p1, p2]
        }
    }
}

/// Build the ffmpeg invocation for an image encode.
/// `quality` is in the target encoder's own scale (see plan::image_quality_ladder).
pub fn image_command(
    input: &Path,
    output: &Path,
    plan: &EncodePlan,
    quality: u32,
    is_webp: bool,
) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "-hide_banner".into(),
        "-y".into(),
        "-i".into(),
        input.display().to_string(),
        "-vf".into(),
        crate::plan::filter_chain(plan),
        "-frames:v".into(),
        "1".into(),
        "-an".into(),
    ];
    if is_webp {
        a.extend([
            "-c:v".into(),
            "libwebp".into(),
            "-q:v".into(),
            format!("{quality}"),
        ]);
    } else if output
        .extension()
        .map(|e| e.eq_ignore_ascii_case("png"))
        .unwrap_or(false)
    {
        a.extend(["-compression_level".into(), "100".into()]);
    } else {
        a.extend(["-q:v".into(), format!("{quality}")]);
    }
    a.push(output.display().to_string());
    a
}

/// Run one ffmpeg pass, streaming `-progress pipe:1` key=value output to the
/// callback as a fraction of `duration_s`.
pub fn run_with_progress(
    tools: &Tools,
    args: &[String],
    duration_s: f64,
    mut on_progress: impl FnMut(f64),
) -> Result<(), String> {
    let mut child = Command::new(&tools.ffmpeg)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to start ffmpeg: {e}"))?;

    // Drain stderr on a side thread so ffmpeg never blocks on a full pipe;
    // keep the tail for error reporting.
    let stderr = child.stderr.take().unwrap();
    let err_tail = std::thread::spawn(move || {
        let mut tail: Vec<String> = Vec::new();
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            tail.push(line);
            if tail.len() > 30 {
                tail.remove(0);
            }
        }
        tail.join("\n")
    });

    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(us) = line.strip_prefix("out_time_us=") {
                if let Ok(us) = us.trim().parse::<f64>() {
                    if duration_s > 0.0 {
                        on_progress((us / 1_000_000.0 / duration_s).clamp(0.0, 1.0));
                    }
                }
            } else if line.trim() == "progress=end" {
                on_progress(1.0);
            }
        }
    }

    let status = child.wait().map_err(|e| format!("ffmpeg died: {e}"))?;
    let tail = err_tail.join().unwrap_or_default();
    if status.success() {
        Ok(())
    } else {
        Err(format!("ffmpeg failed:\n{tail}"))
    }
}

/// Run ffmpeg without progress reporting (images, thumbnails).
pub fn run_quiet(tools: &Tools, args: &[String]) -> Result<(), String> {
    let out = Command::new(&tools.ffmpeg)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("failed to start ffmpeg: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = err.lines().rev().take(10).collect();
        Err(format!(
            "ffmpeg failed:\n{}",
            tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{EncodePlan, RateControl};
    use serde_json::json;

    fn plan_crf() -> EncodePlan {
        EncodePlan {
            crop: Some((864, 1080)),
            out_w: 864,
            out_h: 1080,
            fps: Some(30.0),
            rate: RateControl::Crf(23),
            audio_kbps: None,
        }
    }

    #[test]
    fn parse_probe_reads_video() {
        let v = json!({
            "streams": [
                {"codec_type": "video", "width": 1920, "height": 1080,
                 "avg_frame_rate": "30000/1001", "duration": "12.5",
                 "nb_frames": "374"},
                {"codec_type": "audio"}
            ],
            "format": {"duration": "12.5"}
        });
        let i = parse_probe(&v, 1000).unwrap();
        assert_eq!((i.width, i.height), (1920, 1080));
        assert!(i.has_audio && i.is_video);
        assert!((i.fps - 29.97).abs() < 0.01);
        assert!((i.duration_s - 12.5).abs() < 1e-9);
    }

    #[test]
    fn parse_probe_reads_rotation_and_images() {
        let v = json!({
            "streams": [{
                "codec_type": "video", "width": 1080, "height": 1920,
                "side_data_list": [{"side_data_type": "Display Matrix", "rotation": -90}],
                "avg_frame_rate": "0/0", "nb_frames": "1"
            }],
            "format": {}
        });
        let i = parse_probe(&v, 5).unwrap();
        assert_eq!(i.rotation, -90);
        assert!(!i.is_video);
        assert!(!i.has_audio);
        assert_eq!(i.display_dims(), (1920, 1080));
    }

    #[test]
    fn parse_probe_rejects_audio_only() {
        let v = json!({"streams": [{"codec_type": "audio"}], "format": {}});
        assert!(parse_probe(&v, 0).is_err());
    }

    #[test]
    fn crf_command_is_single_pass_with_faststart() {
        let cmds = video_commands(
            Path::new("in.mp4"),
            Path::new("out.mp4"),
            &plan_crf(),
            "medium",
            Path::new("log"),
            None,
        );
        assert_eq!(cmds.len(), 1);
        let joined = cmds[0].join(" ");
        assert!(joined.contains("-crf 23"), "{joined}");
        assert!(joined.contains("-an"), "{joined}");
        assert!(joined.contains("+faststart"), "{joined}");
        assert!(joined.contains("yuv420p"), "{joined}");
        assert!(joined.contains("crop=864:1080"), "{joined}");
        assert!(joined.ends_with("out.mp4"), "{joined}");
    }

    #[test]
    fn two_pass_commands_share_the_passlog() {
        let plan = EncodePlan {
            rate: RateControl::TwoPass { video_kbps: 2500 },
            audio_kbps: Some(96),
            ..plan_crf()
        };
        let cmds = video_commands(
            Path::new("in.mp4"),
            Path::new("out.mp4"),
            &plan,
            "medium",
            Path::new("statslog"),
            None,
        );
        assert_eq!(cmds.len(), 2);
        let (p1, p2) = (cmds[0].join(" "), cmds[1].join(" "));
        assert!(p1.contains("-pass 1") && p1.contains("statslog"), "{p1}");
        assert!(p1.contains("-f null"), "{p1}");
        assert!(p1.contains("-an"), "pass 1 must not encode audio: {p1}");
        assert!(p2.contains("-pass 2") && p2.contains("statslog"), "{p2}");
        assert!(p2.contains("-b:v 2500k"), "{p2}");
        assert!(p2.contains("-b:a 96k"), "{p2}");
        assert!(p2.ends_with("out.mp4"), "{p2}");
    }

    #[test]
    fn image_command_picks_the_right_encoder() {
        let webp = image_command(
            Path::new("a.png"),
            Path::new("a.webp"),
            &plan_crf(),
            80,
            true,
        )
        .join(" ");
        assert!(
            webp.contains("libwebp") && webp.contains("-q:v 80"),
            "{webp}"
        );

        let jpg = image_command(
            Path::new("a.png"),
            Path::new("a.jpg"),
            &plan_crf(),
            4,
            false,
        )
        .join(" ");
        assert!(jpg.contains("-q:v 4") && !jpg.contains("libwebp"), "{jpg}");
        assert!(jpg.contains("-frames:v 1"), "{jpg}");
    }
}
