# External Integrations

Verificado contra el código el 2026-08-23 (v1.0.1, commit `4b1421d`).

resizer es una aplicación local: **no hay base de datos, ni servicios en la
nube, ni telemetría, ni cuentas**. Sus integraciones son procesos del sistema
y unas URLs de descarga de ffmpeg.

## 1) Integration Inventory

| Sistema | Tipo | Propósito | Auth | Criticidad | Evidencia |
|---------|------|-----------|------|------------|-----------|
| ffmpeg / ffprobe | Subproceso | Todo el trabajo de media: probe, encode, previews | N/A | Alta (núcleo; la app arranca sin él y ofrece instalarlo) | `src/ffmpeg.rs` |
| winget / choco / brew / apt-get / dnf / pacman | Subproceso | Instalar ffmpeg con el gestor del SO | UAC / contraseña vía `pkexec` en Linux | Media (opcional; siempre existe la descarga directa) | `src/install.rs::install` |
| `gyan.dev` (Windows), `johnvansickle.com` (Linux x86_64/aarch64), `evermeet.cx` (macOS; ffprobe en zip aparte) | HTTPS descarga | Método "Descarga directa" de ffmpeg, vía PowerShell `Invoke-WebRequest` o `curl` + `tar`/`unzip` del sistema | HTTPS, sin checksum (ver §4) | Media | `src/install.rs::{download_url, download_into}` |
| Navegador del usuario | Subproceso (`explorer`/`open`/`xdg-open`) | Abrir la GUI y la carpeta de salida | N/A | Alta para la GUI | `src/server.rs::open_in_os` |
| GitHub Actions / Releases | CI/CD | Lint, tests en 3 SO, tests de UI; binarios y release en tag `v*` | `GITHUB_TOKEN` implícito (`permissions: contents: write`) | Solo desarrollo | `.github/workflows/ci.yml`, `release.yml` |

## 2) Data Stores

| Almacén | Rol | Acceso | Riesgo clave | Evidencia |
|---------|-----|--------|--------------|-----------|
| Data dir privado (`%LOCALAPPDATA%\resizer`, `~/Library/Application Support/resizer`, `$XDG_DATA_HOME/resizer`) | ffmpeg autoinstalado (`bin/`), descargas temporales, `resizer-error.log` | `src/install.rs::{data_dir, bin_dir}` | Bajo; nunca junto al ejecutable (funciona desde USB/Descargas) | `src/install.rs`, test `data_dir_is_private_and_not_next_to_the_exe` |
| Scratch temporal (`$TMP/resizer-gui-<pid>`, `resizer-cli-<pid>`) | Uploads del navegador, previews, passlogs x264 | `src/server.rs::run`, `src/jobs.rs` | Acumulación: la GUI no borra su scratch al salir (ver CONCERNS) | `src/server.rs`, `src/jobs.rs` |
| Carpeta de salida (`~/resizer-output` en GUI; `<carpeta>/resized` en CLI) | Resultados; nunca sobreescribe (`-web`, `-web-2`, …) | `jobs::output_path`, `server::default_out_dir` | Bajo | `src/jobs.rs`, `src/server.rs` |

## 3) Secrets and Credentials Handling

- No hay secretos: sin API keys, sin tokens persistentes, sin config sensible.
- El único "secreto" es el **token CSRF por ejecución** de la GUI, generado en
  memoria (`server::gen_token`, SipHash de `RandomState` + reloj + PID),
  inyectado en la página como `<meta name="resizer-token">` y exigido en todo
  POST. No se persiste.
- Búsqueda de credenciales hardcodeadas: sin hallazgos (las URLs de descarga
  son públicas).

## 4) Reliability and Failure Behavior

- **Reintentos**: `curl --retry 2` en la descarga directa; nada más reintenta
  (un encode fallido se reporta por archivo y el lote continúa,
  `jobs::run_bulk` / `BulkItemResult`).
- **Timeouts**: no hay timeouts explícitos en procesos ni HTTP local. Un
  ffmpeg colgado colgaría su worker ([ASK USER] ver CONCERNS §6).
- **Verificación post-descarga en vez de checksum**: los proveedores de builds
  no publican checksum estable para sus URLs "latest", así que
  `install::download_into` ejecuta `ffmpeg -version` y `ffprobe -version`
  sobre lo instalado y borra los binarios si no responden (rationale
  documentado en el propio código).
- **Instalación sin privilegios posible siempre**: la opción `download` existe
  en todos los SO; en Linux sin `pkexec` ni root, el instalador explica el
  comando `sudo` en vez de colgarse (`install::escalation`).
- **PATH obsoleto tras instalar**: un proceso vivo no ve el PATH nuevo, así
  que `ffmpeg::candidate_paths` consulta directamente los directorios
  conocidos de winget/choco/brew/apt (`install::package_manager_dirs`).

## 5) Observability for Integrations

- Llamadas a ffmpeg: la cola de stderr (últimas ~30 líneas) se conserva y se
  devuelve en el error (`ffmpeg::run_with_progress`); el progreso se parsea de
  `-progress pipe:1`.
- Instalación: pasos legibles publicados en `/api/state` (`SetupState.steps`)
  y en stdout de la CLI.
- No hay métricas ni tracing (aplicación local; no aplica).
- Hueco de visibilidad: los fallos de probe en `api_folder` se descartan en
  silencio (`Err(_) => continue`) — un archivo corrupto simplemente no aparece
  en la lista.

## 6) Evidence

- `src/ffmpeg.rs` (descubrimiento y ejecución)
- `src/install.rs` (gestores, URLs, verificación, escalación)
- `src/server.rs` (token, API, data dirs)
- `.github/workflows/ci.yml`, `.github/workflows/release.yml`
