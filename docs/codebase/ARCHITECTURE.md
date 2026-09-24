# Architecture

Verificado contra el código el 2026-08-23 (v1.0.1, commit `4b1421d`).

## 1) Architectural Style

- **Estilo**: aplicación local por capas alrededor de un núcleo puro.
  Un núcleo de planificación sin I/O (`plan.rs`), una capa de ejecución de
  ffmpeg (`ffmpeg.rs`), un pipeline con pool de hilos (`jobs.rs`) y dos
  frontends finos (CLI y servidor HTTP local con página embebida).
- **Por qué**: `plan.rs` declara explícitamente "No I/O here so everything is
  unit-testable"; los binarios solo hacen wiring; `server.rs` y
  `resizer-cli.rs` comparten `jobs::run_bulk` y `plan::Settings`.
- **Restricciones que moldean el diseño**:
  1. Cero consola en Windows: binario GUI con `windows_subsystem="windows"`,
     trait `NoWindow` en cada proceso hijo (`src/lib.rs`), apertura de archivos
     con `explorer` en vez de `cmd`, y `say()` que tolera stdout cerrado
     (`src/server.rs`).
  2. ffmpeg es opcional al arrancar: `tools: Mutex<Option<Tools>>` en el
     contexto del servidor; la pantalla de setup lo rellena en caliente.
  3. Los originales nunca se modifican: la salida siempre va a otro archivo
     no-clobbering (`jobs::plan_output_names`) y el directorio de salida se excluye
     de la re-ingesta (`jobs::collect_inputs` con `exclude`).

## 2) System Flow

Conversión (CLI o GUI, mismo camino):

```text
entrada (archivo/carpeta)
  -> jobs::collect_inputs        (filtra por extensión, salta ocultos y el out_dir)
  -> ffmpeg::probe               (ffprobe JSON -> plan::MediaInfo, con rotación)
  -> plan::plan_video            (crop centrado -> fit -> fps -> rate control)
  -> ffmpeg::video_commands / image_command   (listas de args; 2 pasadas si hay max_mb)
  -> ffmpeg::run_with_progress   (parsea -progress pipe:1, callback 0.0..1.0)
  -> jobs::run_bulk              (pool de N hilos, un proceso ffmpeg por worker)
  -> salida <stem>-web.<ext>     (falla si el archivo queda vacío)
```

GUI: `resizer` → `server::run` → tiny_http en `127.0.0.1:puerto` (4 hilos de
request) → sirve `ui.html` con un token inyectado → la página hace polling de
`GET /api/state` y dispara `POST /api/{upload,folder,settings,preview,convert,
clear,open-output,ffmpeg/install}`. Conversión e instalación corren en hilos
en segundo plano y publican su progreso en el estado compartido.

## 3) Layer/Module Responsibilities

| Capa/módulo | Posee | No debe poseer | Evidencia |
|-------------|-------|----------------|-----------|
| `plan` | Matemática de encode: crop a 4:5/1:1/16:9, `fit_within` (nunca agranda), dimensiones pares, ABR de dos pasadas con piso/techo de bits-por-pixel, escalera de calidad de imagen | Procesos, archivos | `src/plan.rs` |
| `ffmpeg` | Descubrimiento de binarios (orden: explícito, `FFMPEG_PATH`, copia propia, PATH, dirs de gestores, junto al exe), probe, args, ejecución con progreso | Política de calidad | `src/ffmpeg.rs` |
| `jobs` | Conversión de un archivo (video 1–2 pasadas / imagen con escalera hasta caber en presupuesto), previews, cola `run_bulk` con `thread::scope` | Args de ffmpeg | `src/jobs.rs` |
| `install` | Opciones por SO con recomendación, escalación (root/pkexec/imposible-con-mensaje), descarga directa verificada ejecutando `-version` | Conversión | `src/install.rs` |
| `server` | Estado (`AppState`), API JSON, seguridad local, servido de archivos original/preview | Encode | `src/server.rs` |
| `ui.html` | Toda la interfaz: drag&drop, carpeta server-side, ajustes, comparador A/B superpuesto y lado a lado, progreso | Nada de lógica de encode (pide todo a la API) | `src/ui.html` |

## 4) Reused Patterns

| Patrón | Dónde | Por qué |
|--------|-------|---------|
| Trait de extensión `NoWindow` sobre `process::Command` | `src/lib.rs`, usado en `ffmpeg.rs`, `install.rs`, `server.rs` | Ningún hijo abre ventana de consola en Windows |
| Núcleo puro + shell con I/O | `plan.rs` vs resto | Tests unitarios exhaustivos de la matemática sin ffmpeg |
| Args compartidos entre encode real y preview (`base_video_args`) | `src/ffmpeg.rs::preview_command` | "Sharing base_video_args keeps previews honest when encode flags change" |
| Estado compartido `Arc<Ctx>` + `Mutex` + trabajo en hilos de fondo, la página hace polling | `src/server.rs` | GUI sin websockets ni framework; una sola fuente de verdad |
| Errores como `Result<_, String>` legibles por humanos | todos los módulos | La audiencia es una usuaria final; los mensajes de cara al usuario van en español |
| Token CSRF por ejecución + validación del header `Host` | `src/server.rs::{gen_token, token_ok, host_is_local}` | Bloquear POSTs cross-site ciegos y DNS rebinding contra el servidor local |

## 5) Known Architectural Risks

- **Un solo estado global de conversión**: `AppState.converting` es un booleano;
  no hay cancelación de una conversión en curso ni cola de tandas. Impacto:
  para lotes muy grandes solo queda esperar (`src/server.rs::api_convert`).
- **Polling como único canal de progreso**: si la página se cierra, la
  conversión sigue en el hilo de fondo sin dueño visible. Impacto bajo (es
  local y termina sola), pero no hay forma de re-adjuntarse más que reabrir la
  página y mirar `/api/state`.
- **`run_bulk` toma el lock del estado en cada tick de progreso**
  (`src/server.rs`, callback de `api_convert`): con muchos archivos el lock se
  disputa, aunque los ticks son de baja frecuencia (líneas de `-progress`).
- **La estimación de peso en previews CRF extrapola 2.5 s al total**
  (`src/jobs.rs::make_preview`): honesta para ABR (bitrate conocido), gruesa
  para CRF con escenas heterogéneas. Es una estimación mostrada como tal.

## 6) Evidence

- `src/plan.rs` (núcleo puro + constantes `MIN_BITS_PER_PIXEL`, `MAX_BITS_PER_PIXEL`)
- `src/jobs.rs::run_bulk`, `src/server.rs::{run, handle, api_convert}`
- `src/lib.rs` (trait `NoWindow`), `src/bin/resizer.rs`
- `src/ffmpeg.rs::{candidate_paths, video_commands, run_with_progress}`
