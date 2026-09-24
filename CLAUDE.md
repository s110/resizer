# resizer

Wrapper amigable de ffmpeg (GUI `resizer` + CLI `resizer-cli`) para preparar
videos y fotos para kamiru.art. Documentación verificada del código en
`docs/codebase/`.

## Testing

Política:

- NEVER write unit tests after you write code.
- Highly prefer E2E tests as the sole testing mechanism. Use them to verify complex features work. At the end of E2E tests, produce a verifiable and repeatable artifact.
- If you must test a system in isolation, FIRST write all the ways it could fail, THEN write the code.
- When writing E2E tests don't pick the simplest possible scenario to prove it works; pick a medium to hard scenario when verifying the work with E2E tests.
- Tautological tests considered harmful.
- Change-detector tests considered harmful.
- Do not create regression tests for bug fixes without a genuine gap in behavior testing.

E2E principal (necesita `ffmpeg` y `ffprobe` en PATH; sin ellos se salta en
local y falla en CI):

```bash
cargo test --test e2e_shoot_batch
```

- **Escenario**: una carpeta de sesión real pasa por `resizer-cli convert
  --preset hover --max-mb 0.5 --recursive --jobs 3` dos veces sobre la misma
  salida. Mezcla `IMG_0001.MOV` + `IMG_0001.mp4` (mismo nombre), video de
  móvil rotado a 60 fps, foto con orientación EXIF 6, PNG RGBA de 16 bits,
  GIF animado de tamaño impar, panorámica 4000x90, foto pesada en subcarpeta,
  un mp4 corrupto en medio, un `.txt` y un archivo oculto. Se juzga con
  ffprobe, píxeles decodificados y PSNR contra recortes hechos desde la
  fuente, nunca contra el planificador.
- **Artefacto**: `artifacts/e2e/shoot_batch.json` (en `.gitignore`). Trae
  hashes de entradas, propiedades de cada salida, hash de píxeles de las
  salidas bit-exactas y un veredicto por check.
- **Cómo verificarlo**: `"result": "pass"` y ningún check en `fail` ni
  `unexpected_pass`. Los checks `known_failing` documentan bugs abiertos
  (BUG-1 nombres repetidos se pisan en paralelo, BUG-2 orientación EXIF
  ignorada); si uno empieza a pasar el test falla para quitar la marca junto
  con el arreglo. Repetible: dos corridas en la misma máquina dan el mismo
  archivo (`shasum artifacts/e2e/shoot_batch.json`). Los videos solo guardan
  propiedades estables porque x264 en dos pasadas con VBV y varios hilos no es
  bit-exacto.

Resto de la suite: `cargo test --all-targets` (unitarios, `tests/e2e.rs`,
`tests/setup.rs` y el E2E). Interfaz en navegador real:
`cd tests/ui && npm install && node ui-test.mjs ../../target/debug/resizer`.
CI corre todo lo anterior y sube el artefacto del E2E por sistema operativo.
