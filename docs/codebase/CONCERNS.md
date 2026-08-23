# Codebase Concerns

Verificado contra el código el 2026-08-23 (v1.0.1, commit `4b1421d`).
El scan no encontró ningún `TODO`/`FIXME`/`HACK` en código de producción, y el
README está al día con el código (se verificó afirmación por afirmación).

## 1) Top Risks (Prioritized)

| Severidad | Riesgo | Evidencia | Impacto | Acción sugerida |
|-----------|--------|-----------|---------|-----------------|
| Media | Sin cancelación: una conversión en curso no se puede abortar desde la GUI ni la CLI (solo matar el proceso) | `src/server.rs::api_convert` (`converting: bool`, sin endpoint de cancelar) | Lotes grandes equivocados obligan a esperar o matar la app; los ffmpeg hijos quedan corriendo si se mata mal | Endpoint `/api/cancel` que mate los hijos y drene la cola |
| Media | Sin timeout en procesos hijos: un ffmpeg colgado bloquea su worker para siempre | `src/ffmpeg.rs::{run_with_progress, run_quiet}` (esperas sin límite) | Un archivo patológico congela parte del lote sin mensaje | Timeout configurable o watchdog sobre la falta de progreso |
| Baja | El scratch de la GUI (`$TMP/resizer-gui-<pid>`) nunca se borra al salir: uploads solo se limpian con "clear", previews se acumulan | `src/server.rs::run` (crea), `api_clear` (única limpieza); la CLI sí borra el suyo (`src/bin/resizer-cli.rs`) | Disco temporal creciendo entre sesiones largas; el SO suele limpiarlo eventualmente | Borrar el scratch en un hook de salida y/o barrer `resizer-gui-*` viejos al arrancar |
| Baja | `heic` está aceptado como entrada, pero muchos builds de ffmpeg (incl. algunos "essentials") no decodifican HEIC | `src/jobs.rs::IMAGE_EXTS` | El probe falla y el archivo se descarta (GUI: en silencio en modo carpeta) | [TODO] verificar con los builds que instala `download`; si no, avisar en la UI |

## 2) Technical Debt

| Ítem | Por qué existe | Dónde | Riesgo si se ignora | Arreglo sugerido |
|------|----------------|-------|---------------------|------------------|
| Errores como `String` en todas las capas | Simplicidad; los mensajes ya van redactados para el usuario | todos los módulos | Difícil distinguir clases de error programáticamente (p. ej. reintentar solo descargas) | Solo si crece: enum de error con `Display` |
| Fallos de probe silenciosos en modo carpeta | Un archivo corrupto no debe frenar el lote | `src/server.rs::api_folder` (`Err(_) => continue`) | La usuaria no sabe por qué un archivo no aparece | Acumular y mostrar "N archivos no se pudieron leer" |
| Playwright pineado a 1.62.1 con nota de drift | El Chromium de `@latest` se desalinea del `playwright-core` pineado | `tests/ui/package.json`, comentario en `ci.yml` | Bit-rot del pin con el tiempo | Bump periódico deliberado de ambos paquetes a la vez |
| Estimación de peso de previews CRF extrapola 2.5 s | Mantener la preview rápida | `src/jobs.rs::make_preview` | Estimaciones imprecisas en clips heterogéneos (solo cosmético) | Etiquetarla como "≈" en la UI (ya se muestra como estimado) |

Nota: no hay TODOs en tests tampoco; los huecos de cobertura están en
`TESTING.md` §5, no aquí.

## 3) Security Concerns

Contexto: servidor **solo en 127.0.0.1**, un solo usuario local, sin cuentas
ni secretos. Las defensas existentes son deliberadas y están testeadas.

| Riesgo | OWASP | Evidencia | Mitigación actual | Hueco |
|--------|-------|-----------|-------------------|-------|
| Descarga de ffmpeg sin checksum | A08 (integridad) | `src/install.rs::download_into` (rationale en comentario) | HTTPS a los mirrors que enlaza ffmpeg.org + verificación post-instalación ejecutando `-version` (borra los binarios si fallan) | Un mirror comprometido serviría un binario que sí "funciona"; opción: pin de checksums por versión en releases propias |
| CSRF / DNS rebinding contra el servidor local | A01 | `src/server.rs::{token_ok, host_is_local}` + `tests/setup.rs` | Token por ejecución exigido en todo POST; header `Host` restringido a loopback | El token sale de `RandomState` (SipHash) + reloj + PID: impredecible en la práctica pero no CSPRNG; suficiente localmente |
| La API puede leer carpetas arbitrarias del disco (`/api/folder`) y servir esos archivos (`/file/{id}/original`) | A01 | `src/server.rs::{api_folder, serve_file}` | Es funcionalidad (modo carpeta local); protegido por token + Host + loopback; previews solo con prefijo `preview-{id}.` y sin `..` | Aceptable para app local mono-usuario; no exponer nunca el puerto fuera de loopback |
| Instalación con privilegios (pkexec/UAC) | — | `src/install.rs::{escalation, privileged}` | Comandos fijos (sin input del usuario en los args); sin tty explica el `sudo` manual en vez de colgarse | Ninguno observado |
| Binarios de release sin firmar/notarizar | — | `.github/workflows/release.yml` | — | En macOS Gatekeeper pondrá en cuarentena el binario descargado; [ASK USER] §6 |

## 4) Performance and Scaling Concerns

| Tema | Evidencia | Síntoma | Riesgo | Mejora sugerida |
|------|-----------|---------|--------|-----------------|
| Lock global del estado en cada tick de progreso del bulk | `src/server.rs::api_convert` (callback con `ctx.state.lock()`) | Ninguno a escala actual (ticks poco frecuentes) | Contención con cientos de archivos | Canal + un solo hilo que aplique actualizaciones |
| Dos pasadas duplican el tiempo de encode cuando hay `max_mb` | `src/ffmpeg.rs::video_commands` | Conversión ~2× más lenta que CRF | Es el trade-off elegido para clavar el presupuesto | Ninguna; documentado |
| Paralelismo por defecto: mitad de cores, 1–8 | `src/jobs.rs::default_jobs`; GUI permite 1–16 (`api_settings`) | — | ffmpeg ya usa varios hilos por proceso; >8 jobs puede sobresuscribir | Mantener el clamp |
| Uploads del navegador copian el archivo a temp (hasta 4 GB) | `src/server.rs::api_upload` (`MAX_UPLOAD`) | Doble uso de disco para archivos enormes | Bajo (streaming a disco, no a memoria) | Preferir el modo carpeta para lotes pesados |

## 5) Fragile/High-Churn Areas

Historial corto (8 commits); el churn refleja las tareas de release más que
fragilidad real.

| Área | Por qué | Señal | Estrategia de cambio segura |
|------|---------|-------|-----------------------------|
| `.github/workflows/release.yml` | 3 cambios en 90 días (tag input, empaquetado) | scan "HIGH-CHURN" | Probar con `workflow_dispatch` antes de taguear |
| `src/ui.html` (37.8 KB, archivo más grande) | Toda la UI en un archivo sin framework; JS a mano | tamaño + 2 cambios | Correr `tests/ui/ui-test.mjs` siempre; los tests cubren setup, previews y A/B |
| `src/server.rs` (25.4 KB) | Concentra estado, API y seguridad local | tamaño + 2 cambios | Mantener los invariantes token/Host testeados en `tests/setup.rs` |

## 6) `[ASK USER]` Questions

1. [ASK USER] macOS: la release solo compila `aarch64` (Apple Silicon). ¿Los
   Macs Intel quedan fuera a propósito, o vale la pena añadir un target
   `x86_64-apple-darwin` (o universal) al workflow de release?
2. [ASK USER] ¿Interesa firmar/notarizar los binarios de macOS y Windows para
   evitar los avisos de Gatekeeper/SmartScreen al descargarlos de Releases?
3. [ASK USER] ¿Se espera soporte real de HEIC (fotos de iPhone)? Depende del
   build de ffmpeg instalado; habría que verificarlo y, si falta, avisar en la
   UI o documentar la limitación.
4. [ASK USER] ¿Añadir cancelación de conversiones en curso a la GUI, o el
   flujo actual (esperar/cerrar) es suficiente para el uso real?
5. [ASK USER] La UI está fija en español por diseño (kamiru.art). ¿Correcto
   dejarlo así, sin i18n?

## 7) Evidence

- Salida del scan del repo del 2026-08-23 (secciones TODO/FIXME, HIGH-CHURN,
  CODE METRICS): 0 TODOs en producción; churn dominado por `Cargo.toml`,
  `release.yml` y `README.md`; archivos más grandes `ui.html` y `server.rs`
- `src/server.rs`, `src/install.rs`, `src/jobs.rs`, `src/ffmpeg.rs`
- `.github/workflows/release.yml`, `tests/ui/package.json`
