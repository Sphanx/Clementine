# Clementine

A zero-click, offline file converter for Linux. Drag a file onto the window, tap a modifier,
and drop it on a format. Inspired by [Tangerine](https://tangerineformac.com/) for Mac.

![The conversion wheel](docs/wheel.png)

Everything runs on your computer: nothing is uploaded, and an input file is never overwritten —
results land next to it as `name.ext`, or `name (2).ext` when that is taken.

## Using it

| While dragging over the window | Opens |
|---|---|
| **Shift** | the conversion wheel |
| **Ctrl** | the tools wheel (compress, trim, split, crop, resize, speed, merge, extract, zip…) |
| **Alt** | the actions wheel (copy, copy path, copy to…, move to…, show in Files, terminal) |

The wheel stays open until you drop. Drop on a slice to run it, or on the hub to cancel.
A drop without a wheel puts the files on the **shelf**.

**Wayland:** a window is not told about keys pressed while a drag from *another* app hovers it.
So while dragging, three chips appear at the bottom of the window; drag over one to open its wheel.
Modifier keys work when dragging from the shelf, and on X11.

**Keyboard:** select files on the shelf and press **Enter**. Arrows choose a slice, **Tab** switches
wheels, **Enter** runs, **Esc** cancels. **Delete** removes files from the shelf. Shelf rows can be
dragged out to a file manager or back onto the window.

A slice is grey when a program it needs is missing; hover it to see which.

## What it converts

| From | To | Through |
|---|---|---|
| Images: JPG PNG WebP HEIC AVIF TIFF BMP GIF SVG | JPG PNG WebP AVIF HEIC TIFF BMP PDF | ImageMagick |
| Audio: MP3 M4A WAV FLAC OGG Opus AIFF WMA AAC | MP3 M4A WAV FLAC OGG Opus AIFF | ffmpeg |
| Video: MP4 MOV MKV WebM AVI WMV MPG FLV | MP4 MOV MKV WebM AVI GIF MP3 | ffmpeg |
| PDF | PNG / JPG (every page, 300 DPI), TXT, DOCX | poppler, LibreOffice |
| Documents: DOCX ODT RTF MD TXT HTML EPUB | DOCX ODT PDF MD TXT HTML EPUB | pandoc, LibreOffice |
| Spreadsheets: XLSX XLS ODS CSV TSV | XLSX ODS CSV TSV PDF | LibreOffice |
| Slides: PPTX PPT ODP | PDF PPTX ODP PNG JPG | LibreOffice, poppler |
| Subtitles: SRT VTT | SRT VTT TXT | ffmpeg, built-in |
| Archives: ZIP TAR TAR.GZ RAR 7Z | ZIP TAR TAR.GZ | unzip, tar, unrar, 7z |

## Tools

| Files | Tools | Through |
|---|---|---|
| Images | Compress, Resize…, Crop…, Rotate, Strip metadata | ImageMagick |
| Video | Compress, Trim…, Split…, Speed…, Thumbnail…, Half size, Mute | ffmpeg, ffprobe |
| Audio | Compress, Trim…, Split…, Speed…, Fade…, Normalize, Mono | ffmpeg, ffprobe |
| PDF | Merge (2+), Compress, Split pages, Pages…, Rotate… | Ghostscript, poppler, qpdf |
| Archives | Extract | unzip, tar, unrar, 7z |
| Anything | Zip | zip |

A tool whose name ends in **…** asks for its settings in a small window first, filled in from the
file (its length, size or page count) and checked before it runs:

| Tool | Asks for | Writes |
|---|---|---|
| Trim… | start and end (`1:30`, `90.5`) | `name (trimmed).ext` |
| Split… | N equal parts, or a part every X seconds | `name (parts)/name 1.ext`… |
| Speed… | 0.25× to 4× (picture and sound) | `name (2x).ext` |
| Thumbnail… | the time of the frame | `name (thumbnail).jpg` |
| Fade… | fade-in and fade-out seconds | `name (faded).ext` |
| Crop… | a centred ratio (1:1, 4:3, 3:2, 16:9, 9:16, 4:5) or an exact rectangle | `name (cropped).ext` |
| Resize… | a percentage, or a box to fit inside | `name (50%).ext`, `name (800x600).ext` |
| Pages… | first and last page | `name (pages 2-5).pdf` |
| Rotate… (PDF) | 90°, 180° or 270° | `name (rotated).pdf` |

Cuts are re-encoded so they land exactly where asked; only PCM audio (WAV, AIFF) is stream-copied,
which is exact and keeps its bit depth. Pages… uses qpdf when installed and poppler otherwise;
Rotate… needs qpdf.

## Building

```sh
# Debian / Ubuntu
sudo apt install libgtk-4-dev ffmpeg imagemagick pandoc poppler-utils qpdf libreoffice zip unzip ghostscript
cargo build --release
./target/release/clementine [files…]
```

GTK 4.14 or newer. The converters are optional; install the ones you need.

```sh
cargo test                               # fast unit tests
cargo test -- --ignored --nocapture      # every conversion on real sample files
```

## Layout

| File | What it holds |
|---|---|
| `src/catalog.rs` | File kinds, and which conversions / tools / actions each selection gets |
| `src/params.rs` | Settings for the tools that ask (trim range, crop, speed…) and their checks |
| `src/params_ui.rs` | The small window that asks for those settings |
| `src/backend.rs` | Turns an op into commands, names outputs, runs them off the UI thread |
| `src/wheel.rs` | The radial menu: state, hit testing, drawing |
| `src/actions.rs` | The actions wheel: clipboard, copy/move, file manager, terminal |
| `src/main.rs` | The window, drag and drop, keyboard |

## Not yet

- Remappable modifier keys (Settings)
- A visual range picker (waveform / frame strip) for Trim and Crop
- Flatpak and `.deb` packaging
- A global drop target outside the window (possible on X11 only)
