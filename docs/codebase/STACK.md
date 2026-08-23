# Technology Stack

Verificado contra el código el 2026-08-23 (v1.0.1, commit `4b1421d`).

## 1) Runtime Summary

| Área | Valor | Evidencia |
|------|-------|-----------|
| Lenguaje principal | Rust (edition 2021) | `Cargo.toml` |
| Runtime | Binarios nativos (sin runtime); toolchain `stable` en CI | `.github/workflows/ci.yml` |
| Gestor de paquetes | Cargo (lockfile v4) | `Cargo.lock` |
| Build | Cargo; perfil release con `opt-level=3`, `lto`, `strip`, `codegen-units=1` | `Cargo.toml` `[profile.release]` |

Se compilan **una librería** (`resizer`, `src/lib.rs`) y **dos binarios**:
`resizer` (GUI, doble clic, sin ventana de consola en Windows) y `resizer-cli`
(terminal). Definidos en `Cargo.toml` (`[lib]`, `[[bin]]`).

## 2) Production Frameworks and Dependencies

Solo 4 dependencias directas (deliberadamente mínimas):

| Dependencia | Versión | Rol | Evidencia |
|-------------|---------|-----|-----------|
| `clap` (features `derive`) | 4.5 | Parseo de CLI en `resizer-cli` | `Cargo.toml`, `src/bin/resizer-cli.rs` |
| `serde` (features `derive`) | 1 | Serialización de `Settings`, `MediaInfo`, estado de la GUI | `src/plan.rs`, `src/server.rs` |
| `serde_json` | 1 | API JSON de la GUI y parseo de la salida de ffprobe | `src/server.rs`, `src/ffmpeg.rs` |
| `tiny_http` | 0.12 | Servidor HTTP local de la GUI (127.0.0.1) | `src/server.rs` |

**Dependencia externa en runtime: ffmpeg + ffprobe.** No están en Cargo: son
ejecutables del sistema que el programa localiza (`src/ffmpeg.rs::find_tools`)
o instala él mismo (`src/install.rs`). El programa arranca sin ellos y ofrece
instalarlos (pantalla de setup en la GUI, `install-ffmpeg` en la CLI).

## 3) Development Toolchain

| Herramienta | Propósito | Evidencia |
|-------------|-----------|-----------|
| `rustfmt` | Formato (config por defecto, sin archivo propio); CI exige `cargo fmt --all --check` | `.github/workflows/ci.yml` job `lint` |
| `clippy` | Lint; CI exige `cargo clippy --all-targets -- -D warnings` | `.github/workflows/ci.yml` job `lint` |
| `cargo test` | Tests unitarios + integración | `.github/workflows/ci.yml` job `test` |
| Playwright 1.62.1 (pineado) + Node 20 | Tests de interfaz en Chromium real | `tests/ui/package.json`, `tests/ui/ui-test.mjs` |

No hay archivos de configuración de lint/format en el repo: se usan los
valores por defecto de rustfmt/clippy, con el enforcement en CI.

## 4) Key Commands

```bash
cargo test                                  # unit + integración (e2e se saltan sin ffmpeg)
cargo clippy --all-targets -- -D warnings   # lint (igual que CI)
cargo fmt --all                             # formato
cargo run --bin resizer                     # abre la GUI en el navegador
cargo run --bin resizer-cli -- --help       # CLI

# Tests de interfaz (requieren ffmpeg y Chromium):
cd tests/ui && npm install && npx playwright install chromium
node ui-test.mjs ../../target/debug/resizer
```

## 5) Environment and Config

- Fuentes de configuración: no hay archivos de config ni `.env`; todo se pasa
  por flags de CLI o por la GUI.
- Variables de entorno que el código lee:
  - `FFMPEG_PATH` — ruta explícita a ffmpeg, gana sobre la autodetección (`src/ffmpeg.rs::candidate_paths`).
  - `LOCALAPPDATA` / `USERPROFILE` (Windows), `HOME` (macOS/Linux), `XDG_DATA_HOME` (Linux) — para el directorio de datos privado `…/resizer` (`src/install.rs::data_dir`).
  - `ChocolateyInstall` — para localizar los shims de Chocolatey (`src/install.rs::package_manager_dirs`).
  - `CHROMIUM_PATH` — solo tests de UI, ruta a Chromium (`tests/ui/ui-test.mjs`).
- Restricciones de despliegue: la release de Linux se compila contra
  `x86_64-unknown-linux-musl` (binario estático); macOS solo `aarch64-apple-darwin`
  (Apple Silicon); Windows `x86_64-pc-windows-msvc` (`.github/workflows/release.yml`).

## 6) Evidence

- `Cargo.toml`, `Cargo.lock`
- `.github/workflows/ci.yml`, `.github/workflows/release.yml`
- `src/ffmpeg.rs`, `src/install.rs` (dependencia externa ffmpeg)
- `tests/ui/package.json`
