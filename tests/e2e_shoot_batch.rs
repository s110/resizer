//! End-to-end scenario: a real "shoot folder" goes through the hover preset
//! twice, driving the real `resizer-cli` binary against real ffmpeg.
//!
//! The folder mixes everything a kamiru.art upload tends to contain: two
//! clips with the same name (`IMG_0001.MOV` + an edited `IMG_0001.mp4`), a
//! portrait phone video stored sideways with a rotation flag, a phone photo
//! with EXIF orientation, a 16-bit PNG with transparency, an animated GIF
//! with odd dimensions, an extreme panorama, a heavy photo in a subfolder,
//! a corrupt file in the middle, plus a text file and a hidden file that must
//! be ignored. The same command is then re-run on the same output folder.
//!
//! Every check is judged against independent references (ffprobe, decoded
//! pixels, PSNR against a crop made straight from the source), never against
//! resizer's own planner. The run writes `artifacts/e2e/shoot_batch.json`
//! with inputs, outputs, content and decoded-pixel hashes and a verdict per
//! check. Checks that expose a known bug are kept and marked `known_bug`:
//! they are expected to fail, and the test fails if one starts passing so the
//! marker gets removed together with the fix.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const BUDGET_MB: &str = "0.5";
const BUDGET_BYTES: u64 = 512 * 1024;
/// Media files in the scenario (notes.txt and .hidden.jpg are not counted).
const MEDIA_INPUTS: usize = 9;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_resizer-cli")
}

fn ffmpeg_available() -> bool {
    ["ffmpeg", "ffprobe"].iter().all(|b| {
        Command::new(b)
            .arg("-version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}

fn ffmpeg(args: &[&str]) {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-v", "error", "-y"])
        .args(args)
        .output()
        .expect("run ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn p(path: &Path) -> &str {
    path.to_str().expect("utf-8 path")
}

fn file_sha256(path: &Path) -> String {
    let bytes = std::fs::read(path).expect("read file");
    format!("{:x}", Sha256::digest(bytes))
}

/// SHA-256 of every decoded video frame, as computed by ffmpeg's hash muxer.
/// Independent of container bytes, so it pins what a viewer actually sees.
fn decoded_sha256(path: &Path) -> String {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-v", "error", "-i", p(path)])
        .args(["-map", "0:v:0", "-f", "hash", "-hash", "sha256", "-"])
        .output()
        .expect("run ffmpeg hash");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .trim_start_matches("SHA256=")
        .to_string()
}

fn ffprobe(path: &Path) -> Value {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-print_format", "json"])
        .args(["-show_streams", "-show_format", p(path)])
        .output()
        .expect("run ffprobe");
    serde_json::from_slice(&out.stdout).unwrap_or(Value::Null)
}

fn video_stream(probe: &Value) -> Value {
    probe["streams"]
        .as_array()
        .and_then(|s| s.iter().find(|s| s["codec_type"] == "video").cloned())
        .unwrap_or(Value::Null)
}

fn has_audio(probe: &Value) -> bool {
    probe["streams"]
        .as_array()
        .map(|s| s.iter().any(|s| s["codec_type"] == "audio"))
        .unwrap_or(false)
}

fn dims(v: &Value) -> (u64, u64) {
    (
        v["width"].as_u64().unwrap_or(0),
        v["height"].as_u64().unwrap_or(0),
    )
}

fn rate(v: &Value) -> f64 {
    let s = v["avg_frame_rate"].as_str().unwrap_or("0/1");
    let (n, d) = s.split_once('/').unwrap_or(("0", "1"));
    let (n, d): (f64, f64) = (n.parse().unwrap_or(0.0), d.parse().unwrap_or(1.0));
    if d == 0.0 {
        0.0
    } else {
        n / d
    }
}

fn rotation(v: &Value) -> i64 {
    v["side_data_list"]
        .as_array()
        .and_then(|sd| sd.iter().find_map(|d| d["rotation"].as_i64()))
        .unwrap_or(0)
}

/// PSNR (dB) of the first frame of `out`, scaled to `reference`'s size,
/// against `reference`. Scaling first lets a wrongly sized output still be
/// judged on content (upright vs. sideways). Returns 0 when it can't compare.
fn psnr_first_frame(out: &Path, reference: &Path, w: u64, h: u64) -> f64 {
    psnr(out, reference, &format!("trim=end_frame=1,scale={w}:{h},"))
}

/// Average PSNR (dB) of `a` (after the `pre_a` filters) against `b`, over
/// all frames. 99 stands for identical; 0 means the two could not be compared.
fn psnr(a: &Path, b: &Path, pre_a: &str) -> f64 {
    let graph = format!("[0:v]{pre_a}format=rgb24[a];[1:v]format=rgb24[b];[a][b]psnr");
    let res = Command::new("ffmpeg")
        .args(["-hide_banner", "-i", p(a), "-i", p(b)])
        .args(["-lavfi", &graph, "-f", "null", "-"])
        .output()
        .expect("run ffmpeg psnr");
    let err = String::from_utf8_lossy(&res.stderr);
    if !res.status.success() || !err.contains("average:") {
        return 0.0;
    }
    err.rsplit("average:")
        .next()
        .and_then(|s| s.split_whitespace().next())
        .map(|s| {
            if s == "inf" {
                99.0
            } else {
                s.parse().unwrap_or(0.0)
            }
        })
        .unwrap_or(0.0)
}

/// 8-bit alpha samples at (x, y) positions of an image.
fn alpha_at(path: &Path, w: u64, points: &[(u64, u64)]) -> Vec<u8> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-v", "error", "-i", p(path)])
        .args(["-vf", "alphaextract,format=gray", "-f", "rawvideo", "-"])
        .output()
        .expect("run ffmpeg alphaextract");
    points
        .iter()
        .map(|(x, y)| out.stdout.get((y * w + x) as usize).copied().unwrap_or(255))
        .collect()
}

/// True when the MP4's `moov` atom precedes `mdat` (streamable on the web).
fn is_faststart(path: &Path) -> bool {
    let data = std::fs::read(path).unwrap_or_default();
    let mut i = 0usize;
    let (mut moov, mut mdat) = (None, None);
    while i + 8 <= data.len() {
        let mut size = u32::from_be_bytes(data[i..i + 4].try_into().unwrap()) as usize;
        let kind = &data[i + 4..i + 8];
        if size == 1 && i + 16 <= data.len() {
            size = u64::from_be_bytes(data[i + 8..i + 16].try_into().unwrap()) as usize;
        }
        if kind == b"moov" && moov.is_none() {
            moov = Some(i);
        }
        if kind == b"mdat" && mdat.is_none() {
            mdat = Some(i);
        }
        if size < 8 {
            break;
        }
        i += size;
    }
    matches!((moov, mdat), (Some(a), Some(b)) if a < b)
}

/// Deterministic garbage for the corrupt file (no RNG, same bytes every run).
fn garbage(len: usize) -> Vec<u8> {
    let mut x: u32 = 0x1234_5678;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x >> 24) as u8
        })
        .collect()
}

/// Insert an EXIF APP1 segment with the given Orientation right after SOI.
fn add_exif_orientation(jpeg: &Path, orientation: u16) {
    let data = std::fs::read(jpeg).unwrap();
    assert_eq!(&data[..2], &[0xFF, 0xD8], "not a JPEG");
    let mut tiff = b"II*\0".to_vec();
    tiff.extend(8u32.to_le_bytes()); // IFD0 offset
    tiff.extend(1u16.to_le_bytes()); // one entry
    tiff.extend(0x0112u16.to_le_bytes()); // Orientation
    tiff.extend(3u16.to_le_bytes()); // SHORT
    tiff.extend(1u32.to_le_bytes()); // count
    tiff.extend(orientation.to_le_bytes());
    tiff.extend(0u16.to_le_bytes()); // padding of the 4-byte value slot
    tiff.extend(0u32.to_le_bytes()); // no next IFD
    let mut app1 = b"Exif\0\0".to_vec();
    app1.extend(tiff);
    let mut out = vec![0xFF, 0xD8, 0xFF, 0xE1];
    out.extend(((app1.len() + 2) as u16).to_be_bytes());
    out.extend(app1);
    out.extend(&data[2..]);
    std::fs::write(jpeg, out).unwrap();
}

/// Build the shoot folder plus the independent reference frames.
#[rustfmt::skip]
fn build_inputs(shoot: &Path, refs: &Path) {
    std::fs::create_dir_all(shoot.join("extra")).unwrap();
    std::fs::create_dir_all(refs).unwrap();
    let s = |name: &str| shoot.join(name);
    let r = |name: &str| refs.join(name);

    // Same stem, two clips: the camera original and an edited export. The
    // .MOV is the short one so the finishing order is stable.
    ffmpeg(&[
        "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=30:duration=3",
        "-c:v", "libx264", "-pix_fmt", "yuv420p", p(&s("IMG_0001.MOV")),
    ]);
    ffmpeg(&[
        "-f", "lavfi", "-i", "testsrc2=size=1280x720:rate=30:duration=6",
        "-f", "lavfi", "-i", "sine=frequency=440:duration=6",
        "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest",
        p(&s("IMG_0001.mp4")),
    ]);

    // Portrait 60 fps phone video, stored sideways with a rotation flag.
    let upright = r("phone-upright.mp4");
    ffmpeg(&[
        "-f", "lavfi", "-i", "testsrc2=size=360x640:rate=60:duration=3",
        "-c:v", "libx264", "-pix_fmt", "yuv420p", p(&upright),
    ]);
    let sideways = r("phone-sideways.mp4");
    ffmpeg(&[
        "-i", p(&upright), "-vf", "transpose=2", "-c:v", "libx264", "-pix_fmt", "yuv420p",
        p(&sideways),
    ]);
    let flagged = Command::new("ffmpeg")
        .args(["-hide_banner", "-v", "error", "-y", "-display_rotation", "-90"])
        .args(["-i", p(&sideways), "-c", "copy", p(&s("phone.mp4"))])
        .status()
        .map(|st| st.success())
        .unwrap_or(false);
    if !flagged {
        // ffmpeg builds without -display_rotation still honour the old tag.
        ffmpeg(&[
            "-i", p(&sideways), "-c", "copy", "-metadata:s:v:0", "rotate=90",
            p(&s("phone.mp4")),
        ]);
    }
    let rot = rotation(&video_stream(&ffprobe(&s("phone.mp4"))));
    assert!(rot.abs() == 90, "could not tag phone.mp4 as rotated (got {rot})");
    // Reference: the centred 4:5 crop of the upright first frame.
    ffmpeg(&[
        "-i", p(&upright), "-frames:v", "1", "-vf", "crop=360:450", p(&r("phone-ref.png")),
    ]);

    // Phone photo: upright 600x800, stored as 800x600 with EXIF Orientation 6.
    ffmpeg(&[
        "-f", "lavfi", "-i", "testsrc2=size=600x800:rate=1:duration=1", "-frames:v", "1",
        p(&r("photo-upright.png")),
    ]);
    ffmpeg(&[
        "-i", p(&r("photo-upright.png")), "-vf", "transpose=2", "-q:v", "2",
        p(&s("photo.jpg")),
    ]);
    add_exif_orientation(&s("photo.jpg"), 6);
    ffmpeg(&[
        "-i", p(&r("photo-upright.png")), "-vf", "crop=600:750", p(&r("photo-ref.png")),
    ]);

    // 16-bit RGBA logo, left half fully transparent.
    ffmpeg(&[
        "-f", "lavfi", "-i",
        "testsrc2=size=900x700:rate=1:duration=1,format=rgba,\
         geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='if(lt(X,450),0,255)'",
        "-frames:v", "1", "-pix_fmt", "rgba64be", p(&s("logo.png")),
    ]);

    // Animated GIF with odd dimensions.
    ffmpeg(&[
        "-f", "lavfi", "-i", "testsrc2=size=333x199:rate=12:duration=2",
        "-filter_complex", "split[a][b];[a]palettegen[p];[b][p]paletteuse",
        p(&s("anim.gif")),
    ]);

    // Extreme panorama and its centred 4:5 reference.
    ffmpeg(&[
        "-f", "lavfi", "-i", "testsrc2=size=4000x90:rate=1:duration=1", "-frames:v", "1",
        "-q:v", "2", p(&s("pano.jpg")),
    ]);
    ffmpeg(&["-i", p(&s("pano.jpg")), "-vf", "crop=72:90", p(&r("pano-ref.png"))]);

    // Heavy, noisy photo in a subfolder: needs the quality ladder to fit.
    ffmpeg(&[
        "-f", "lavfi", "-i", "testsrc2=size=1500x2000:rate=1:duration=1,noise=alls=25:allf=t",
        "-frames:v", "1", "-q:v", "2", p(&s("extra/detail.jpg")),
    ]);

    // A corrupt "video" in the middle of the batch, and files to ignore.
    std::fs::write(s("broken.mp4"), garbage(64 * 1024)).unwrap();
    std::fs::write(s("notes.txt"), "client notes, not media").unwrap();
    std::fs::copy(s("pano.jpg"), s(".hidden.jpg")).unwrap();
}

struct Run {
    exit_code: i32,
    stdout: String,
}

impl Run {
    /// The "converting N file(s)" and "finished" lines, without temp paths.
    fn summary(&self) -> Vec<String> {
        self.stdout
            .lines()
            .filter(|l| l.starts_with("converting") || l.starts_with("finished"))
            .map(|l| l.split(" -> ").next().unwrap_or(l).to_string())
            .collect()
    }
}

fn run_batch(shoot: &Path) -> Run {
    let out = Command::new(bin())
        .arg("convert")
        .arg(shoot)
        .args(["--preset", "hover", "--max-mb", BUDGET_MB, "--recursive"])
        .args(["--jobs", "3", "--speed", "veryfast"])
        .output()
        .expect("run resizer-cli");
    Run {
        exit_code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
    }
}

/// One output file as seen from outside resizer.
struct Output {
    path: PathBuf,
    bytes: u64,
    sha256: String,
    decoded_sha256: String,
    probe: Value,
}

impl Output {
    fn is_video(&self) -> bool {
        self.path.extension().is_some_and(|e| e == "mp4")
    }

    /// Artifact entry. x264 two-pass with VBV and frame threads is not
    /// bit-reproducible, so videos record only properties that are stable
    /// run to run; images (bit-exact) also record size and hashes.
    fn describe(&self) -> Value {
        let v = video_stream(&self.probe);
        let mut d = json!({
            "codec": v["codec_name"], "pix_fmt": v["pix_fmt"],
            "width": v["width"], "height": v["height"],
            "within_budget": self.bytes <= BUDGET_BYTES,
        });
        if self.is_video() {
            d["frames"] = v["nb_frames"].clone();
            d["fps"] = json!(rate(&v));
            d["audio"] = json!(has_audio(&self.probe));
            d["faststart"] = json!(is_faststart(&self.path));
        } else {
            d["bytes"] = json!(self.bytes);
            d["sha256"] = json!(self.sha256);
            d["decoded_sha256"] = json!(self.decoded_sha256);
        }
        d
    }
}

fn snapshot(dir: &Path) -> BTreeMap<String, Output> {
    let mut map = BTreeMap::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = e.path();
        map.insert(
            e.file_name().to_string_lossy().into_owned(),
            Output {
                bytes: e.metadata().map(|m| m.len()).unwrap_or(0),
                sha256: file_sha256(&path),
                decoded_sha256: decoded_sha256(&path),
                probe: ffprobe(&path),
                path,
            },
        );
    }
    map
}

/// "IMG_0001-web-2.mp4" -> ("IMG_0001", "mp4"): the family a file belongs to.
fn family(name: &str) -> (String, String) {
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    let base = stem.split("-web").next().unwrap_or(stem);
    (base.to_string(), ext.to_string())
}

/// Same picture: bit-exact for images, visually identical for videos.
fn same_content(a: &Output, b: &Output) -> bool {
    if a.is_video() {
        dims(&video_stream(&a.probe)) == dims(&video_stream(&b.probe))
            && psnr(&a.path, &b.path, "") >= 30.0
    } else {
        a.decoded_sha256 == b.decoded_sha256
    }
}

struct Checks(Vec<Value>);

impl Checks {
    fn add(&mut self, id: &str, what: &str, ok: bool, detail: Value, known_bug: Option<&str>) {
        let status = match (ok, known_bug) {
            (true, None) => "pass",
            (false, None) => "fail",
            (false, Some(_)) => "known_failing",
            (true, Some(_)) => "unexpected_pass",
        };
        self.0.push(json!({
            "id": id,
            "what": what,
            "status": status,
            "known_bug": known_bug,
            "detail": detail,
        }));
    }
}

#[test]
fn shoot_folder_hover_batch_twice() {
    if !ffmpeg_available() {
        assert!(
            std::env::var_os("CI").is_none(),
            "ffmpeg/ffprobe missing in CI: the E2E must not silently skip"
        );
        eprintln!("SKIP: ffmpeg not installed");
        return;
    }

    let root = std::env::temp_dir().join(format!("resizer-e2e-shoot-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let shoot = root.join("shoot");
    let refs = root.join("refs");
    build_inputs(&shoot, &refs);
    let out_dir = shoot.join("resized");

    let mut inputs = BTreeMap::new();
    for rel in [
        "IMG_0001.MOV",
        "IMG_0001.mp4",
        "anim.gif",
        "broken.mp4",
        "extra/detail.jpg",
        "logo.png",
        "pano.jpg",
        "phone.mp4",
        "photo.jpg",
        "notes.txt",
        ".hidden.jpg",
    ] {
        let path = shoot.join(rel);
        inputs.insert(
            rel.to_string(),
            json!({
                "bytes": std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
                "sha256": file_sha256(&path),
            }),
        );
    }

    let mut c = Checks(Vec::new());
    let ok_count = MEDIA_INPUTS - 1;
    let expected_summary = vec![
        format!("converting {MEDIA_INPUTS} file(s)"),
        format!("finished: {ok_count} ok, 1 failed"),
    ];

    // ---------------- run 1 ----------------
    let run1 = run_batch(&shoot);
    let snap1 = snapshot(&out_dir);
    let names1: Vec<&String> = snap1.keys().collect();

    c.add(
        "run1.exit_code",
        "a corrupt file makes the batch exit 1 instead of pretending success",
        run1.exit_code == 1,
        json!({"expected": 1, "actual": run1.exit_code}),
        None,
    );
    c.add(
        "run1.summary",
        "9 media files picked up (subfolder included, .txt and hidden file ignored); 8 ok, 1 failed",
        run1.summary() == expected_summary,
        json!({"expected": expected_summary, "actual": run1.summary()}),
        None,
    );
    c.add(
        "run1.corrupt_reported",
        "broken.mp4 is reported as FAIL and leaves no output behind",
        run1.stdout.contains("FAIL  broken.mp4") && !names1.iter().any(|k| k.starts_with("broken")),
        json!({"outputs": names1}),
        None,
    );
    c.add(
        "run1.ignored_files",
        "notes.txt and .hidden.jpg produce no output",
        !names1
            .iter()
            .any(|k| k.starts_with("notes") || k.starts_with(".hidden")),
        json!({"outputs": names1}),
        None,
    );
    c.add(
        "run1.one_output_per_success",
        "every input reported ok has its own output file (IMG_0001.MOV and IMG_0001.mp4 must not overwrite each other)",
        snap1.len() == ok_count,
        json!({"expected": ok_count, "actual": snap1.len(),
               "img_0001_outputs": names1.iter().filter(|k| k.starts_with("IMG_0001")).collect::<Vec<_>>()}),
        Some("BUG-1: parallel jobs with the same stem pick the same output name and clobber each other"),
    );

    let over: Vec<&String> = snap1
        .iter()
        .filter(|(_, o)| o.bytes > BUDGET_BYTES)
        .map(|(n, _)| n)
        .collect();
    c.add(
        "run1.budget",
        "every output fits the --max-mb 0.5 budget (two-pass videos, jpg quality ladder, png)",
        over.is_empty() && !snap1.is_empty(),
        json!({"budget_bytes": BUDGET_BYTES, "over_budget": over}),
        None,
    );

    for (name, o) in snap1.iter().filter(|(_, o)| o.is_video()) {
        let v = video_stream(&o.probe);
        let (w, h) = dims(&v);
        let ok = v["codec_name"] == "h264"
            && v["pix_fmt"] == "yuv420p"
            && w % 2 == 0
            && h % 2 == 0
            && h > 0
            && (w as f64 / h as f64 - 0.8).abs() <= 2.0 / h as f64
            && !has_audio(&o.probe)
            && rate(&v) <= 30.01
            && rotation(&v) == 0
            && is_faststart(&o.path);
        c.add(
            &format!("run1.hover_video.{name}"),
            "h264/yuv420p, even 4:5 frame, muted, <=30 fps, no rotation flag left, moov before mdat",
            ok,
            o.describe(),
            None,
        );
    }

    // Phone video: displayed portrait, capped from 60 to 30 fps, upright.
    let phone = out_dir.join("phone-web.mp4");
    let pv = video_stream(&ffprobe(&phone));
    let phone_psnr = psnr_first_frame(&phone, &refs.join("phone-ref.png"), 360, 450);
    c.add(
        "run1.phone_rotation",
        "rotated phone video comes out 360x450 at 30 fps, upright centre crop (PSNR >= 30 dB vs reference)",
        dims(&pv) == (360, 450) && (rate(&pv) - 30.0).abs() < 0.01 && phone_psnr >= 30.0,
        json!({"dims": dims(&pv), "fps": rate(&pv), "psnr_at_least_30db": phone_psnr >= 30.0}),
        None,
    );

    // Phone photo with EXIF orientation: should be treated like the video.
    let photo = out_dir.join("photo-web.jpg");
    let (pw, ph) = dims(&video_stream(&ffprobe(&photo)));
    let photo_psnr = psnr_first_frame(&photo, &refs.join("photo-ref.png"), 600, 750);
    c.add(
        "run1.photo_exif_orientation",
        "EXIF-rotated photo comes out 600x750, the upright full-width 4:5 crop (PSNR >= 30 dB)",
        (pw, ph) == (600, 750) && photo_psnr >= 30.0,
        json!({"expected_dims": [600, 750], "dims": [pw, ph], "psnr_db": photo_psnr}),
        Some(
            "BUG-2: probe ignores EXIF orientation, so the plan uses stored (sideways) dimensions",
        ),
    );

    // 16-bit transparent logo: alpha survives the crop and re-encode.
    let logo = out_dir.join("logo-web.png");
    let lv = video_stream(&ffprobe(&logo));
    let (lw, lh) = dims(&lv);
    let alpha = alpha_at(&logo, lw, &[(20, 350), (540, 350)]);
    c.add(
        "run1.logo_alpha",
        "PNG stays PNG with alpha, 560x700 centre crop: left edge transparent, right edge opaque",
        lv["codec_name"] == "png"
            && lv["pix_fmt"].as_str().unwrap_or("").contains('a')
            && (lw, lh) == (560, 700)
            && alpha.len() == 2
            && alpha[0] <= 5
            && alpha[1] >= 250,
        json!({"codec": lv["codec_name"], "pix_fmt": lv["pix_fmt"], "dims": [lw, lh],
               "alpha_samples": alpha}),
        None,
    );

    // Panorama: centred crop, not squashed.
    let pano = out_dir.join("pano-web.jpg");
    let pano_dims = dims(&video_stream(&ffprobe(&pano)));
    let pano_psnr = psnr_first_frame(&pano, &refs.join("pano-ref.png"), 72, 90);
    c.add(
        "run1.panorama",
        "4000x90 panorama becomes the 72x90 centre crop (PSNR >= 30 dB vs reference)",
        pano_dims == (72, 90) && pano_psnr >= 30.0,
        json!({"dims": pano_dims, "psnr_db": pano_psnr}),
        None,
    );

    // Animated GIF: becomes a real video, not a still.
    let gif_src = video_stream(&ffprobe(&shoot.join("anim.gif")));
    let gif_out = video_stream(&ffprobe(&out_dir.join("anim-web.mp4")));
    let frames: u64 = gif_out["nb_frames"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let src_h_even = dims(&gif_src).1 & !1;
    c.add(
        "run1.gif",
        "animated GIF with odd size becomes an h264 mp4 at full (even) height with all its frames",
        gif_out["codec_name"] == "h264" && dims(&gif_out).1 == src_h_even && frames >= 20,
        json!({"source_dims": dims(&gif_src), "dims": dims(&gif_out), "frames": frames}),
        None,
    );

    // Heavy photo from the subfolder: full hover resolution, ladder hit budget.
    let detail = out_dir.join("detail-web.jpg");
    let detail_dims = dims(&video_stream(&ffprobe(&detail)));
    c.add(
        "run1.detail",
        "noisy 1500x2000 photo from extra/ lands at 1080x1350 as jpg",
        detail_dims == (1080, 1350),
        json!({"dims": detail_dims}),
        None,
    );

    // ---------------- run 2: same command, same output folder ----------------
    let run2 = run_batch(&shoot);
    let snap2 = snapshot(&out_dir);
    let untouched = snap1
        .iter()
        .filter(|(n, o)| snap2.get(*n).is_some_and(|o2| o2.sha256 == o.sha256))
        .count();
    let new_files: Vec<(&String, &Output)> = snap2
        .iter()
        .filter(|(n, _)| !snap1.contains_key(*n))
        .collect();

    c.add(
        "run2.no_reingest",
        "re-run with --recursive picks up the same 9 inputs (resized/ is not re-ingested)",
        run2.exit_code == 1 && run2.summary() == expected_summary,
        json!({"exit_code": run2.exit_code, "expected": expected_summary, "actual": run2.summary()}),
        None,
    );
    c.add(
        "run2.no_clobber",
        "run 1 outputs are byte-identical after run 2, which writes only new files",
        untouched == snap1.len() && new_files.len() == snap1.len(),
        json!({"run1_untouched": untouched, "run1_outputs": snap1.len(),
               "run2_new_files": new_files.iter().map(|(n, _)| n).collect::<Vec<_>>()}),
        None,
    );
    let unmatched: Vec<&String> = snap1
        .iter()
        .filter(|(n, o)| {
            !new_files
                .iter()
                .any(|(n2, o2)| family(n) == family(n2) && same_content(o, o2))
        })
        .map(|(n, _)| n)
        .collect();
    c.add(
        "run2.reproducible",
        "each run 1 output has a run 2 copy with the same picture (images bit-exact, videos PSNR >= 30 dB, since x264 two-pass is not bit-exact)",
        unmatched.is_empty() && !snap1.is_empty(),
        json!({"without_matching_copy": unmatched}),
        None,
    );

    // ---------------- artifact ----------------
    let outputs = |snap: &BTreeMap<String, Output>| -> Value {
        snap.iter()
            .map(|(n, o)| (n.clone(), o.describe()))
            .collect::<serde_json::Map<_, _>>()
            .into()
    };
    let failed: Vec<&Value> =
        c.0.iter()
            .filter(|x| x["status"] == "fail" || x["status"] == "unexpected_pass")
            .collect();
    let version = Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| {
            let text = String::from_utf8_lossy(&o.stdout).into_owned();
            let first = text.lines().next().unwrap_or("");
            first.split(" Copyright").next().unwrap_or("").to_string()
        })
        .unwrap_or_default();
    let artifact = json!({
        "scenario": "shoot_batch",
        "command": format!("resizer-cli convert shoot --preset hover --max-mb {BUDGET_MB} --recursive --jobs 3 --speed veryfast (run twice)"),
        "ffmpeg": version,
        "os": std::env::consts::OS,
        "inputs": inputs,
        "run1": {"exit_code": run1.exit_code, "summary": run1.summary(), "outputs": outputs(&snap1)},
        "run2": {"exit_code": run2.exit_code, "summary": run2.summary(), "outputs": outputs(&snap2)},
        "checks": c.0,
        "result": if failed.is_empty() { "pass" } else { "fail" },
    });
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("artifacts/e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let artifact_path = dir.join("shoot_batch.json");
    let text = serde_json::to_string_pretty(&artifact).unwrap() + "\n";
    std::fs::write(&artifact_path, text).unwrap();

    if failed.is_empty() {
        let _ = std::fs::remove_dir_all(&root);
    }
    assert!(
        failed.is_empty(),
        "E2E checks failed (scenario kept in {}, artifact {}):\n{}\n--- run1 stdout ---\n{}",
        root.display(),
        artifact_path.display(),
        serde_json::to_string_pretty(&failed).unwrap(),
        run1.stdout
    );
}
