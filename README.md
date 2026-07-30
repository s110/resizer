# resizer

Herramienta para preparar **videos y fotos para kamiru.art** sin pelearse con
ffmpeg: recorta al formato del sitio (las tarjetas *hover* 4:5, cuadrado 1:1 o
16:9), comprime a un peso objetivo y guarda copias listas para subir.
Los archivos originales **nunca se modifican**.

Funciona en Windows, Linux y macOS (Apple Silicon). Es un wrapper de ffmpeg:
solo usa las funciones básicas (libx264 + escalado), así que cualquier build
"essentials" de ffmpeg sirve.

## Instalación

1. Descarga el binario de tu sistema desde
   [Releases](https://github.com/s110/resizer/releases) y descomprímelo.
2. Instala ffmpeg (una sola vez):
   - **Windows:** `winget install Gyan.FFmpeg` — o descarga la build
     *essentials* de [gyan.dev](https://www.gyan.dev/ffmpeg/builds/) y deja
     `ffmpeg.exe` y `ffprobe.exe` junto a `resizer.exe`.
   - **macOS:** `brew install ffmpeg`
   - **Linux:** `sudo apt install ffmpeg`

## Uso normal (interfaz gráfica)

Haz doble clic en `resizer` (o ejecuta `resizer` en la terminal). Se abre el
navegador con la interfaz:

1. **Arrastra** tus videos o fotos (o una carpeta entera) a la zona de carga,
   o pega la ruta de una carpeta para procesarla donde está.
2. Elige el **formato del sitio**: *Hover 4:5* deja el video exactamente como
   lo necesitan las tarjetas de la página de trabajos (recorte centrado, sin
   audio, máximo 10 MB por defecto).
3. Ajusta lo que quieras: peso máximo, resolución máxima, calidad, FPS,
   quitar audio, formato de imagen…
4. **Vista previa**: botón "previa" en cada archivo — muestra los primeros
   segundos ya convertidos, lado a lado con el original, y el peso estimado.
5. **Convertir todo**: procesa varios archivos en paralelo y muestra el
   progreso y el ahorro de peso de cada uno.

### Auto-ajuste de peso

Con "Limitar peso" activado, tú solo dices el máximo de MB y la resolución
máxima; la herramienta calcula sola el bitrate (codificación en dos pasadas)
y, si el presupuesto es muy chico para esa resolución, baja la resolución
automáticamente para que no se vea pixelado.

## Uso por terminal (bulk / scripts)

```bash
# Toda una carpeta al formato hover (4:5, sin audio, ≤10MB c/u), 4 a la vez:
resizer convert ~/Videos/proyectos --preset hover --jobs 4

# Carpeta con subcarpetas, máximo 6 MB y 720p de alto:
resizer convert ./media --recursive --max-mb 6 --max-height 720

# Solo comprimir sin recortar, calidad fija:
resizer convert clip.mov --preset original --crf 22 --keep-audio

# Fotos a webp de máximo 300 KB:
resizer convert ./fotos --image-format webp --max-mb 0.3

# Ver qué detecta ffprobe en un archivo:
resizer probe clip.mp4
```

Opciones principales de `resizer convert`:

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
cargo run           # abre la GUI
```

CI corre fmt, clippy y los tests en Linux, Windows y macOS (Apple Silicon).
Al empujar un tag `v*` se compilan los binarios de las tres plataformas y se
publica la release automáticamente.
