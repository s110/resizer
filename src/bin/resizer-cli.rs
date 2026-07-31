//! resizer-cli — the terminal interface. Double-clicking users want the
//! `resizer` binary instead (graphical, no console window); this one exists
//! for scripted and bulk work.

use std::path::PathBuf;
use std::sync::Mutex;

use clap::{Parser, Subcommand};

use resizer::plan::{ImageFormat, Settings};
use resizer::{ffmpeg, install, jobs, server};

#[derive(Parser)]
#[command(
    name = "resizer-cli",
    version,
    about = "Resize & compress images/videos for the web (friendly ffmpeg wrapper)",
    long_about = None
)]
struct Cli {
    /// Path to the ffmpeg executable (default: auto-detect).
    #[arg(long, global = true)]
    ffmpeg: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open the graphical interface in your browser (default when no command).
    Gui {
        /// Port to serve on (0 = pick a free one).
        #[arg(long, default_value_t = 0)]
        port: u16,
        /// Don't open the browser automatically.
        #[arg(long)]
        no_browser: bool,
    },
    /// Convert files and/or whole folders from the command line.
    Convert {
        /// Input files and/or directories.
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
        /// Preset: hover (4:5, muted, ≤10MB), square, landscape, original.
        #[arg(short, long, default_value = "original")]
        preset: String,
        /// Auto-tune: maximum output size in MB (two-pass for videos).
        #[arg(long)]
        max_mb: Option<f64>,
        /// Maximum output width in pixels.
        #[arg(long)]
        max_width: Option<u32>,
        /// Maximum output height in pixels.
        #[arg(long)]
        max_height: Option<u32>,
        /// x264 CRF quality, 0-51 (used when --max-mb is not set).
        #[arg(long, value_parser = clap::value_parser!(u8).range(0..=51))]
        crf: Option<u8>,
        /// Cap the output frame rate (e.g. 30). Use 0 to keep the source rate.
        #[arg(long)]
        fps: Option<f64>,
        /// Remove the audio track from videos.
        #[arg(long)]
        no_audio: bool,
        /// Keep the audio track (overrides presets that mute, like hover).
        #[arg(long, conflicts_with = "no_audio")]
        keep_audio: bool,
        /// x264 speed preset: veryfast | medium | slow.
        #[arg(long, default_value = "medium")]
        speed: String,
        /// Image output format: keep | jpg | webp | png.
        #[arg(long, default_value = "keep")]
        image_format: String,
        /// Image quality 1-100 (used when --max-mb is not set).
        #[arg(long, default_value_t = 85)]
        image_quality: u8,
        /// Output directory (default: <first input's folder>/resized).
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Parallel conversions (default: half your CPU cores).
        #[arg(short, long)]
        jobs: Option<usize>,
        /// Recurse into subdirectories of input folders.
        #[arg(short, long)]
        recursive: bool,
    },
    /// Show what ffprobe sees in a file.
    Probe { input: PathBuf },
    /// Install ffmpeg. With no --method, lists the options for this system.
    InstallFfmpeg {
        /// winget | chocolatey | homebrew | apt | dnf | pacman | download
        #[arg(long)]
        method: Option<String>,
    },
}

fn main() {
    let cli = Cli::parse();

    // These two work without ffmpeg: one installs it, the other offers to.
    match &cli.command {
        Some(Cmd::InstallFfmpeg { method }) => {
            std::process::exit(match run_install(method.as_deref()) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("error: {e}");
                    1
                }
            });
        }
        Some(Cmd::Gui { .. }) | None => {
            let (port, no_browser) = match cli.command {
                Some(Cmd::Gui { port, no_browser }) => (port, no_browser),
                _ => (0, false),
            };
            let tools = ffmpeg::find_tools(cli.ffmpeg.as_deref()).ok();
            if let Err(e) = server::run(tools, port, no_browser) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
            return;
        }
        _ => {}
    }

    let tools = match ffmpeg::find_tools(cli.ffmpeg.as_deref()) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };

    let result = match cli.command {
        None | Some(Cmd::Gui { .. }) | Some(Cmd::InstallFfmpeg { .. }) => {
            unreachable!("handled above")
        }
        Some(Cmd::Probe { input }) => match ffmpeg::probe(&tools, &input) {
            Ok(info) => {
                println!("{}", serde_json::to_string_pretty(&info).unwrap());
                Ok(())
            }
            Err(e) => Err(e),
        },
        Some(Cmd::Convert {
            inputs,
            preset,
            max_mb,
            max_width,
            max_height,
            crf,
            fps,
            no_audio,
            keep_audio,
            speed,
            image_format,
            image_quality,
            out,
            jobs: njobs,
            recursive,
        }) => {
            let mut s = match Settings::preset(&preset) {
                Some(s) => s,
                None => {
                    eprintln!(
                        "unknown preset '{preset}' (try: hover, square, landscape, original)"
                    );
                    std::process::exit(2);
                }
            };
            if let Some(mb) = max_mb {
                s.max_mb = Some(mb);
            }
            if let Some(w) = max_width {
                s.max_width = w;
            }
            if let Some(h) = max_height {
                s.max_height = h;
            }
            if let Some(c) = crf {
                s.crf = c;
                if max_mb.is_none() {
                    s.max_mb = None; // explicit CRF wins over a preset's size cap
                }
            }
            match fps {
                Some(f) if f <= 0.0 => s.fps_cap = None,
                Some(f) => s.fps_cap = Some(f),
                None => {}
            }
            if no_audio {
                s.strip_audio = true;
            }
            if keep_audio {
                s.strip_audio = false;
            }
            s.x264_preset = match speed.as_str() {
                "veryfast" | "medium" | "slow" => speed,
                other => {
                    eprintln!("unknown --speed '{other}' (try: veryfast, medium, slow)");
                    std::process::exit(2);
                }
            };
            s.image_format = match image_format.as_str() {
                "keep" => ImageFormat::Keep,
                "jpg" | "jpeg" => ImageFormat::Jpg,
                "webp" => ImageFormat::Webp,
                "png" => ImageFormat::Png,
                other => {
                    eprintln!("unknown --image-format '{other}' (try: keep, jpg, webp, png)");
                    std::process::exit(2);
                }
            };
            s.image_quality = image_quality;

            run_convert(&tools, inputs, s, out, njobs, recursive)
        }
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run_convert(
    tools: &ffmpeg::Tools,
    inputs: Vec<PathBuf>,
    settings: Settings,
    out: Option<PathBuf>,
    njobs: Option<usize>,
    recursive: bool,
) -> Result<(), String> {
    let out_dir = out.unwrap_or_else(|| {
        let first = &inputs[0];
        let base = if first.is_dir() {
            first.clone()
        } else {
            first
                .parent()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
        };
        base.join("resized")
    });

    let files = jobs::collect_inputs(&inputs, recursive, Some(&out_dir));
    if files.is_empty() {
        return Err("no images or videos found in the given inputs".into());
    }
    let njobs = njobs.unwrap_or_else(jobs::default_jobs);
    let scratch = std::env::temp_dir().join(format!("resizer-cli-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).map_err(|e| format!("scratch dir: {e}"))?;

    println!(
        "converting {} file(s) -> {}  ({} at a time)",
        files.len(),
        out_dir.display(),
        njobs.min(files.len())
    );

    let progress: Mutex<Vec<u8>> = Mutex::new(vec![0; files.len()]);
    let names: Vec<String> = files
        .iter()
        .map(|f| {
            f.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?")
                .to_string()
        })
        .collect();

    let results = jobs::run_bulk(
        tools,
        files.clone(),
        &out_dir,
        &settings,
        &scratch,
        njobs,
        |idx, frac, done| {
            if let Some(item) = done {
                match &item.result {
                    Ok((outp, in_b, out_b)) => {
                        let pct = if *in_b > 0 {
                            format!("-{}%", 100u64.saturating_sub(out_b * 100 / in_b))
                        } else {
                            String::new()
                        };
                        println!(
                            "  done  {}  ({} -> {} {})",
                            outp.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
                            human(*in_b),
                            human(*out_b),
                            pct
                        );
                    }
                    Err(e) => {
                        let first = e.lines().last().unwrap_or("failed");
                        println!("  FAIL  {}  ({first})", names[idx]);
                    }
                }
            } else {
                // Coarse progress ticks (25% steps) so logs stay readable.
                let step = (frac * 4.0) as u8;
                let mut seen = progress.lock().unwrap();
                if step > seen[idx] {
                    seen[idx] = step;
                    println!("  ....  {}  {}%", names[idx], step as u32 * 25);
                }
            }
        },
    );

    let _ = std::fs::remove_dir_all(&scratch);

    let ok = results.iter().filter(|r| r.result.is_ok()).count();
    let failed = results.len() - ok;
    println!("finished: {ok} ok, {failed} failed");
    if failed > 0 {
        return Err(format!("{failed} file(s) failed"));
    }
    Ok(())
}

/// `install-ffmpeg`: list the options, or run the chosen one.
fn run_install(method: Option<&str>) -> Result<(), String> {
    if ffmpeg::is_available() && method.is_none() {
        println!("ffmpeg ya está instalado y funcionando.");
        return Ok(());
    }

    let options = install::options();
    let Some(method) = method else {
        println!("ffmpeg no está instalado. Formas de instalarlo en este sistema:\n");
        for o in &options {
            println!(
                "  --method {:<12} {}{}\n      {}",
                o.id,
                o.label,
                if o.recommended { "  (recomendado)" } else { "" },
                o.detail
            );
        }
        println!(
            "\nEjemplo: resizer-cli install-ffmpeg --method {}",
            options
                .iter()
                .find(|o| o.recommended)
                .map(|o| o.id)
                .unwrap_or("download")
        );
        return Ok(());
    };

    let parsed =
        install::Method::from_id(method).ok_or_else(|| format!("método desconocido '{method}'"))?;
    if !options.iter().any(|o| o.id == parsed.id()) {
        return Err(format!(
            "'{method}' no está disponible en este sistema (prueba: {})",
            options.iter().map(|o| o.id).collect::<Vec<_>>().join(", ")
        ));
    }

    install::install(parsed, |step| println!("  {step}"))?;
    let tools = ffmpeg::find_tools(None)
        .map_err(|_| "la instalación terminó pero ffmpeg sigue sin responder".to_string())?;
    println!("Listo: {}", ffmpeg::version(&tools));
    Ok(())
}

fn human(b: u64) -> String {
    if b >= 1024 * 1024 {
        format!("{:.1}MB", b as f64 / 1048576.0)
    } else {
        format!("{}KB", b / 1024)
    }
}
