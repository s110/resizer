//! Thin wrapper around the ffmpeg / ffprobe executables: locate them, probe
//! sources, build argument lists, and run encodes while reporting progress.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::install;
use crate::plan::{EncodePlan, MediaInfo, RateControl};
use crate::NoWindow;

#[derive(Debug, Clone)]
pub struct Tools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

/// Shown when ffmpeg is missing and the user asked for a terminal-only run.
/// The GUI never shows this: it offers to install ffmpeg instead.
pub const INSTALL_HELP: &str = "\
ffmpeg was not found. Let resizer install it for you:
  resizer-cli install-ffmpeg            (shows the options for your system)
  resizer-cli install-ffmpeg --method download
Or open the graphical interface (run `resizer`) and follow the setup screen.";

/// Locate ffmpeg + ffprobe, in order: explicit path, FFMPEG_PATH, the copy
/// this program installed for itself, the system PATH, and finally next to
/// our own executable (a portable folder still works, but is never required).
pub fn find_tools(explicit: Option<&Path>) -> Result<Tools, String> {
    for c in candidate_paths(explicit) {
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

/// Every place ffmpeg might live, in priority order.
pub fn candidate_paths(explicit: Option<&Path>) -> Vec<PathBuf> {
    // An explicit choice (--ffmpeg or FFMPEG_PATH) is authoritative: if the
    // user names a build, silently running a different one would be wrong.
    if let Some(p) = explicit {
        return vec![p.to_path_buf()];
    }
    if let Some(p) = std::env::var_os("FFMPEG_PATH") {
        return vec![PathBuf::from(p)];
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    // Our own managed copy (installed by the setup screen).
    candidates.push(install::bin_dir().join(install::exe_name("ffmpeg")));
    // Anything on PATH, including package-manager installs.
    candidates.push(PathBuf::from(install::exe_name("ffmpeg")));
    // Package managers drop ffmpeg in well-known places that a *running*
    // process cannot see yet: PATH is inherited at launch, so a fresh winget
    // or Homebrew install would otherwise look like a failed one.
    candidates.extend(
        install::package_manager_dirs()
            .into_iter()
            .map(|d| d.join(install::exe_name("ffmpeg"))),
    );
    // Portable layout: alongside the executable.
    if let Ok(me) = std::env::current_exe() {
        if let Some(dir) = me.parent() {
            candidates.push(dir.join(install::exe_name("ffmpeg")));
        }
    }
    candidates
}

/// True when ffmpeg is usable right now.
pub fn is_available() -> bool {
    find_tools(None).is_ok()
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
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .no_window()
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn version(tools: &Tools) -> String {
    Command::new(&tools.ffmpeg)
        .arg("-version")
        .no_window()
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
        .no_window()
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
    let mut info = parse_probe(&v, std::fs::metadata(path).map(|m| m.len()).unwrap_or(0))?;
    // ffprobe does not report EXIF orientation at stream level, and ffmpeg
    // versions disagree on which formats they rotate by themselves (6.1 turns
    // JPEG but not PNG, 8.x turns both). Read it from the file so the plan and
    // the pixels always agree; images are then encoded with -noautorotate.
    if !info.is_video {
        if let Some(o) = read_head(path, EXIF_SCAN_BYTES).and_then(|d| exif_orientation(&d)) {
            info.orientation = o;
        }
    }
    Ok(info)
}

/// How much of an image to scan for EXIF. JPEG and PNG keep it before the
/// pixel data; this bound only matters for huge TIFF or WebP files that
/// store it at the end, which then stay as stored instead of costing
/// gigabytes of memory across parallel jobs.
const EXIF_SCAN_BYTES: u64 = 64 * 1024 * 1024;

fn read_head(path: &Path, limit: u64) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut data = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(limit)
        .read_to_end(&mut data)
        .ok()?;
    Some(data)
}

/// EXIF Orientation (1..=8) of a JPEG, PNG, WebP or TIFF file, or None when
/// there is none or it cannot be read. Never panics on malformed input.
pub fn exif_orientation(data: &[u8]) -> Option<u8> {
    if data.starts_with(&[0xFF, 0xD8]) {
        return jpeg_exif(data).and_then(tiff_orientation);
    }
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return png_exif(data).and_then(tiff_orientation);
    }
    if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        return webp_exif(data).and_then(tiff_orientation);
    }
    tiff_orientation(data)
}

/// The TIFF block of the first APP1 "Exif" segment before the image data.
fn jpeg_exif(data: &[u8]) -> Option<&[u8]> {
    let mut i = 2;
    loop {
        if *data.get(i)? != 0xFF {
            return None;
        }
        let marker = *data.get(i + 1)?;
        match marker {
            0xFF => i += 1,               // fill byte
            0x01 | 0xD0..=0xD8 => i += 2, // no length field
            0xD9 | 0xDA => return None,   // EOI / start of scan
            _ => {
                let len = u16::from_be_bytes([*data.get(i + 2)?, *data.get(i + 3)?]) as usize;
                let seg = data.get(i + 4..(i + 2).checked_add(len)?)?;
                if marker == 0xE1 {
                    if let Some(tiff) = seg.strip_prefix(b"Exif\0\0") {
                        return Some(tiff);
                    }
                }
                i += 2 + len.max(2);
            }
        }
    }
}

/// The payload of the PNG eXIf chunk.
fn png_exif(data: &[u8]) -> Option<&[u8]> {
    let mut i = 8;
    loop {
        let len = u32::from_be_bytes(data.get(i..i + 4)?.try_into().ok()?) as usize;
        let kind = data.get(i + 4..i + 8)?;
        let body = data.get(i + 8..(i + 8).checked_add(len)?)?;
        match kind {
            b"eXIf" => return Some(body),
            b"IEND" => return None,
            _ => i = i + 12 + len,
        }
    }
}

/// The payload of the WebP EXIF chunk (some writers keep the JPEG prefix).
fn webp_exif(data: &[u8]) -> Option<&[u8]> {
    let mut i = 12;
    loop {
        let kind = data.get(i..i + 4)?;
        let len = u32::from_le_bytes(data.get(i + 4..i + 8)?.try_into().ok()?) as usize;
        let body = data.get(i + 8..(i + 8).checked_add(len)?)?;
        if kind == b"EXIF" {
            return Some(body.strip_prefix(b"Exif\0\0").unwrap_or(body));
        }
        i = i + 8 + len + (len & 1);
    }
}

/// Orientation tag (0x0112) of IFD0 in a TIFF structure, either byte order.
fn tiff_orientation(t: &[u8]) -> Option<u8> {
    let le = match t.get(0..4)? {
        b"II*\0" => true,
        b"MM\0*" => false,
        _ => return None,
    };
    let u16_at = |o: usize| -> Option<u16> {
        let b: [u8; 2] = t.get(o..o.checked_add(2)?)?.try_into().ok()?;
        Some(if le {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    };
    let u32_at = |o: usize| -> Option<u32> {
        let b: [u8; 4] = t.get(o..o.checked_add(4)?)?.try_into().ok()?;
        Some(if le {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    };
    let ifd = u32_at(4)? as usize;
    for k in 0..u16_at(ifd)? as usize {
        let entry = ifd.checked_add(2 + k * 12)?;
        if u16_at(entry)? != 0x0112 {
            continue;
        }
        let value = match u16_at(entry + 2)? {
            3 => u16_at(entry + 8)? as u32, // SHORT, the type the spec uses
            4 => u32_at(entry + 8)?,        // LONG, written by some tools
            _ => return None,
        };
        return (1..=8).contains(&value).then_some(value as u8);
    }
    None
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

    // Images are encoded with -noautorotate, so a stream-level rotation (HEIC)
    // becomes an orientation we apply ourselves. EXIF, when present, wins
    // (see `probe`).
    let orientation = if is_video {
        1
    } else {
        match rotation.rem_euclid(360) {
            90 => 8,
            180 => 3,
            270 => 6,
            _ => 1,
        }
    };

    Ok(MediaInfo {
        width,
        height,
        rotation,
        orientation,
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
    // The plan already turns the picture upright (plan.orient), so ffmpeg must
    // not rotate it again, and the output must not carry the orientation
    // forward or viewers would rotate it a second time (ffmpeg 8 writes it
    // into PNG eXIf). Only the display matrix is dropped: ICC profiles stay.
    let mut a: Vec<String> = vec![
        "-hide_banner".into(),
        "-y".into(),
        "-noautorotate".into(),
        "-i".into(),
        input.display().to_string(),
        "-vf".into(),
        format!(
            "{},sidedata=mode=delete:type=DISPLAYMATRIX",
            crate::plan::filter_chain(plan)
        ),
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
        .no_window()
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
        .no_window()
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
            orient: None,
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
