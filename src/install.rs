//! Finding and installing ffmpeg for the user.
//!
//! The program never requires ffmpeg to sit next to the executable: it looks
//! in the OS package locations first, then in its own private data directory,
//! and it can install ffmpeg there on request. Every installation method is
//! offered to the user with a per-OS recommendation before anything runs.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::NoWindow;

/// How ffmpeg can be installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    /// Windows package manager (ships with Windows 10/11).
    Winget,
    /// Windows community package manager.
    Chocolatey,
    /// macOS package manager.
    Homebrew,
    /// Debian/Ubuntu.
    Apt,
    /// Fedora/RHEL.
    Dnf,
    /// Arch.
    Pacman,
    /// Download an official static build into the app's own folder.
    Download,
}

impl Method {
    pub fn id(self) -> &'static str {
        match self {
            Method::Winget => "winget",
            Method::Chocolatey => "chocolatey",
            Method::Homebrew => "homebrew",
            Method::Apt => "apt",
            Method::Dnf => "dnf",
            Method::Pacman => "pacman",
            Method::Download => "download",
        }
    }

    pub fn from_id(s: &str) -> Option<Method> {
        Some(match s {
            "winget" => Method::Winget,
            "chocolatey" | "choco" => Method::Chocolatey,
            "homebrew" | "brew" => Method::Homebrew,
            "apt" => Method::Apt,
            "dnf" => Method::Dnf,
            "pacman" => Method::Pacman,
            "download" => Method::Download,
            _ => return None,
        })
    }

    /// The command that must exist on PATH for this method to be usable.
    fn driver(self) -> Option<&'static str> {
        match self {
            Method::Winget => Some("winget"),
            Method::Chocolatey => Some("choco"),
            Method::Homebrew => Some("brew"),
            Method::Apt => Some("apt-get"),
            Method::Dnf => Some("dnf"),
            Method::Pacman => Some("pacman"),
            Method::Download => None,
        }
    }
}

/// One installation option as presented to the user.
#[derive(Debug, Clone, Serialize)]
pub struct Option_ {
    pub id: &'static str,
    /// Short name shown on the button.
    pub label: &'static str,
    /// One line explaining what will happen, in Spanish (the GUI's language).
    pub detail: String,
    /// True when this is the suggested choice for the current OS.
    pub recommended: bool,
    /// May pop up a UAC / password prompt.
    pub needs_admin: bool,
}

fn on_path(cmd: &str) -> bool {
    Command::new(cmd)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .no_window()
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Methods that make sense on this machine, best first. `Download` is always
/// present as the no-prerequisites fallback.
pub fn options() -> Vec<Option_> {
    let candidates: Vec<Method> = if cfg!(windows) {
        vec![Method::Winget, Method::Chocolatey, Method::Download]
    } else if cfg!(target_os = "macos") {
        vec![Method::Homebrew, Method::Download]
    } else {
        vec![Method::Apt, Method::Dnf, Method::Pacman, Method::Download]
    };

    let usable: Vec<Method> = candidates
        .into_iter()
        .filter(|m| m.driver().map(on_path).unwrap_or(true))
        .collect();

    // Recommendation: the OS's own package manager when it is actually
    // present (updates come for free), otherwise the self-contained download.
    let recommended = usable
        .iter()
        .copied()
        .find(|m| *m != Method::Download)
        .unwrap_or(Method::Download);

    usable
        .into_iter()
        .map(|m| Option_ {
            id: m.id(),
            label: match m {
                Method::Winget => "Winget",
                Method::Chocolatey => "Chocolatey",
                Method::Homebrew => "Homebrew",
                Method::Apt => "apt",
                Method::Dnf => "dnf",
                Method::Pacman => "pacman",
                Method::Download => "Descarga directa",
            },
            detail: detail_for(m),
            recommended: m == recommended,
            needs_admin: matches!(
                m,
                Method::Winget | Method::Chocolatey | Method::Apt | Method::Dnf | Method::Pacman
            ),
        })
        .collect()
}

fn detail_for(m: Method) -> String {
    match m {
        Method::Winget => "Usa el gestor de paquetes de Windows. Puede pedirte permiso de \
             administrador; después ffmpeg queda disponible para todo el sistema."
            .into(),
        Method::Chocolatey => "Instala con Chocolatey. Requiere permisos de administrador.".into(),
        Method::Homebrew => "Instala con Homebrew (brew install ffmpeg). Es la forma habitual \
             en macOS y se actualiza con el resto de tus paquetes."
            .into(),
        Method::Apt => "Instala el paquete ffmpeg del sistema. Te pedirá tu contraseña.".into(),
        Method::Dnf => "Instala el paquete ffmpeg del sistema. Te pedirá tu contraseña.".into(),
        Method::Pacman => "Instala el paquete ffmpeg del sistema. Te pedirá tu contraseña.".into(),
        Method::Download => format!(
            "Descarga una versión oficial y la guarda solo para este programa, en {}. \
             No necesita permisos ni toca el resto del sistema.",
            bin_dir().display()
        ),
    }
}

/// Private data directory for this app (never next to the executable, so the
/// program works from Downloads, a USB stick, or anywhere else).
pub fn data_dir() -> PathBuf {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("AppData\\Local"))
            })
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    };
    base.unwrap_or_else(std::env::temp_dir).join("resizer")
}

/// Where a downloaded ffmpeg lives.
pub fn bin_dir() -> PathBuf {
    data_dir().join("bin")
}

pub fn exe_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}

/// Directories where the OS package managers put ffmpeg. A process that just
/// triggered an install still has its old PATH, so these are checked directly.
pub fn package_manager_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
            // winget's shim directory.
            dirs.push(local.join("Microsoft\\WinGet\\Links"));
        }
        // Chocolatey's shim directory.
        dirs.push(
            PathBuf::from(
                std::env::var("ChocolateyInstall")
                    .unwrap_or_else(|_| "C:\\ProgramData\\chocolatey".into()),
            )
            .join("bin"),
        );
    } else if cfg!(target_os = "macos") {
        // Homebrew on Apple Silicon and on Intel.
        dirs.push(PathBuf::from("/opt/homebrew/bin"));
        dirs.push(PathBuf::from("/usr/local/bin"));
    } else {
        dirs.push(PathBuf::from("/usr/bin"));
        dirs.push(PathBuf::from("/usr/local/bin"));
    }
    dirs
}

/// Quote a path for a PowerShell single-quoted string. Paths come from the
/// user's own profile, which may legitimately contain an apostrophe
/// (`C:\Users\O'Brien\...`); without doubling it the command is malformed.
fn ps_quote(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', "''"))
}

/// Run a command, returning a readable error with the tail of its output.
fn run(cmd: &mut Command, what: &str) -> Result<String, String> {
    let out = cmd
        .stdin(std::process::Stdio::null())
        .no_window()
        .output()
        .map_err(|e| format!("no pude ejecutar {what}: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if out.status.success() {
        Ok(stdout)
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = err
            .lines()
            .chain(stdout.lines())
            .filter(|l| !l.trim().is_empty())
            .rev()
            .take(6)
            .collect();
        Err(format!(
            "{what} falló:\n{}",
            tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        ))
    }
}

/// Official static-build URLs used by the `Download` method.
pub fn download_url() -> Result<&'static str, String> {
    Ok(match (std::env::consts::OS, std::env::consts::ARCH) {
        // Gyan's "essentials" build is the one ffmpeg.org links for Windows.
        ("windows", _) => "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip",
        ("linux", "x86_64") => {
            "https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-amd64-static.tar.xz"
        }
        ("linux", "aarch64") => {
            "https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-arm64-static.tar.xz"
        }
        // evermeet.cx publishes the builds ffmpeg.org links for macOS.
        ("macos", _) => "https://evermeet.cx/ffmpeg/getrelease/zip",
        (os, arch) => {
            return Err(format!(
                "No tengo una descarga directa para {os}/{arch}. Usa el gestor de paquetes de tu sistema."
            ))
        }
    })
}

/// macOS needs a second download because evermeet ships ffprobe separately.
fn macos_ffprobe_url() -> &'static str {
    "https://evermeet.cx/ffmpeg/getrelease/ffprobe/zip"
}

/// Install ffmpeg using `method`, reporting human-readable progress steps.
/// Returns the directory ffmpeg ended up in (None = somewhere on PATH).
pub fn install(method: Method, mut on_step: impl FnMut(&str)) -> Result<Option<PathBuf>, String> {
    match method {
        Method::Download => {
            let dir = download_into(bin_dir(), &mut on_step)?;
            Ok(Some(dir))
        }
        Method::Winget => {
            on_step("Instalando con winget (puede pedirte permiso de administrador)…");
            run(
                Command::new("winget").args([
                    "install",
                    "--id",
                    "Gyan.FFmpeg",
                    "-e",
                    "--source",
                    "winget",
                    "--accept-package-agreements",
                    "--accept-source-agreements",
                    "--disable-interactivity",
                ]),
                "winget",
            )?;
            Ok(None)
        }
        Method::Chocolatey => {
            on_step("Instalando con Chocolatey (requiere administrador)…");
            run(
                Command::new("choco").args(["install", "ffmpeg", "-y", "--no-progress"]),
                "choco",
            )?;
            Ok(None)
        }
        Method::Homebrew => {
            on_step("Instalando con Homebrew…");
            run(Command::new("brew").args(["install", "ffmpeg"]), "brew")?;
            Ok(None)
        }
        Method::Apt | Method::Dnf | Method::Pacman => {
            let args: Vec<&str> = match method {
                Method::Apt => vec!["apt-get", "install", "-y", "ffmpeg"],
                Method::Dnf => vec!["dnf", "install", "-y", "ffmpeg"],
                _ => vec!["pacman", "-S", "--noconfirm", "ffmpeg"],
            };
            match escalation() {
                Escalation::None => on_step("Instalando el paquete del sistema…"),
                Escalation::Pkexec => {
                    on_step("Instalando el paquete del sistema (te pedirá tu contraseña)…")
                }
                // Without a terminal, sudo has no way to ask for a password;
                // say so instead of hanging or failing cryptically.
                Escalation::Unavailable => {
                    return Err(format!(
                        "Para instalar el paquete del sistema hace falta permiso de administrador, \
                         y aquí no puedo pedírtelo. Abre una terminal y ejecuta:\n\n    sudo {}\n\n\
                         O elige \"Descarga directa\", que no necesita permisos.",
                        args.join(" ")
                    ))
                }
            }
            let mut cmd = privileged(&args);
            run(&mut cmd, args[0])?;
            Ok(None)
        }
    }
}

/// How this process can gain the privileges a system package install needs.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Escalation {
    /// Already root: run the command directly.
    None,
    /// `pkexec` can show a graphical password prompt.
    Pkexec,
    /// No way to prompt (no tty, no pkexec).
    Unavailable,
}

fn escalation() -> Escalation {
    if is_root() {
        Escalation::None
    } else if on_path("pkexec") {
        Escalation::Pkexec
    } else {
        Escalation::Unavailable
    }
}

/// True when this process already runs as root. `USER` can be unset, so fall
/// back to asking the system whose id we are.
fn is_root() -> bool {
    if ["USER", "LOGNAME"]
        .iter()
        .any(|k| std::env::var(k).map(|v| v == "root").unwrap_or(false))
    {
        return true;
    }
    Command::new("id")
        .arg("-u")
        .no_window()
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim() == "0")
        .unwrap_or(false)
}

/// Build a command that installs a system package with the privileges of
/// whatever escalation is actually usable here.
fn privileged(args: &[&str]) -> Command {
    match escalation() {
        Escalation::None => {
            let mut c = Command::new(args[0]);
            c.args(&args[1..]);
            c
        }
        // `sudo` is unreachable from here (see `install`), so this is pkexec.
        _ => {
            let mut c = Command::new("pkexec");
            c.args(args);
            c
        }
    }
}

/// Download an official build and extract ffmpeg/ffprobe into `dest`.
/// Uses the tools every supported OS already ships (PowerShell / curl+tar),
/// so the binary stays dependency-free.
fn download_into(dest: PathBuf, on_step: &mut impl FnMut(&str)) -> Result<PathBuf, String> {
    let url = download_url()?;
    std::fs::create_dir_all(&dest).map_err(|e| format!("no pude crear {}: {e}", dest.display()))?;
    let work = data_dir().join("download");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| format!("no pude crear carpeta temporal: {e}"))?;

    if cfg!(windows) {
        let zip = work.join("ffmpeg.zip");
        on_step("Descargando ffmpeg (unos 30 MB)…");
        run(
            Command::new("powershell").args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!(
                    "$ProgressPreference='SilentlyContinue'; \
                     Invoke-WebRequest -Uri '{url}' -OutFile {}",
                    ps_quote(&zip)
                ),
            ]),
            "la descarga",
        )?;
        on_step("Descomprimiendo…");
        run(
            Command::new("powershell").args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!(
                    "Expand-Archive -Path {} -DestinationPath {} -Force",
                    ps_quote(&zip),
                    ps_quote(&work)
                ),
            ]),
            "la descompresión",
        )?;
    } else {
        let is_zip = url.ends_with("zip");
        let archive = work.join(if is_zip {
            "ffmpeg.zip"
        } else {
            "ffmpeg.tar.xz"
        });
        on_step("Descargando ffmpeg…");
        run(
            Command::new("curl")
                .args(["-fL", "--retry", "2", "-o"])
                .arg(&archive)
                .arg(url),
            "la descarga",
        )?;
        on_step("Descomprimiendo…");
        if is_zip {
            run(
                Command::new("unzip")
                    .arg("-oq")
                    .arg(&archive)
                    .arg("-d")
                    .arg(&work),
                "la descompresión",
            )?;
            // evermeet ships ffmpeg and ffprobe as separate archives.
            let probe_zip = work.join("ffprobe.zip");
            run(
                Command::new("curl")
                    .args(["-fL", "--retry", "2", "-o"])
                    .arg(&probe_zip)
                    .arg(macos_ffprobe_url()),
                "la descarga de ffprobe",
            )?;
            run(
                Command::new("unzip")
                    .arg("-oq")
                    .arg(&probe_zip)
                    .arg("-d")
                    .arg(&work),
                "la descompresión de ffprobe",
            )?;
        } else {
            run(
                Command::new("tar")
                    .arg("-xJf")
                    .arg(&archive)
                    .arg("-C")
                    .arg(&work),
                "la descompresión",
            )?;
        }
    }

    on_step("Colocando ffmpeg en su sitio…");
    let mut installed: Vec<PathBuf> = Vec::new();
    for base in ["ffmpeg", "ffprobe"] {
        let name = exe_name(base);
        let src = find_file(&work, &name, 4)
            .ok_or_else(|| format!("el paquete descargado no traía {name}"))?;
        let target = dest.join(&name);
        std::fs::copy(&src, &target).map_err(|e| format!("no pude copiar {name}: {e}"))?;
        make_executable(&target)?;
        installed.push(target);
    }
    let _ = std::fs::remove_dir_all(&work);

    // The archives are fetched over HTTPS from the builds ffmpeg.org links,
    // and they publish no stable checksum for their rolling "latest" URLs —
    // so verify what actually landed on disk: both binaries must run and
    // report a version. A truncated or wrong-architecture download fails here
    // rather than at the user's first conversion.
    on_step("Comprobando la instalación…");
    for bin in &installed {
        let ok = Command::new(bin)
            .arg("-version")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .no_window()
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            for b in &installed {
                let _ = std::fs::remove_file(b);
            }
            return Err(format!(
                "lo descargado no funciona en este equipo ({} no responde). \
                 Prueba con el gestor de paquetes de tu sistema.",
                bin.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
    }
    Ok(dest)
}

#[cfg(unix)]
fn make_executable(p: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("no pude dar permisos a {}: {e}", p.display()))
}

#[cfg(not(unix))]
fn make_executable(_p: &Path) -> Result<(), String> {
    Ok(())
}

/// Breadth-limited search for a file by name inside an extracted archive.
pub fn find_file(dir: &Path, name: &str, depth: u32) -> std::option::Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut subdirs = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            subdirs.push(p);
        } else if p.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(p);
        }
    }
    if depth == 0 {
        return None;
    }
    subdirs
        .into_iter()
        .find_map(|d| find_file(&d, name, depth - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_os_offers_at_least_the_download_option() {
        let opts = options();
        assert!(!opts.is_empty());
        assert!(
            opts.iter().any(|o| o.id == "download"),
            "download must always be offered: {opts:?}"
        );
    }

    #[test]
    fn exactly_one_option_is_recommended() {
        let opts = options();
        let n = opts.iter().filter(|o| o.recommended).count();
        assert_eq!(n, 1, "expected one recommendation, got {n}: {opts:?}");
    }

    #[test]
    fn recommendation_prefers_a_package_manager_when_present() {
        let opts = options();
        let rec = opts.iter().find(|o| o.recommended).unwrap();
        // If any package manager is available, it must be the recommendation.
        let has_pm = opts.iter().any(|o| o.id != "download");
        if has_pm {
            assert_ne!(
                rec.id, "download",
                "a package manager was available: {opts:?}"
            );
        } else {
            assert_eq!(rec.id, "download");
        }
    }

    #[test]
    fn options_carry_a_human_explanation() {
        for o in options() {
            assert!(!o.label.is_empty());
            assert!(
                o.detail.len() > 20,
                "option {} needs a real explanation",
                o.id
            );
        }
    }

    #[test]
    fn method_ids_round_trip() {
        for m in [
            Method::Winget,
            Method::Chocolatey,
            Method::Homebrew,
            Method::Apt,
            Method::Dnf,
            Method::Pacman,
            Method::Download,
        ] {
            assert_eq!(Method::from_id(m.id()), Some(m));
        }
        assert_eq!(Method::from_id("nope"), None);
        // Friendly aliases.
        assert_eq!(Method::from_id("brew"), Some(Method::Homebrew));
        assert_eq!(Method::from_id("choco"), Some(Method::Chocolatey));
    }

    #[test]
    fn data_dir_is_private_and_not_next_to_the_exe() {
        let d = data_dir();
        assert!(d.ends_with("resizer"), "{d:?}");
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()));
        if let Some(exe_dir) = exe_dir {
            assert_ne!(bin_dir(), exe_dir);
        }
    }

    #[test]
    fn download_url_is_defined_for_this_platform() {
        // Every platform CI builds on must have a direct download.
        let url = download_url().expect("no download URL for this platform");
        assert!(url.starts_with("https://"), "{url}");
    }

    #[test]
    fn find_file_locates_nested_binaries() {
        let root = std::env::temp_dir().join(format!("resizer-find-{}", std::process::id()));
        let nested = root.join("ffmpeg-7.0-essentials/bin");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("ffmpeg.exe"), b"x").unwrap();
        assert!(find_file(&root, "ffmpeg.exe", 4).is_some());
        assert!(find_file(&root, "ffprobe.exe", 4).is_none());
        // Depth limit is respected.
        assert!(find_file(&root, "ffmpeg.exe", 0).is_none());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
