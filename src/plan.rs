//! Pure planning logic: given source media info + user settings, decide the
//! exact output dimensions, crop, and encoder rate control. No I/O here so
//! everything is unit-testable.

use serde::{Deserialize, Serialize};

/// Aspect-ratio treatment for the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Ratio {
    /// Center-crop to 4:5 (the kamiru.art work-grid "hover" cards).
    Hover45,
    /// Center-crop to 1:1.
    Square,
    /// Center-crop to 16:9.
    Landscape169,
    /// Keep the source aspect ratio, only scale down.
    #[default]
    Original,
}

impl Ratio {
    /// Target aspect as (w, h), or None to keep the source ratio.
    pub fn target(self) -> Option<(u32, u32)> {
        match self {
            Ratio::Hover45 => Some((4, 5)),
            Ratio::Square => Some((1, 1)),
            Ratio::Landscape169 => Some((16, 9)),
            Ratio::Original => None,
        }
    }
}

/// Output image format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ImageFormat {
    /// Keep the source format (falls back to jpg for exotic inputs).
    #[default]
    Keep,
    Jpg,
    Webp,
    Png,
}

/// Everything the user can tune. Shared by the CLI and the GUI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub ratio: Ratio,
    /// Bounding box the output must fit inside (after any crop).
    pub max_width: u32,
    pub max_height: u32,
    /// Auto-tune: hard cap on the output file size, in megabytes.
    /// When set, videos use two-pass ABR sized to hit this; images walk the
    /// quality ladder down until they fit.
    pub max_mb: Option<f64>,
    /// x264 CRF used when `max_mb` is not set (18 = near lossless, 32 = tiny).
    pub crf: u8,
    /// Cap the output frame rate (only applied when the source is higher).
    pub fps_cap: Option<f64>,
    /// Drop the audio track entirely (hover videos autoplay muted anyway).
    pub strip_audio: bool,
    /// x264 speed/efficiency trade-off.
    pub x264_preset: String,
    pub image_format: ImageFormat,
    /// Image quality 1..=100 used when `max_mb` is not set.
    pub image_quality: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            ratio: Ratio::Original,
            max_width: 1920,
            max_height: 1920,
            max_mb: None,
            crf: 23,
            fps_cap: Some(30.0),
            strip_audio: false,
            x264_preset: "medium".into(),
            image_format: ImageFormat::Keep,
            image_quality: 85,
        }
    }
}

impl Settings {
    /// Defaults tuned for the kamiru.art hover cards: 4:5 crop, muted,
    /// capped to 10 MB at up to 1080x1350.
    pub fn hover() -> Self {
        Settings {
            ratio: Ratio::Hover45,
            max_width: 1080,
            max_height: 1350,
            max_mb: Some(10.0),
            strip_audio: true,
            ..Settings::default()
        }
    }

    pub fn preset(name: &str) -> Option<Self> {
        match name {
            "hover" => Some(Self::hover()),
            "square" => Some(Settings {
                ratio: Ratio::Square,
                max_width: 1080,
                max_height: 1080,
                ..Settings::default()
            }),
            "landscape" => Some(Settings {
                ratio: Ratio::Landscape169,
                max_width: 1920,
                max_height: 1080,
                ..Settings::default()
            }),
            "original" => Some(Settings::default()),
            _ => None,
        }
    }
}

/// What we learned about the source via ffprobe.
#[derive(Debug, Clone, Default, Serialize)]
pub struct MediaInfo {
    pub width: u32,
    pub height: u32,
    /// Display rotation in degrees (from the container's display matrix).
    pub rotation: i32,
    pub duration_s: f64,
    pub fps: f64,
    pub has_audio: bool,
    pub is_video: bool,
    pub size_bytes: u64,
}

impl MediaInfo {
    /// Width/height as displayed (i.e. after rotation is applied).
    pub fn display_dims(&self) -> (u32, u32) {
        if self.rotation.rem_euclid(180) == 90 {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        }
    }
}

/// Rate control for the video encoder.
#[derive(Debug, Clone, PartialEq)]
pub enum RateControl {
    Crf(u8),
    /// Two-pass ABR at this video bitrate (kbit/s), sized for a byte target.
    TwoPass {
        video_kbps: u32,
    },
}

/// A fully resolved encode plan for one file.
#[derive(Debug, Clone)]
pub struct EncodePlan {
    /// Centered crop applied first, in source display pixels. None = no crop.
    pub crop: Option<(u32, u32)>,
    /// Final output dimensions (even, >= 2).
    pub out_w: u32,
    pub out_h: u32,
    /// Output fps override (only when capping below the source rate).
    pub fps: Option<f64>,
    pub rate: RateControl,
    /// AAC audio bitrate in kbit/s; None = strip audio (or no audio track).
    pub audio_kbps: Option<u32>,
}

/// Below ~this many bits per pixel per frame, x264 output turns to mush;
/// auto-tune downscales instead of undershooting it.
const MIN_BITS_PER_PIXEL: f64 = 0.045;
/// Above ~this many bits per pixel per frame, extra bitrate is invisible for
/// web playback; the size budget is a *cap*, not a target to fill, so short
/// clips are not inflated up to it.
const MAX_BITS_PER_PIXEL: f64 = 0.15;
/// Never plan a video bitrate below this, no matter what.
const MIN_VIDEO_KBPS: u32 = 120;
/// Fraction of the size budget given to the A/V streams (rest = mux overhead).
const CONTAINER_MARGIN: f64 = 0.96;
const AUDIO_KBPS: u32 = 96;

fn even(n: u32) -> u32 {
    (n & !1).max(2)
}

/// Centered crop of (w, h) to the target aspect `aw:ah`, in source pixels.
pub fn crop_to_ratio(w: u32, h: u32, aw: u32, ah: u32) -> (u32, u32) {
    let (w, h, aw, ah) = (w as u64, h as u64, aw as u64, ah as u64);
    // Try full height first: cw = h * aw / ah. If that overflows the width,
    // keep full width and shrink height instead.
    let cw = (h * aw) / ah;
    if cw <= w {
        (cw.max(1) as u32, h as u32)
    } else {
        let ch = (w * ah) / aw;
        (w as u32, ch.max(1) as u32)
    }
}

/// Scale (w, h) down (never up) to fit inside (max_w, max_h), keeping aspect.
pub fn fit_within(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if w <= max_w && h <= max_h {
        return (w, h);
    }
    let s = f64::min(max_w as f64 / w as f64, max_h as f64 / h as f64);
    let fw = ((w as f64 * s).floor() as u32).max(1);
    let fh = ((h as f64 * s).floor() as u32).max(1);
    (fw, fh)
}

/// Build the encode plan for one source file.
pub fn plan_video(info: &MediaInfo, s: &Settings) -> EncodePlan {
    let (dw, dh) = info.display_dims();
    let (dw, dh) = (dw.max(2), dh.max(2));

    // 1. Crop to the requested aspect ratio (centered).
    let (crop, cw, ch) = match s.ratio.target() {
        Some((aw, ah)) => {
            let (cw, ch) = crop_to_ratio(dw, dh, aw, ah);
            if cw == dw && ch == dh {
                (None, dw, dh)
            } else {
                (Some((cw, ch)), cw, ch)
            }
        }
        None => (None, dw, dh),
    };

    // 2. Fit inside the resolution box; make dimensions even for h264 now so
    //    the rate-control math below sees the real output size.
    let (mut ow, mut oh) = fit_within(cw, ch, s.max_width.max(2), s.max_height.max(2));
    (ow, oh) = (even(ow), even(oh));

    // 3. Frame rate cap.
    let fps_out = match s.fps_cap {
        Some(cap) if info.fps > cap + 0.01 => cap,
        _ => info.fps,
    };
    let fps_used = if fps_out > 0.0 { fps_out } else { 30.0 };

    // 4. Rate control.
    let keep_audio = info.has_audio && !s.strip_audio;
    let rate = match s.max_mb {
        Some(mb) if info.duration_s > 0.05 => {
            let budget_bits = mb.max(0.1) * 1024.0 * 1024.0 * 8.0 * CONTAINER_MARGIN;
            let audio_bits = if keep_audio {
                AUDIO_KBPS as f64 * 1000.0 * info.duration_s
            } else {
                0.0
            };
            let mut video_kbps =
                ((budget_bits - audio_bits) / info.duration_s / 1000.0).floor() as i64;
            video_kbps = video_kbps.max(MIN_VIDEO_KBPS as i64);

            // If the budget is too thin for these dimensions, shrink the frame
            // until the bits-per-pixel floor is respected again.
            let needed_kbps =
                |w: u32, h: u32| (w as f64 * h as f64 * fps_used * MIN_BITS_PER_PIXEL) / 1000.0;
            if (video_kbps as f64) < needed_kbps(ow, oh) {
                let shrink = ((video_kbps as f64) / needed_kbps(ow, oh)).sqrt();
                ow = even(((ow as f64 * shrink).floor() as u32).max(2));
                oh = even(((oh as f64 * shrink).floor() as u32).max(2));
            }
            // ...and don't waste bits either: quality plateaus past the
            // ceiling, so short clips come out well under the size cap.
            let ceiling_kbps =
                ((ow as f64 * oh as f64 * fps_used * MAX_BITS_PER_PIXEL) / 1000.0).ceil() as i64;
            video_kbps = video_kbps.min(ceiling_kbps.max(MIN_VIDEO_KBPS as i64));
            RateControl::TwoPass {
                video_kbps: video_kbps as u32,
            }
        }
        _ => RateControl::Crf(s.crf.clamp(0, 51)),
    };

    let fps = if fps_out + 0.01 < info.fps {
        Some(fps_out)
    } else {
        None
    };

    EncodePlan {
        crop: crop.map(|(w, h)| (even(w), even(h))),
        out_w: even(ow),
        out_h: even(oh),
        fps,
        rate,
        audio_kbps: keep_audio.then_some(AUDIO_KBPS),
    }
}

/// The ffmpeg `-vf` filter chain for a plan. Crop first (in source display
/// pixels), then scale to the exact output size.
pub fn filter_chain(plan: &EncodePlan) -> String {
    let mut parts = Vec::new();
    if let Some((cw, ch)) = plan.crop {
        parts.push(format!("crop={cw}:{ch}"));
    }
    parts.push(format!("scale={}:{}:flags=lanczos", plan.out_w, plan.out_h));
    parts.push("setsar=1".to_string());
    parts.join(",")
}

/// Quality ladder used when auto-tuning an image to a byte budget.
/// Values are in each encoder's own scale (mjpeg 2..31 lower=better,
/// libwebp 0..100 higher=better, jpeg-style 1..100 for our UI slider).
pub fn image_quality_ladder(format: ImageFormat) -> Vec<u32> {
    match format {
        ImageFormat::Webp => vec![90, 82, 75, 65, 55, 45, 35, 25],
        // mjpeg -q:v scale (2 = best).
        _ => vec![2, 3, 5, 7, 10, 14, 18, 24, 31],
    }
}

/// Map a 1..=100 UI quality slider to the mjpeg -q:v scale (2 best .. 31 worst).
pub fn jpeg_q_from_percent(q: u8) -> u32 {
    let q = q.clamp(1, 100) as f64;
    (2.0 + (100.0 - q) * 29.0 / 99.0).round() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(w: u32, h: u32, dur: f64) -> MediaInfo {
        MediaInfo {
            width: w,
            height: h,
            rotation: 0,
            duration_s: dur,
            fps: 30.0,
            has_audio: true,
            is_video: true,
            size_bytes: 0,
        }
    }

    #[test]
    fn crop_lands_exactly_on_ratio() {
        // 1920x1080 -> 4:5 must crop the width: 1080 * 4/5 = 864.
        assert_eq!(crop_to_ratio(1920, 1080, 4, 5), (864, 1080));
        // Portrait source taller than 4:5 crops the height instead.
        assert_eq!(crop_to_ratio(1080, 1920, 4, 5), (1080, 1350));
        // Already square stays untouched.
        assert_eq!(crop_to_ratio(500, 500, 1, 1), (500, 500));
    }

    #[test]
    fn fit_never_upscales() {
        assert_eq!(fit_within(640, 480, 1920, 1920), (640, 480));
        assert_eq!(fit_within(3840, 2160, 1920, 1080), (1920, 1080));
        // Height-bound fit.
        assert_eq!(fit_within(1080, 2400, 1080, 1350), (607, 1350));
    }

    #[test]
    fn dimensions_are_always_even() {
        let s = Settings {
            max_width: 607,
            max_height: 1351,
            ..Settings::default()
        };
        let p = plan_video(&info(607, 1351, 5.0), &s);
        assert_eq!(p.out_w % 2, 0);
        assert_eq!(p.out_h % 2, 0);
        assert!(p.out_w >= 2 && p.out_h >= 2);
    }

    #[test]
    fn hover_preset_crops_landscape_source_to_4_5() {
        let s = Settings::hover();
        let p = plan_video(&info(1920, 1080, 8.0), &s);
        assert_eq!(p.crop, Some((864, 1080)));
        assert_eq!((p.out_w, p.out_h), (864, 1080));
        // Hover strips audio by default.
        assert_eq!(p.audio_kbps, None);
        // Aspect is 4:5.
        assert!((p.out_w as f64 / p.out_h as f64 - 0.8).abs() < 0.01);
    }

    #[test]
    fn rotated_source_uses_display_dimensions() {
        let mut i = info(1920, 1080, 5.0);
        i.rotation = 90; // phone video: displayed as 1080x1920
        assert_eq!(i.display_dims(), (1080, 1920));
        let s = Settings {
            ratio: Ratio::Original,
            max_width: 1080,
            max_height: 1920,
            ..Settings::default()
        };
        let p = plan_video(&i, &s);
        assert_eq!((p.out_w, p.out_h), (1080, 1920));
    }

    #[test]
    fn autotune_hits_the_bitrate_math() {
        // 10 s video, 8 MB budget, audio kept at 96 kbps:
        // bits = 8 * 1024^2 * 8 * 0.96 = 64424509.44 * 0.96...
        let s = Settings {
            max_mb: Some(8.0),
            strip_audio: false,
            ..Settings::default()
        };
        let p = plan_video(&info(1280, 720, 10.0), &s);
        match p.rate {
            RateControl::TwoPass { video_kbps } => {
                let budget: f64 = 8.0 * 1024.0 * 1024.0 * 8.0 * 0.96;
                let from_budget = ((budget - 96_000.0 * 10.0) / 10.0 / 1000.0).floor();
                let ceiling = (1280.0 * 720.0 * 30.0 * 0.15 / 1000.0_f64).ceil();
                assert_eq!(video_kbps as f64, from_budget.min(ceiling));
            }
            other => panic!("expected two-pass, got {other:?}"),
        }
        assert_eq!(p.audio_kbps, Some(96));
    }

    #[test]
    fn autotune_does_not_inflate_short_clips() {
        // A 3 s clip with a 10 MB cap: filling the cap would mean ~28 Mbps,
        // which is absurd. The bits-per-pixel ceiling must kick in.
        let s = Settings {
            max_mb: Some(10.0),
            strip_audio: true,
            max_width: 1080,
            max_height: 1350,
            ..Settings::default()
        };
        let p = plan_video(&info(1080, 1920, 3.0), &s);
        match p.rate {
            RateControl::TwoPass { video_kbps } => {
                let ceiling =
                    (p.out_w as f64 * p.out_h as f64 * 30.0 * 0.15 / 1000.0).ceil() as u32;
                assert!(
                    video_kbps <= ceiling,
                    "bitrate {video_kbps} should be capped near {ceiling}"
                );
                // Sanity: implied size is far below the 10 MB cap.
                let implied_mb = video_kbps as f64 * 1000.0 * 3.0 / 8.0 / 1024.0 / 1024.0;
                assert!(implied_mb < 5.0, "implied {implied_mb:.1} MB");
            }
            other => panic!("expected two-pass, got {other:?}"),
        }
    }

    #[test]
    fn autotune_downscales_when_budget_is_tiny() {
        // 1 MB for 60 s of 1080p is hopeless: the planner must shrink the frame.
        let s = Settings {
            max_mb: Some(1.0),
            strip_audio: true,
            max_width: 1920,
            max_height: 1920,
            ..Settings::default()
        };
        let p = plan_video(&info(1920, 1080, 60.0), &s);
        assert!(
            p.out_w < 1920 && p.out_h < 1080,
            "should downscale, got {}x{}",
            p.out_w,
            p.out_h
        );
        match p.rate {
            RateControl::TwoPass { video_kbps } => assert!(video_kbps >= MIN_VIDEO_KBPS),
            _ => panic!("expected two-pass"),
        }
    }

    #[test]
    fn no_budget_means_crf() {
        let s = Settings {
            max_mb: None,
            crf: 21,
            ..Settings::default()
        };
        let p = plan_video(&info(640, 480, 3.0), &s);
        assert_eq!(p.rate, RateControl::Crf(21));
    }

    #[test]
    fn fps_cap_only_lowers() {
        let mut i = info(1280, 720, 5.0);
        i.fps = 60.0;
        let s = Settings {
            fps_cap: Some(30.0),
            ..Settings::default()
        };
        assert_eq!(plan_video(&i, &s).fps, Some(30.0));
        i.fps = 24.0;
        assert_eq!(plan_video(&i, &s).fps, None);
    }

    #[test]
    fn filter_chain_orders_crop_then_scale() {
        let p = EncodePlan {
            crop: Some((864, 1080)),
            out_w: 864,
            out_h: 1080,
            fps: None,
            rate: RateControl::Crf(23),
            audio_kbps: None,
        };
        assert_eq!(
            filter_chain(&p),
            "crop=864:1080,scale=864:1080:flags=lanczos,setsar=1"
        );
    }

    #[test]
    fn jpeg_quality_mapping_covers_the_scale() {
        assert_eq!(jpeg_q_from_percent(100), 2);
        assert_eq!(jpeg_q_from_percent(1), 31);
        let mid = jpeg_q_from_percent(50);
        assert!((2..=31).contains(&mid));
    }

    #[test]
    fn presets_resolve() {
        for name in ["hover", "square", "landscape", "original"] {
            assert!(Settings::preset(name).is_some(), "missing preset {name}");
        }
        assert!(Settings::preset("nope").is_none());
        assert_eq!(Settings::preset("hover").unwrap().ratio, Ratio::Hover45);
    }
}
