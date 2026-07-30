//! Local GUI: a tiny HTTP server on 127.0.0.1 serving the embedded single-page
//! interface plus a small JSON API. Files arrive either as browser uploads
//! (drag & drop) or as server-side folder paths (bulk mode).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tiny_http::{Header, Method, Request, Response, Server};

use crate::ffmpeg::{self, Tools};
use crate::jobs;
use crate::plan::{MediaInfo, Settings};

const UI_HTML: &str = include_str!("ui.html");
/// Cap browser uploads (the site itself refuses anything over 200 MB anyway,
/// and sources are reasonably below 4 GB).
const MAX_UPLOAD: u64 = 4 * 1024 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    Ready,
    Working,
    Done,
    Error,
}

#[derive(Serialize)]
struct Entry {
    id: u64,
    name: String,
    #[serde(skip)]
    path: PathBuf,
    /// True when `path` is a temp copy uploaded from the browser.
    uploaded: bool,
    size: u64,
    info: MediaInfo,
    status: Status,
    progress: f64,
    out_path: Option<String>,
    out_size: Option<u64>,
    error: Option<String>,
}

struct AppState {
    entries: Vec<Entry>,
    settings: Settings,
    out_dir: PathBuf,
    jobs: usize,
    converting: bool,
}

struct Ctx {
    tools: Tools,
    state: Mutex<AppState>,
    scratch: PathBuf,
    next_id: AtomicU64,
    /// Per-run secret embedded in the served page; state-changing requests
    /// must echo it back, which blocks blind cross-site POSTs from web pages.
    token: String,
}

/// Random-enough token from the process's SipHash keys (no rand dependency).
fn gen_token() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut h1 = RandomState::new().build_hasher();
    h1.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    let mut h2 = RandomState::new().build_hasher();
    h2.write_u64(std::process::id() as u64);
    format!("{:016x}{:016x}", h1.finish(), h2.finish())
}

fn json_response(status: u32, body: String) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_string(body)
        .with_status_code(status as u16)
        .with_header(Header::from_bytes("Content-Type", "application/json").unwrap())
}

fn ok_json<T: Serialize>(v: &T) -> Response<std::io::Cursor<Vec<u8>>> {
    json_response(
        200,
        serde_json::to_string(v).unwrap_or_else(|_| "{}".into()),
    )
}

fn err_json(status: u32, msg: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    json_response(status, serde_json::json!({ "error": msg }).to_string())
}

/// Default output directory: `<home>/resizer-output`.
pub fn default_out_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join("resizer-output")
}

fn open_in_os(path: &Path) {
    let (cmd, args): (&str, Vec<String>) = if cfg!(target_os = "windows") {
        (
            "cmd",
            vec![
                "/C".into(),
                "start".into(),
                "".into(),
                path.display().to_string(),
            ],
        )
    } else if cfg!(target_os = "macos") {
        ("open", vec![path.display().to_string()])
    } else {
        ("xdg-open", vec![path.display().to_string()])
    };
    let _ = std::process::Command::new(cmd)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

pub fn open_browser(url: &str) {
    open_in_os(Path::new(url));
}

fn content_type_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("mp4" | "m4v" | "mov") => "video/mp4",
        Some("webm") => "video/webm",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        _ => "application/octet-stream",
    }
}

/// Run the GUI server; blocks forever. Prints the URL and opens the browser.
pub fn run(tools: Tools, port: u16, no_browser: bool) -> Result<(), String> {
    let scratch = std::env::temp_dir().join(format!("resizer-gui-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).map_err(|e| format!("scratch dir: {e}"))?;

    let server = Server::http(("127.0.0.1", port))
        .map_err(|e| format!("could not bind 127.0.0.1:{port}: {e}"))?;
    let actual = match server.server_addr() {
        tiny_http::ListenAddr::IP(a) => a.port(),
        #[allow(unreachable_patterns)]
        _ => port,
    };
    let url = format!("http://127.0.0.1:{actual}");
    println!("resizer GUI: {url}");
    println!("({})", ffmpeg::version(&tools));
    if !no_browser {
        open_browser(&url);
    }

    let ctx = Arc::new(Ctx {
        tools,
        state: Mutex::new(AppState {
            entries: Vec::new(),
            settings: Settings::hover(),
            out_dir: default_out_dir(),
            jobs: jobs::default_jobs(),
            converting: false,
        }),
        scratch,
        next_id: AtomicU64::new(1),
        token: gen_token(),
    });

    // A handful of request threads: uploads and previews can overlap.
    let server = Arc::new(server);
    let mut handles = Vec::new();
    for _ in 0..4 {
        let server = Arc::clone(&server);
        let ctx = Arc::clone(&ctx);
        handles.push(std::thread::spawn(move || {
            while let Ok(req) = server.recv() {
                handle(req, &ctx);
            }
        }));
    }
    for h in handles {
        let _ = h.join();
    }
    Ok(())
}

/// The Host header must be our own loopback address: this blocks DNS
/// rebinding, where a hostile page's domain resolves to 127.0.0.1.
fn host_is_local(req: &Request) -> bool {
    let host = req
        .headers()
        .iter()
        .find(|h| h.field.equiv("Host"))
        .map(|h| h.value.as_str().to_string())
        .unwrap_or_default();
    let name = host.split(':').next().unwrap_or("");
    name == "127.0.0.1" || name == "localhost" || name == "[::1]"
}

fn token_ok(req: &Request, ctx: &Ctx) -> bool {
    req.headers()
        .iter()
        .find(|h| h.field.equiv("X-Resizer-Token"))
        .map(|h| h.value.as_str() == ctx.token)
        .unwrap_or(false)
}

fn handle(mut req: Request, ctx: &Arc<Ctx>) {
    if !host_is_local(&req) {
        let _ = req.respond(err_json(403, "forbidden"));
        return;
    }
    let url = req.url().to_string();
    let path = url.split('?').next().unwrap_or("/").to_string();
    let method = req.method().clone();

    // State-changing requests must carry the page token (CSRF protection).
    if method == Method::Post && !token_ok(&req, ctx) {
        let _ = req.respond(err_json(403, "bad token"));
        return;
    }

    let response = match (&method, path.as_str()) {
        (Method::Get, "/") => {
            let page = UI_HTML.replace("__RESIZER_TOKEN__", &ctx.token);
            let _ = req.respond(Response::from_string(page).with_header(
                Header::from_bytes("Content-Type", "text/html; charset=utf-8").unwrap(),
            ));
            return;
        }
        (Method::Get, "/api/state") => api_state(ctx),
        (Method::Post, "/api/upload") => api_upload(&mut req, ctx),
        (Method::Post, "/api/folder") => api_folder(&mut req, ctx),
        (Method::Post, "/api/settings") => api_settings(&mut req, ctx),
        (Method::Post, "/api/preview") => api_preview(&mut req, ctx),
        (Method::Post, "/api/convert") => api_convert(ctx),
        (Method::Post, "/api/clear") => api_clear(ctx),
        (Method::Post, "/api/open-output") => {
            let dir = ctx.state.lock().unwrap().out_dir.clone();
            let _ = std::fs::create_dir_all(&dir);
            open_in_os(&dir);
            ok_json(&serde_json::json!({ "ok": true }))
        }
        (Method::Get, p) if p.starts_with("/file/") => {
            let _ = serve_file(req, ctx, p);
            return;
        }
        _ => err_json(404, "not found"),
    };
    let _ = req.respond(response);
}

fn read_json_body(req: &mut Request) -> Result<serde_json::Value, String> {
    let mut buf = Vec::new();
    req.as_reader()
        .take(1 << 20)
        .read_to_end(&mut buf)
        .map_err(|e| format!("read body: {e}"))?;
    serde_json::from_slice(&buf).map_err(|e| format!("bad json: {e}"))
}

fn api_state(ctx: &Arc<Ctx>) -> Response<std::io::Cursor<Vec<u8>>> {
    let st = ctx.state.lock().unwrap();
    ok_json(&serde_json::json!({
        "entries": st.entries,
        "settings": st.settings,
        "out_dir": st.out_dir.display().to_string(),
        "jobs": st.jobs,
        "converting": st.converting,
    }))
}

fn api_upload(req: &mut Request, ctx: &Arc<Ctx>) -> Response<std::io::Cursor<Vec<u8>>> {
    // Raw body upload; the (urlencoded) filename travels in X-Filename.
    let name = req
        .headers()
        .iter()
        .find(|h| h.field.equiv("X-Filename"))
        .map(|h| h.value.as_str().to_string())
        .unwrap_or_default();
    let name = urldecode(&name);
    let safe = Path::new(&name)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("upload.bin")
        .to_string();
    if !jobs::is_media_path(Path::new(&safe)) {
        return err_json(415, "Ese tipo de archivo no es imagen ni video.");
    }

    let id = ctx.next_id.fetch_add(1, Ordering::Relaxed);
    // Per-id directory so the temp copy keeps its original filename — output
    // names are derived from it ("clip.mp4" -> "clip-web.mp4").
    let tmp_dir = ctx.scratch.join(format!("in-{id}"));
    if let Err(e) = std::fs::create_dir_all(&tmp_dir) {
        return err_json(500, &format!("temp dir: {e}"));
    }
    let tmp = tmp_dir.join(&safe);
    let mut file = match std::fs::File::create(&tmp) {
        Ok(f) => f,
        Err(e) => return err_json(500, &format!("temp file: {e}")),
    };
    // Read one byte past the cap so truncation is detected, not silent.
    let copied = std::io::copy(&mut req.as_reader().take(MAX_UPLOAD + 1), &mut file);
    match copied {
        Ok(n) if n > MAX_UPLOAD => {
            let _ = std::fs::remove_file(&tmp);
            return err_json(413, "Ese archivo pasa de 4 GB; es demasiado grande.");
        }
        Ok(n) if n > 0 => {}
        _ => {
            let _ = std::fs::remove_file(&tmp);
            return err_json(400, "El archivo llegó vacío.");
        }
    }

    match ffmpeg::probe(&ctx.tools, &tmp) {
        Ok(info) => {
            let size = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
            let entry = Entry {
                id,
                name: safe,
                path: tmp,
                uploaded: true,
                size,
                info,
                status: Status::Ready,
                progress: 0.0,
                out_path: None,
                out_size: None,
                error: None,
            };
            let mut st = ctx.state.lock().unwrap();
            st.entries.push(entry);
            ok_json(&serde_json::json!({ "id": id }))
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            err_json(415, &format!("No pude leer ese archivo: {e}"))
        }
    }
}

fn api_folder(req: &mut Request, ctx: &Arc<Ctx>) -> Response<std::io::Cursor<Vec<u8>>> {
    let body = match read_json_body(req) {
        Ok(b) => b,
        Err(e) => return err_json(400, &e),
    };
    let dir = PathBuf::from(body["path"].as_str().unwrap_or(""));
    let recursive = body["recursive"].as_bool().unwrap_or(false);
    if !dir.is_dir() {
        return err_json(
            400,
            "Esa carpeta no existe. Copia la ruta completa de la carpeta.",
        );
    }
    // Never re-ingest our own output directory.
    let out_dir = ctx.state.lock().unwrap().out_dir.clone();
    let found = jobs::collect_inputs(&[dir], recursive, Some(&out_dir));
    if found.is_empty() {
        return err_json(400, "No encontré imágenes ni videos en esa carpeta.");
    }
    let mut added = 0;
    for f in found {
        let info = match ffmpeg::probe(&ctx.tools, &f) {
            Ok(i) => i,
            Err(_) => continue,
        };
        let id = ctx.next_id.fetch_add(1, Ordering::Relaxed);
        let size = std::fs::metadata(&f).map(|m| m.len()).unwrap_or(0);
        let mut st = ctx.state.lock().unwrap();
        if st.entries.iter().any(|e| e.path == f) {
            continue;
        }
        st.entries.push(Entry {
            id,
            name: f
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("archivo")
                .to_string(),
            path: f,
            uploaded: false,
            size,
            info,
            status: Status::Ready,
            progress: 0.0,
            out_path: None,
            out_size: None,
            error: None,
        });
        added += 1;
    }
    ok_json(&serde_json::json!({ "added": added }))
}

fn api_settings(req: &mut Request, ctx: &Arc<Ctx>) -> Response<std::io::Cursor<Vec<u8>>> {
    let body = match read_json_body(req) {
        Ok(b) => b,
        Err(e) => return err_json(400, &e),
    };
    let mut st = ctx.state.lock().unwrap();
    if let Some(s) = body.get("settings") {
        match serde_json::from_value::<Settings>(s.clone()) {
            Ok(parsed) => st.settings = parsed,
            Err(e) => return err_json(400, &format!("bad settings: {e}")),
        }
    }
    if let Some(dir) = body["out_dir"].as_str() {
        if !dir.trim().is_empty() {
            st.out_dir = PathBuf::from(dir.trim());
        }
    }
    if let Some(j) = body["jobs"].as_u64() {
        st.jobs = (j as usize).clamp(1, 16);
    }
    ok_json(&serde_json::json!({ "ok": true }))
}

fn api_preview(req: &mut Request, ctx: &Arc<Ctx>) -> Response<std::io::Cursor<Vec<u8>>> {
    let body = match read_json_body(req) {
        Ok(b) => b,
        Err(e) => return err_json(400, &e),
    };
    let id = body["id"].as_u64().unwrap_or(0);
    let (path, info, settings) = {
        let st = ctx.state.lock().unwrap();
        let Some(e) = st.entries.iter().find(|e| e.id == id) else {
            return err_json(404, "archivo no encontrado");
        };
        (e.path.clone(), e.info.clone(), st.settings.clone())
    };
    match jobs::make_preview(&ctx.tools, &path, &info, &settings, &ctx.scratch, id) {
        Ok((out, est)) => {
            let plan = crate::plan::plan_video(&info, &settings);
            ok_json(&serde_json::json!({
                "url": format!("/file/{}/preview/{}", id, out.file_name().unwrap().to_string_lossy()),
                "est_bytes": est,
                "width": plan.out_w,
                "height": plan.out_h,
            }))
        }
        Err(e) => err_json(500, &e),
    }
}

fn api_convert(ctx: &Arc<Ctx>) -> Response<std::io::Cursor<Vec<u8>>> {
    {
        let mut st = ctx.state.lock().unwrap();
        if st.converting {
            return err_json(409, "Ya hay una conversión en curso.");
        }
        if !st.entries.iter().any(|e| e.status == Status::Ready) {
            return err_json(400, "No hay archivos pendientes.");
        }
        st.converting = true;
        for e in st.entries.iter_mut().filter(|e| e.status == Status::Ready) {
            e.status = Status::Working;
            e.progress = 0.0;
        }
    }

    let ctx2 = Arc::clone(ctx);
    std::thread::spawn(move || {
        let (ids_paths, out_dir, settings, njobs) = {
            let st = ctx2.state.lock().unwrap();
            (
                st.entries
                    .iter()
                    .filter(|e| e.status == Status::Working)
                    .map(|e| (e.id, e.path.clone()))
                    .collect::<Vec<_>>(),
                st.out_dir.clone(),
                st.settings.clone(),
                st.jobs,
            )
        };
        let inputs: Vec<PathBuf> = ids_paths.iter().map(|(_, p)| p.clone()).collect();
        let ids: Vec<u64> = ids_paths.iter().map(|(id, _)| *id).collect();

        jobs::run_bulk(
            &ctx2.tools,
            inputs,
            &out_dir,
            &settings,
            &ctx2.scratch,
            njobs,
            |idx, frac, done| {
                let mut st = ctx2.state.lock().unwrap();
                let id = ids[idx];
                if let Some(e) = st.entries.iter_mut().find(|e| e.id == id) {
                    e.progress = frac;
                    if let Some(item) = done {
                        match &item.result {
                            Ok((out, _in_b, out_b)) => {
                                e.status = Status::Done;
                                e.out_path = Some(out.display().to_string());
                                e.out_size = Some(*out_b);
                            }
                            Err(msg) => {
                                e.status = Status::Error;
                                e.error = Some(msg.clone());
                            }
                        }
                    }
                }
            },
        );
        ctx2.state.lock().unwrap().converting = false;
    });

    ok_json(&serde_json::json!({ "started": true }))
}

fn api_clear(ctx: &Arc<Ctx>) -> Response<std::io::Cursor<Vec<u8>>> {
    let mut st = ctx.state.lock().unwrap();
    if st.converting {
        return err_json(409, "Espera a que termine la conversión.");
    }
    for e in st.entries.drain(..) {
        if e.uploaded {
            // Uploads live in their own per-id scratch directory.
            if let Some(dir) = e.path.parent() {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }
    ok_json(&serde_json::json!({ "ok": true }))
}

/// GET /file/{id}/original  |  /file/{id}/preview/{name}
fn serve_file(req: Request, ctx: &Arc<Ctx>, path: &str) -> Result<(), std::io::Error> {
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let id: u64 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let kind = parts.get(2).copied().unwrap_or("");

    let file_path = match kind {
        "original" => {
            let st = ctx.state.lock().unwrap();
            st.entries
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.path.clone())
        }
        "preview" => {
            // Only serve preview files we generated for this id, never
            // arbitrary paths.
            let name = parts.get(3).copied().unwrap_or("");
            let expected_prefix = format!("preview-{id}.");
            if name.starts_with(&expected_prefix) && !name.contains("..") {
                Some(ctx.scratch.join(name))
            } else {
                None
            }
        }
        _ => None,
    };

    match file_path.filter(|p| p.is_file()) {
        Some(p) => {
            let f = std::fs::File::open(&p)?;
            let len = f.metadata().map(|m| m.len()).unwrap_or(0);
            req.respond(
                Response::from_file(f)
                    .with_header(Header::from_bytes("Content-Type", content_type_for(&p)).unwrap())
                    .with_header(Header::from_bytes("Content-Length", len.to_string()).unwrap()),
            )
        }
        None => req.respond(Response::from_string("not found").with_status_code(404)),
    }
}

fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                if let Ok(b) = u8::from_str_radix(hex, 16) {
                    out.push(b);
                    i += 3;
                    continue;
                }
                out.push(bytes[i]);
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urldecode_handles_utf8_and_spaces() {
        assert_eq!(urldecode("hola%20mundo.mp4"), "hola mundo.mp4");
        assert_eq!(urldecode("ni%C3%B1a.jpg"), "niña.jpg");
        assert_eq!(urldecode("a+b.png"), "a b.png");
        assert_eq!(urldecode("plain.mp4"), "plain.mp4");
        // Truncated escape stays as-is instead of panicking.
        assert_eq!(urldecode("bad%2"), "bad%2");
    }

    #[test]
    fn content_types_cover_outputs() {
        assert_eq!(content_type_for(Path::new("a.mp4")), "video/mp4");
        assert_eq!(content_type_for(Path::new("a.webp")), "image/webp");
        assert_eq!(
            content_type_for(Path::new("a.bin")),
            "application/octet-stream"
        );
    }

    #[test]
    fn default_out_dir_is_under_home() {
        let d = default_out_dir();
        assert!(d.ends_with("resizer-output"));
    }
}
