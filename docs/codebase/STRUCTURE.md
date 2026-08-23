# Codebase Structure

Verificado contra el código el 2026-08-23 (v1.0.1, commit `4b1421d`).

## 1) Top-Level Map

| Ruta | Propósito | Evidencia |
|------|-----------|-----------|
| `Cargo.toml` / `Cargo.lock` | Manifiesto: 1 lib + 2 binarios, 4 dependencias | `Cargo.toml` |
| `src/lib.rs` | Raíz de la librería: declara los módulos y el trait `NoWindow` | `src/lib.rs` |
| `src/bin/resizer.rs` | Binario GUI (doble clic; `windows_subsystem = "windows"`) | `src/bin/resizer.rs` |
| `src/bin/resizer-cli.rs` | Binario de terminal (clap: `gui`, `convert`, `probe`, `install-ffmpeg`) | `src/bin/resizer-cli.rs` |
| `src/plan.rs` | Lógica pura de planificación (sin I/O): crop, escala, control de bitrate | `src/plan.rs` |
| `src/ffmpeg.rs` | Localizar/ejecutar ffmpeg y ffprobe, construir listas de argumentos | `src/ffmpeg.rs` |
| `src/jobs.rs` | Pipeline de conversión de un archivo + cola multihilo para bulk | `src/jobs.rs` |
| `src/install.rs` | Instalación de ffmpeg (gestores de paquetes o descarga directa) | `src/install.rs` |
| `src/server.rs` | Servidor HTTP local de la GUI + API JSON | `src/server.rs` |
| `src/ui.html` | Interfaz completa en una sola página (embebida con `include_str!`) | `src/server.rs` línea `const UI_HTML` |
| `tests/e2e.rs` | Tests end-to-end de la CLI contra ffmpeg real (se saltan si falta) | `tests/e2e.rs` |
| `tests/setup.rs` | Tests del arranque sin ffmpeg (pantalla de setup, API, subsystem PE) | `tests/setup.rs` |
| `tests/ui/` | Tests de interfaz con Playwright (script propio, no framework) | `tests/ui/ui-test.mjs` |
| `.github/workflows/` | `ci.yml` (fmt+clippy, tests en 3 SO, tests de UI) y `release.yml` (binarios + GitHub Release) | ambos archivos |
| `README.md` | Documentación de usuario (en español); verificada al día con el código | `README.md` |
| `docs/codebase/` | Esta documentación | — |

No hay `CLAUDE.md` ni otros documentos de intención en el repo.

## 2) Entry Points

- **GUI**: `src/bin/resizer.rs::main` → `resizer::server::run`. Acepta solo
  `--port`, `--no-browser`, `--version` (flags de depuración; sin superficie
  CLI a propósito). Si ffmpeg falta, arranca igual y la página muestra la
  pantalla de instalación. Si `server::run` falla antes de existir la UI,
  escribe `resizer-error.log` en el data dir y lo abre.
- **CLI**: `src/bin/resizer-cli.rs::main`. Sin subcomando (o con `gui`) abre
  la misma GUI. `install-ffmpeg` y `gui` funcionan sin ffmpeg; `convert` y
  `probe` exigen encontrarlo (exit code 2 si no).
- La selección de binario es del usuario: los dos se empaquetan juntos en la
  release (`.github/workflows/release.yml`, paso "Package").

## 3) Module Boundaries

| Módulo | Qué le pertenece | Qué no debe tener |
|--------|------------------|-------------------|
| `plan` | Decisiones puras: crop, dimensiones, fps, control de bitrate, escalera de calidad | I/O, procesos, rutas de archivos reales |
| `ffmpeg` | Localizar binarios, `probe`, construir args, ejecutar con/sin progreso | Decisiones de calidad/tamaño (vienen de `plan`) |
| `jobs` | Orquestar la conversión de un archivo y la cola paralela; nombres de salida | Construcción de comandos ffmpeg (delegada a `ffmpeg`) |
| `install` | Detección de gestores de paquetes, descarga directa, data dir | Nada de conversión de media |
| `server` | HTTP, API JSON, estado de la GUI, seguridad local (token + Host) | Lógica de encode (usa `jobs`/`plan`) |
| binarios | Parseo de flags y wiring; salida a consola | Lógica de negocio |

## 4) Naming and Organization Rules

- Archivos y módulos: `snake_case` de una palabra (`plan.rs`, `jobs.rs`).
- Organización por capa/responsabilidad, no por feature (el proyecto es pequeño).
- Sin path aliases ni re-exports; los binarios importan `resizer::{modulo}`.
- Archivos de salida de conversión: `<stem>-web.<ext>` con sufijo `-2`, `-3`…
  para no sobreescribir (`src/jobs.rs::output_path`).

## 5) Evidence

- `Cargo.toml` (`[lib]`, `[[bin]]`)
- `src/lib.rs` (declaración de módulos)
- `src/bin/resizer.rs`, `src/bin/resizer-cli.rs`
- `.github/workflows/release.yml` (empaquetado de ambos binarios)
