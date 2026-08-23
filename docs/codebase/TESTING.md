# Testing Patterns

Verificado contra el código el 2026-08-23 (v1.0.1, commit `4b1421d`).

## 1) Test Stack and Commands

- Framework principal: el test harness integrado de Rust (`cargo test`), sin
  crates de testing adicionales (no hay dev-dependencies en `Cargo.toml`).
- Aserciones/mocking: `assert!`/`assert_eq!` estándar; **no hay mocks** — se
  testea lógica pura o ffmpeg real.
- Tests de interfaz: script Node propio con `playwright-core` 1.62.1 pineado
  (contador de checks manual, no un framework de test).

```bash
cargo test                    # todo: unit + tests/e2e.rs + tests/setup.rs
cargo test --lib              # solo unitarios de la librería
cargo test --test e2e         # e2e CLI (se saltan solos si no hay ffmpeg)
cargo test --test setup       # arranque sin ffmpeg (corren en cualquier máquina)

# UI (requiere ffmpeg y un binario compilado):
cd tests/ui && npm install && npx playwright install chromium
node ui-test.mjs ../../target/debug/resizer     # o target/release/resizer
```

No hay herramienta de cobertura configurada.

## 2) Test Layout

- Unitarios: co-locados al final de cada módulo en `#[cfg(test)] mod tests`
  (`src/plan.rs`, `src/ffmpeg.rs`, `src/jobs.rs`, `src/install.rs`,
  `src/server.rs`).
- Integración: `tests/e2e.rs` (CLI contra ffmpeg real) y `tests/setup.rs`
  (GUI/CLI con ffmpeg deliberadamente inalcanzable).
- UI: `tests/ui/ui-test.mjs` + `package.json` propio (`npm test` apunta al
  binario release).
- No hay archivos de setup compartidos; cada test crea y borra sus propios
  directorios temporales.

## 3) Test Scope Matrix

| Alcance | ¿Cubierto? | Objetivo típico | Notas |
|---------|------------|-----------------|-------|
| Unit | Sí (amplio) | Matemática de `plan` (crop exacto, no-upscale, dimensiones pares, presupuesto ABR con piso/techo, presets), parseo de ffprobe, construcción de args, rutas de salida, opciones de instalación | Todo puro, corre sin ffmpeg |
| Integración/E2E | Sí | Binario `resizer-cli` real: preset hover produce 4:5 mudo bajo presupuesto, `--keep-audio`, bulk de carpeta mixta, auto-ajuste de imagen, `probe`, error amable sin ffmpeg | `require_ffmpeg!` los salta con aviso si falta ffmpeg; CI sí los corre |
| Arranque sin ffmpeg | Sí | El servidor arranca, sirve la pantalla de setup, ofrece opciones con una recomendación, los endpoints de media devuelven 503 con mensaje claro, sobrevive a stdout cerrado; en Windows verifica el subsystem PE (GUI=2, CLI=3) | `tests/setup.rs`; cliente HTTP mínimo escrito a mano (sin dev-deps) |
| UI (navegador real) | Sí | Pantalla de instalación (nada se instala solo), previews grandes, comparador A/B: arrastre del divisor, flechas de teclado, `object-fit` idéntico en ambas capas, clip-path real, modo lado a lado, limpieza al cerrar | Chromium vía Playwright; corre en CI (job `ui`) |

## 4) Mocking and Isolation Strategy

- Enfoque: **sin mocks**. La frontera testeable es la pureza: `plan.rs` y
  `parse_probe` se testean con datos sintéticos; lo demás contra ffmpeg real
  (videos generados con `testsrc2`/`sine` de lavfi, `tests/e2e.rs`).
- Aislamiento: directorios temporales con PID en el nombre, borrados al final;
  `HOME`/`LOCALAPPDATA`/`XDG_DATA_HOME` apuntan a sandboxes y `PATH=""` +
  `FFMPEG_PATH` falso para simular la ausencia de ffmpeg sin tocar el sistema
  real (`tests/setup.rs::sandbox`, `tests/e2e.rs::no_ffmpeg_home`).
- Modo de fallo típico: tests que dependen del entorno (p. ej.
  `recommendation_prefers_a_package_manager_when_present` se adapta a qué
  gestores hay en la máquina en vez de asumirlos).

## 5) Coverage and Quality Signals

- Cobertura: sin herramienta ni umbral configurados. `[TODO]` si se quiere
  medir (p. ej. `cargo llvm-cov`).
- CI (`.github/workflows/ci.yml`): fmt + clippy `-D warnings`; `cargo test
  --all-targets` con ffmpeg instalado en ubuntu-latest, windows-latest y
  macos-14 (Apple Silicon); tests de UI en ubuntu con Chromium pineado.
- Huecos conocidos:
  - `src/server.rs` solo tiene unitarios de helpers (`urldecode`,
    `content_type_for`, `default_out_dir`); los endpoints con ffmpeg presente
    (upload, preview, convert) se cubren indirectamente vía los tests de UI.
  - `install::install` (los caminos que de verdad instalan) no se ejecuta en
    tests — razonable: instalaría software real.
  - El comentario de CI sobre Playwright: `npx playwright@latest` desalinearía
    el Chromium del `playwright-core` pineado; por eso se usa
    `npx --no-install` (documentado en `ci.yml`).

## 6) Evidence

- `tests/e2e.rs`, `tests/setup.rs`, `tests/ui/ui-test.mjs`, `tests/ui/package.json`
- Módulos `#[cfg(test)]` en `src/plan.rs`, `src/ffmpeg.rs`, `src/jobs.rs`,
  `src/install.rs`, `src/server.rs`
- `.github/workflows/ci.yml`
