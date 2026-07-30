//! The conversion pipeline for one file, plus a multi-threaded queue that
//! drives many files in parallel (each worker owns one ffmpeg process).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use crate::ffmpeg::{self, Tools};
use crate::plan::{self, ImageFormat, MediaInfo, RateControl, Settings};

/// File extensions we accept as inputs.
pub const VIDEO_EXTS: &[&str] = &[
    "mp4", "mov", "m4v", "mkv", "webm", "avi", "gif", "mts", "wmv",
];
pub const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "bmp", "tif", "tiff", "heic"];

pub fn is_media_path(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let e = e.to_ascii_lowercase();
            VIDEO_EXTS.contains(&e.as_str()) || IMAGE_EXTS.contains(&e.as_str())
        })
        .unwrap_or(false)
}

/// Collect media files from a mix of files and directories.
/// Directories are walked recursively when `recursive` is set, otherwise only
/// their direct children are taken. Hidden files are skipped, and so is
/// `exclude` (the output directory), so re-running a conversion never
/// re-ingests its own results.
pub fn collect_inputs(paths: &[PathBuf], recursive: bool, exclude: Option<&Path>) -> Vec<PathBuf> {
    let excluded = exclude.map(normalized);
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            walk_dir(p, recursive, excluded.as_deref(), &mut out);
        } else if p.is_file() && is_media_path(p) {
            out.push(p.clone());
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Best-effort canonical form for path comparison (the path may not exist yet).
fn normalized(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

fn walk_dir(dir: &Path, recursive: bool, exclude: Option<&Path>, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let hidden = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with('.'))
            .unwrap_or(true);
        if hidden {
            continue;
        }
        if path.is_dir() {
            let skip = exclude.is_some_and(|ex| normalized(&path) == *ex);
            if recursive && !skip {
                walk_dir(&path, true, exclude, out);
            }
        } else if is_media_path(&path) {
            out.push(path);
        }
    }
}

/// Decide the output extension for a source file under the given settings.
pub fn output_ext(input: &Path, info: &MediaInfo, s: &Settings) -> String {
    if info.is_video {
        return "mp4".into();
    }
    let src = input
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    match s.image_format {
        ImageFormat::Jpg => "jpg".into(),
        ImageFormat::Webp => "webp".into(),
        ImageFormat::Png => "png".into(),
        ImageFormat::Keep => match src.as_str() {
            "jpg" | "jpeg" => "jpg".into(),
            "png" => "png".into(),
            "webp" => "webp".into(),
            // Exotic sources (heic, tiff, bmp) become web-friendly jpg.
            _ => "jpg".into(),
        },
    }
}

/// Build a non-clobbering output path: `<out_dir>/<stem>-web.<ext>`,
/// adding `-2`, `-3`, ... if that name is taken.
pub fn output_path(out_dir: &Path, input: &Path, ext: &str) -> PathBuf {
    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("media");
    let mut candidate = out_dir.join(format!("{stem}-web.{ext}"));
    let mut n = 2;
    while candidate.exists() {
        candidate = out_dir.join(format!("{stem}-web-{n}.{ext}"));
        n += 1;
    }
    candidate
}

static PASSLOG_SEQ: AtomicU64 = AtomicU64::new(0);

/// Unique passlog prefix so parallel two-pass jobs never collide.
fn passlog_prefix(scratch: &Path) -> PathBuf {
    let n = PASSLOG_SEQ.fetch_add(1, Ordering::Relaxed);
    scratch.join(format!("x264-pass-{}-{n}", std::process::id()))
}

fn cleanup_passlog(prefix: &Path) {
    // x264 writes "<prefix>-0.log" and "<prefix>-0.log.mbtree".
    if let (Some(dir), Some(name)) = (prefix.parent(), prefix.file_name().and_then(|n| n.to_str()))
    {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for e in entries.flatten() {
                if e.file_name().to_string_lossy().starts_with(name) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
}

pub struct ConvertOutcome {
    pub output: PathBuf,
    pub out_bytes: u64,
}

/// Convert one file. `on_progress` receives 0.0..=1.0 across all passes.
pub fn convert_file(
    tools: &Tools,
    input: &Path,
    out_dir: &Path,
    settings: &Settings,
    scratch: &Path,
    mut on_progress: impl FnMut(f64),
) -> Result<ConvertOutcome, String> {
    let info = ffmpeg::probe(tools, input)?;
    std::fs::create_dir_all(out_dir).map_err(|e| format!("cannot create output dir: {e}"))?;
    let ext = output_ext(input, &info, settings);
    let output = output_path(out_dir, input, &ext);

    if info.is_video {
        convert_video(
            tools,
            input,
            &output,
            &info,
            settings,
            scratch,
            &mut on_progress,
        )?;
    } else {
        convert_image(tools, input, &output, &info, settings)?;
        on_progress(1.0);
    }

    let out_bytes = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);
    if out_bytes == 0 {
        return Err("output file is empty".into());
    }
    Ok(ConvertOutcome { output, out_bytes })
}

fn convert_video(
    tools: &Tools,
    input: &Path,
    output: &Path,
    info: &MediaInfo,
    settings: &Settings,
    scratch: &Path,
    on_progress: &mut impl FnMut(f64),
) -> Result<(), String> {
    let plan = plan::plan_video(info, settings);
    let passlog = passlog_prefix(scratch);
    let cmds = ffmpeg::video_commands(input, output, &plan, &settings.x264_preset, &passlog, None);
    let passes = cmds.len() as f64;
    let result = (|| {
        for (i, args) in cmds.iter().enumerate() {
            let base = i as f64 / passes;
            ffmpeg::run_with_progress(tools, args, info.duration_s, |f| {
                on_progress(base + f / passes)
            })?;
        }
        Ok(())
    })();
    cleanup_passlog(&passlog);
    result
}

fn convert_image(
    tools: &Tools,
    input: &Path,
    output: &Path,
    info: &MediaInfo,
    settings: &Settings,
) -> Result<(), String> {
    let plan = plan::plan_video(info, settings);
    let is_webp = output
        .extension()
        .map(|e| e.eq_ignore_ascii_case("webp"))
        .unwrap_or(false);

    let is_png = output
        .extension()
        .map(|e| e.eq_ignore_ascii_case("png"))
        .unwrap_or(false);

    match settings.max_mb {
        // PNG is lossless: there is no quality ladder to walk. Encode once and
        // be honest if the result cannot meet the budget.
        Some(mb) if is_png => {
            let args = ffmpeg::image_command(input, output, &plan, 0, false);
            ffmpeg::run_quiet(tools, &args)?;
            let budget = (mb * 1024.0 * 1024.0) as u64;
            let size = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);
            if size > budget {
                let _ = std::fs::remove_file(output);
                return Err(format!(
                    "PNG cannot be compressed below the size cap ({:.1} MB > {:.1} MB). \
                     Use webp or jpg output for this one.",
                    size as f64 / 1048576.0,
                    mb
                ));
            }
            Ok(())
        }
        // Auto-tune: walk the quality ladder until the file fits the budget.
        Some(mb) => {
            let budget = (mb * 1024.0 * 1024.0) as u64;
            let format = if is_webp {
                ImageFormat::Webp
            } else {
                ImageFormat::Jpg
            };
            let ladder = plan::image_quality_ladder(format);
            let last = *ladder.last().expect("ladder is never empty");
            for q in &ladder {
                let args = ffmpeg::image_command(input, output, &plan, *q, is_webp);
                ffmpeg::run_quiet(tools, &args)?;
                let size = std::fs::metadata(output)
                    .map(|m| m.len())
                    .unwrap_or(u64::MAX);
                if size <= budget || *q == last {
                    return Ok(());
                }
            }
            Ok(())
        }
        None => {
            let q = if is_webp {
                settings.image_quality.clamp(1, 100) as u32
            } else {
                plan::jpeg_q_from_percent(settings.image_quality)
            };
            let args = ffmpeg::image_command(input, output, &plan, q, is_webp);
            ffmpeg::run_quiet(tools, &args)
        }
    }
}

/// Encode a short, fast preview of a video (or a scaled image) reflecting the
/// current settings. Returns (preview_path, estimated_full_bytes).
pub fn make_preview(
    tools: &Tools,
    input: &Path,
    info: &MediaInfo,
    settings: &Settings,
    scratch: &Path,
    id: u64,
) -> Result<(PathBuf, u64), String> {
    std::fs::create_dir_all(scratch).map_err(|e| format!("scratch dir: {e}"))?;
    if info.is_video {
        let clip = info.duration_s.min(2.5);
        let plan = plan::plan_video(info, settings);
        let out = scratch.join(format!("preview-{id}.mp4"));
        let args = ffmpeg::preview_command(input, &out, &plan, Some(clip));
        ffmpeg::run_quiet(tools, &args)?;
        let bytes = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
        let est = match &plan.rate {
            // The planned bitrate is known exactly; add ~4% mux overhead.
            RateControl::TwoPass { video_kbps } => {
                let kbps = (video_kbps + plan.audio_kbps.unwrap_or(0)) as f64;
                (kbps * 1000.0 * info.duration_s / 8.0 * 1.04) as u64
            }
            // CRF: extrapolate the preview bytes to the full duration.
            RateControl::Crf(_) if clip > 0.0 => (bytes as f64 * (info.duration_s / clip)) as u64,
            _ => bytes,
        };
        Ok((out, est))
    } else {
        let ext = output_ext(input, info, settings);
        let out = scratch.join(format!("preview-{id}.{ext}"));
        convert_image(tools, input, &out, info, settings)?;
        let bytes = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
        Ok((out, bytes))
    }
}

/// Result of one queue item.
#[derive(Debug)]
pub struct BulkItemResult {
    /// Ok((output_path, input_bytes, output_bytes)) or the failure message.
    pub result: Result<(PathBuf, u64, u64), String>,
}

/// Run many conversions on a fixed-size worker pool. `on_event` is called on
/// progress ticks and completion — from worker threads, so it must be Sync.
pub fn run_bulk<F>(
    tools: &Tools,
    inputs: Vec<PathBuf>,
    out_dir: &Path,
    settings: &Settings,
    scratch: &Path,
    jobs: usize,
    on_event: F,
) -> Vec<BulkItemResult>
where
    F: Fn(usize, f64, Option<&BulkItemResult>) + Send + Sync,
{
    let jobs = jobs.max(1);
    let (tx, rx) = mpsc::channel::<usize>();
    for i in 0..inputs.len() {
        tx.send(i).expect("queue send");
    }
    drop(tx);

    let rx = Arc::new(Mutex::new(rx));
    let results: Arc<Mutex<Vec<Option<BulkItemResult>>>> =
        Arc::new(Mutex::new((0..inputs.len()).map(|_| None).collect()));
    let inputs = Arc::new(inputs);
    let on_event = Arc::new(on_event);

    std::thread::scope(|scope| {
        for _ in 0..jobs.min(inputs.len().max(1)) {
            let rx = Arc::clone(&rx);
            let results = Arc::clone(&results);
            let inputs = Arc::clone(&inputs);
            let on_event = Arc::clone(&on_event);
            scope.spawn(move || loop {
                let idx = {
                    let guard = rx.lock().expect("queue lock");
                    guard.try_recv()
                };
                let Ok(idx) = idx else { break };
                let input = &inputs[idx];
                let in_bytes = std::fs::metadata(input).map(|m| m.len()).unwrap_or(0);
                let res = convert_file(tools, input, out_dir, settings, scratch, |f| {
                    on_event(idx, f, None);
                });
                let item = BulkItemResult {
                    result: res.map(|o| (o.output, in_bytes, o.out_bytes)),
                };
                on_event(idx, 1.0, Some(&item));
                results.lock().expect("results lock")[idx] = Some(item);
            });
        }
    });

    Arc::try_unwrap(results)
        .expect("workers done")
        .into_inner()
        .expect("results lock")
        .into_iter()
        .flatten()
        .collect()
}

/// Default parallel jobs: half the cores, between 1 and 8.
pub fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map(|n| (n.get() / 2).clamp(1, 8))
        .unwrap_or(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_paths_are_filtered_by_extension() {
        assert!(is_media_path(Path::new("a/b/clip.MP4")));
        assert!(is_media_path(Path::new("photo.jpeg")));
        assert!(is_media_path(Path::new("anim.gif")));
        assert!(!is_media_path(Path::new("notes.txt")));
        assert!(!is_media_path(Path::new("noext")));
    }

    #[test]
    fn output_ext_maps_video_and_images() {
        let vid = MediaInfo {
            is_video: true,
            ..Default::default()
        };
        let img = MediaInfo {
            is_video: false,
            ..Default::default()
        };
        let s = Settings::default();
        assert_eq!(output_ext(Path::new("a.mov"), &vid, &s), "mp4");
        assert_eq!(output_ext(Path::new("a.gif"), &vid, &s), "mp4");
        assert_eq!(output_ext(Path::new("a.PNG"), &img, &s), "png");
        assert_eq!(output_ext(Path::new("a.heic"), &img, &s), "jpg");
        let webp = Settings {
            image_format: ImageFormat::Webp,
            ..Settings::default()
        };
        assert_eq!(output_ext(Path::new("a.png"), &img, &webp), "webp");
    }

    #[test]
    fn output_path_never_clobbers() {
        let dir = std::env::temp_dir().join(format!("resizer-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = output_path(&dir, Path::new("clip.mov"), "mp4");
        assert_eq!(first.file_name().unwrap(), "clip-web.mp4");
        std::fs::write(&first, b"x").unwrap();
        let second = output_path(&dir, Path::new("clip.mov"), "mp4");
        assert_eq!(second.file_name().unwrap(), "clip-web-2.mp4");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn collect_inputs_walks_folders() {
        let dir = std::env::temp_dir().join(format!("resizer-walk-{}", std::process::id()));
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(dir.join("a.mp4"), b"x").unwrap();
        std::fs::write(dir.join("b.txt"), b"x").unwrap();
        std::fs::write(dir.join(".hidden.mp4"), b"x").unwrap();
        std::fs::write(sub.join("c.jpg"), b"x").unwrap();

        let flat = collect_inputs(std::slice::from_ref(&dir), false, None);
        assert_eq!(flat.len(), 1, "{flat:?}");
        let deep = collect_inputs(std::slice::from_ref(&dir), true, None);
        assert_eq!(deep.len(), 2, "{deep:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn collect_inputs_skips_the_output_dir() {
        let dir = std::env::temp_dir().join(format!("resizer-excl-{}", std::process::id()));
        let resized = dir.join("resized");
        std::fs::create_dir_all(&resized).unwrap();
        std::fs::write(dir.join("a.mp4"), b"x").unwrap();
        std::fs::write(resized.join("a-web.mp4"), b"x").unwrap();

        // Without the exclusion a recursive re-run would re-ingest a-web.mp4.
        let all = collect_inputs(std::slice::from_ref(&dir), true, None);
        assert_eq!(all.len(), 2, "{all:?}");
        let safe = collect_inputs(std::slice::from_ref(&dir), true, Some(&resized));
        assert_eq!(safe.len(), 1, "{safe:?}");
        assert!(safe[0].ends_with("a.mp4"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn default_jobs_is_sane() {
        let j = default_jobs();
        assert!((1..=8).contains(&j));
    }
}
