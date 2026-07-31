# resizer

Herramienta para preparar **videos y fotos para kamiru.art** sin pelearse con
ffmpeg: recorta al formato del sitio (las tarjetas *hover* 4:5, cuadrado 1:1 o
16:9), comprime a un peso objetivo y guarda copias listas para subir.
Los archivos originales **nunca se modifican**.

Funciona en Windows, Linux y macOS (Apple Silicon). Es un wrapper de ffmpeg:
solo usa las funciones básicas (libx264 + escalado), así que cualquier build
"essentials" de ffmpeg sirve.

## Instalación

1. Descarga el archivo de tu sistema desde
   [Releases](https://github.com/s110/resizer/releases) y descomprímelo.
2. Haz doble clic en **`resizer`**. No hace falta terminal ni ventanas negras:
   se abre directamente el navegador con la interfaz.
3. La primera vez, si te falta **ffmpeg**, el programa te lo dice y te pregunta
   *cómo* quieres instalarlo, con una recomendación según tu sistema
   (winget en Windows, Homebrew en macOS, el gestor de paquetes en Linux, o
   una descarga directa que no necesita permisos). Eliges y él se encarga.

ffmpeg **no** tiene que estar junto al programa: si lo instala él mismo, lo
guarda en su propia carpeta de datos (`%LOCALAPPDATA%\resizer\bin` en Windows),
así que `resizer` funciona desde Descargas, el Escritorio o un USB.

Cada archivo del zip:

| Archivo | Para qué |
| --- | --- |
| `resizer` | La aplicación: doble clic y se abre la interfaz. **Es la que usas normalmente.** |
| `resizer-cli` | La versión de terminal, para automatizar o convertir carpetas por línea de comandos. |

## Uso normal (interfaz gráfica)

Haz doble clic en `resizer`. Se abre el navegador con la interfaz:

1. **Arrastra** tus videos o fotos (o una carpeta entera) a la zona de carga,
   o pega la ruta de una carpeta para procesarla donde está.
2. Elige el **formato del sitio**: *Hover 4:5* deja el video exactamente como
   lo necesitan las tarjetas de la página de trabajos (recorte centrado, sin
   audio, máximo 10 MB por defecto).
3. Ajusta lo que quieras: peso máximo, resolución máxima, calidad, FPS,
   quitar audio, formato de imagen…
4. **Vista previa**: botón "previa" en cada archivo. Se abre grande y en modo
   **A/B superpuesto**: el original y el resultado en el mismo sitio, con una
   línea que arrastras (o mueves con ← →) para ver el antes y el después sobre
   los mismos píxeles. También hay modo **lado a lado**, y el peso estimado.
5. **Convertir todo**: procesa varios archivos en paralelo y muestra el
   progreso y el ahorro de peso de cada uno.

### Auto-ajuste de peso

Con "Limitar peso" activado, tú solo dices el máximo de MB y la resolución
máxima; la herramienta calcula sola el bitrate (codificación en dos pasadas)
y, si el presupuesto es muy chico para esa resolución, baja la resolución
automáticamente para que no se vea pixelado.

## Uso por terminal (bulk / scripts)

```bash
# Instalar ffmpeg desde la terminal (sin --method lista las opciones):
resizer-cli install-ffmpeg
resizer-cli install-ffmpeg --method download

# Toda una carpeta al formato hover (4:5, sin audio, ≤10MB c/u), 4 a la vez:
resizer-cli convert ~/Videos/proyectos --preset hover --jobs 4

# Carpeta con subcarpetas, máximo 6 MB y 720p de alto:
resizer-cli convert ./media --recursive --max-mb 6 --max-height 720

# Solo comprimir sin recortar, calidad fija:
resizer-cli convert clip.mov --preset original --crf 22 --keep-audio

# Fotos a webp de máximo 300 KB:
resizer-cli convert ./fotos --image-format webp --max-mb 0.3

# Ver qué detecta ffprobe en un archivo:
resizer-cli probe clip.mp4
```

Opciones principales de `resizer-cli convert`:

| Opción | Qué hace |
| --- | --- |
| `--preset hover\|square\|landscape\|original` | Formato del sitio (hover = 4:5, mudo, ≤10MB) |
| `--max-mb N` | Auto-ajuste: peso máximo por archivo (videos: dos pasadas) |
| `--max-width / --max-height N` | Resolución máxima (nunca se agranda) |
| `--crf N` | Calidad fija 0–51 cuando no hay peso máximo (23 por defecto) |
| `--fps N` | Limita los FPS (0 = como el original) |
| `--no-audio` / `--keep-audio` | Quitar o conservar el audio |
| `--speed veryfast\|medium\|slow` | Velocidad vs. compresión |
| `--image-format keep\|jpg\|webp\|png` | Formato de salida de imágenes |
| `--jobs N` | Cuántos archivos convertir en paralelo |
| `--recursive` | Incluir subcarpetas |
| `--out DIR` | Carpeta de salida (por defecto `<carpeta>/resized`) |

## Desarrollo

```bash
cargo test          # unit + integración (los e2e se saltan si no hay ffmpeg)
cargo clippy --all-targets -- -D warnings
cargo fmt --all
cargo run --bin resizer     # abre la GUI

# Tests de la interfaz (pantalla de instalación, previas, comparador A/B):
cd tests/ui && npm install && npx playwright install chromium
node ui-test.mjs ../../target/debug/resizer
```

CI corre fmt, clippy, los tests de Rust en Linux/Windows/macOS (Apple Silicon)
y los tests de interfaz en un navegador real. Al empujar un tag `v*` (o
lanzando el workflow *Release* a mano) se compilan los binarios de las tres
plataformas y se publica la release automáticamente.
