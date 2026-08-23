# Coding Conventions

Verificado contra el código el 2026-08-23 (v1.0.1, commit `4b1421d`).

## 1) Naming Rules

| Elemento | Regla | Ejemplo | Evidencia |
|----------|-------|---------|-----------|
| Archivos/módulos | `snake_case`, una palabra por responsabilidad | `plan.rs`, `install.rs` | `src/` |
| Funciones | `snake_case`, verbos descriptivos | `plan_video`, `collect_inputs`, `find_tools` | `src/plan.rs`, `src/jobs.rs` |
| Tipos | `PascalCase`; enums con variantes descriptivas | `EncodePlan`, `RateControl::TwoPass`, `Method::Winget` | `src/plan.rs`, `src/install.rs` |
| Constantes | `SCREAMING_SNAKE_CASE` con comentario de rationale | `MIN_BITS_PER_PIXEL`, `MAX_UPLOAD` | `src/plan.rs`, `src/server.rs` |
| Serde | `#[serde(rename_all = "snake_case")]` en enums serializados | `Ratio`, `ImageFormat`, `Method` | `src/plan.rs`, `src/install.rs` |
| Tests | nombres-frase que enuncian el comportamiento | `autotune_does_not_inflate_short_clips`, `output_path_never_clobbers` | `src/plan.rs`, `src/jobs.rs` |

## 2) Formatting and Linting

- Formatter: `rustfmt` con configuración por defecto (no hay `rustfmt.toml`).
- Linter: `clippy` con configuración por defecto, pero **CI trata todo warning
  como error**: `cargo clippy --all-targets -- -D warnings`.
- Comandos: `cargo fmt --all` y el comando de clippy anterior
  (`.github/workflows/ci.yml`, job `lint`, corre en cada push a main y PR).

## 3) Import and Module Conventions

- Orden de `use` observado en todos los archivos: `std` primero, luego crates
  externos, luego `crate::…`, separados por línea en blanco (p. ej.
  `src/server.rs`, `src/jobs.rs`).
- Sin barrels ni re-exports: `lib.rs` solo declara `pub mod` y el trait
  `NoWindow`. Los binarios usan rutas `resizer::modulo`.
- La UI se embebe con `include_str!("ui.html")` — un solo artefacto, sin
  assets externos.

## 4) Error and Logging Conventions

- **Estrategia de error**: `Result<T, String>` con mensajes ya redactados para
  humanos en todas las capas; no hay tipos de error propios ni `anyhow`.
  Los errores de procesos incluyen la cola de stderr (últimas ~6–30 líneas:
  `install::run`, `ffmpeg::run_with_progress`).
- **Idiomas**: mensajes de cara al usuario en **español** (GUI, instalador,
  API: "ffmpeg todavía no está instalado."); mensajes técnicos/CLI de encode y
  comentarios/doc-comments en **inglés**. Es intencional: `install.rs`
  documenta "in Spanish (the GUI's language)".
- **Sin panics en caminos de usuario**: el binario GUI nunca usa `println!`
  (usa `say()` que tolera stdout cerrado, `src/server.rs`); el arranque
  fallido escribe `resizer-error.log` y lo abre (`src/bin/resizer.rs`).
  `unwrap()/expect()` se reservan para locks e invariantes internas.
- **Logging**: no hay framework de logging; stdout informativo en CLI y unos
  `say()` al arrancar la GUI. El progreso viaja por callbacks y por
  `/api/state`, no por logs.
- Códigos de salida de la CLI: `1` fallo de operación, `2` error de uso /
  ffmpeg no encontrado (`src/bin/resizer-cli.rs`).

## 5) Testing Conventions

- Tests unitarios co-locados en `#[cfg(test)] mod tests` al final de cada
  módulo; integración en `tests/*.rs`; UI en `tests/ui/ui-test.mjs`.
- Los e2e se auto-saltan sin ffmpeg con el macro `require_ffmpeg!`
  (`tests/e2e.rs`) para que `cargo test` pase en cualquier máquina.
- Aislamiento por directorios temporales con el PID en el nombre y
  HOME/LOCALAPPDATA/XDG_DATA_HOME falsos para no tocar datos reales
  (`tests/setup.rs::sandbox`).
- Sin mocks: se testea contra ffmpeg real o contra funciones puras.

## 6) Evidence

- `.github/workflows/ci.yml` (fmt + clippy -D warnings)
- `src/server.rs` (`say`, mensajes en español, `Result<_, String>`)
- `src/install.rs` (rationale de idioma), `src/bin/resizer-cli.rs` (exit codes)
- `tests/e2e.rs`, `tests/setup.rs`

## Commit / branching (observado en el historial)

- Mensajes en inglés, imperativo, describiendo el efecto ("Add resizer: …",
  "Allow dispatching the release workflow with a tag input", "Release 1.0.1").
- Trabajo por ramas + merge a `main` vía PR (merges #1–#3 en `git log`).
- Releases: tag `v*` o dispatch manual del workflow Release.
