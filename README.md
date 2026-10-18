# Clementine

A zero-click, offline file converter for Linux. Drag a file onto the window, tap a modifier,
and drop it on a format. Inspired by [Tangerine](https://tangerineformac.com/) for Mac.

![The conversion wheel](docs/wheel.png)

Everything runs on your computer: nothing is uploaded, and an input file is never overwritten —
results land next to it (or in a folder you choose) as `name.ext`, or `name (2).ext` when that is taken.

## Using it

| While dragging over the window | Opens |
|---|---|
| **Shift** (default) | the conversion wheel |
| **Ctrl** (default) | the tools wheel (compress, trim, split, crop, resize, speed, merge, extract, zip…) |
| **Alt** (default) | the actions wheel (copy, copy path, copy to…, move to…, show in Files, terminal) |

The wheel stays open until you drop. Drop on a slice to run it, or on the hub to cancel.
A drop without a wheel puts the files on the **shelf**.

**Wayland:** a window is not told about keys pressed while a drag from *another* app hovers it.
So while dragging, three chips appear at the bottom of the window; drag over one to open its wheel.
Modifier keys work when dragging from the shelf, and on X11.

**Keyboard:** select files on the shelf and press **Enter**. Arrows choose a slice, **Tab** switches
wheels, **Enter** runs, **Esc** cancels. **Delete** removes files from the shelf. Shelf rows can be
dragged out to a file manager or back onto the window.

**Settings** (the gear in the header bar) change which modifier opens which wheel (Shift, Ctrl, Alt or
Super, each on one wheel only), whether results go next to the original or into one folder, and whether
results are added to the shelf. Changes apply at once and are saved to
`~/.config/clementine/config.json` (`$XDG_CONFIG_HOME` is honoured); a missing or broken file means defaults.

A slice is grey when a program it needs is missing; hover it to see which.

**Activity.** Each conversion gets a row with a progress bar and a count such as `2 of 5 · 43%`
(file 2 of 5, 43% of the whole job). ffmpeg reports how far it has got; other programs only say
which file and step they are on, so the bar pulses. The **×** button cancels: the running program
(and anything it started, such as LibreOffice's `soffice.bin`) is stopped, the unfinished file is
removed, and files already finished are kept. The row then says **Cancelled**, **Failed** (hover for
why) or offers **Show**. Closing the window while conversions run asks whether to cancel them and
quit, or keep running. A conversion that ends while the window is in the background sends a
desktop notification.

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

## From the command line

```sh
clementine --wheel FILE…               # the window, with the Convert wheel already open (keyboard)
clementine --convert png FILE…         # convert, print each result's path
clementine --tool compress FILE…       # run a tool, print each result's path
clementine --tool trim --start 0:02 --end 0:10 clip.mp4
clementine --list FILE…                # what the wheels offer for these files
clementine --help
```

`--list` prints one option per line as `id<TAB>label`, e.g. `convert:png` tab `PNG`, or `tool:compress` tab `Compress`,
followed by `<TAB>params` for a tool that takes settings and `<TAB>unavailable: missing gs` when a
program it needs is not installed. Tool names are the same for every kind of file: `compress` is
image, video, audio or PDF compression depending on the files, `rotate` turns an image 90° or asks
`--degrees` for a PDF, `split` splits a PDF into pages or audio/video by `--parts`/`--every`.

Tools that ask for settings in the window (Trim…, Resize…, Crop…, Speed…, Split…, Fade…,
Thumbnail…, Pages…, Rotate… for PDFs) take them as flags; `clementine --help` lists them. A flag left
out keeps the dialog's default (resize 50%, split in 2, speed 2×…), and the values are checked the
same way the dialog checks them. Results go where Settings say. `--notify` also reports the result
as a desktop notification (through `notify-send`), which is how the file manager menus give feedback.
Exit status: 0 on success, 1 when a conversion fails, 2 on a usage error, 130 when cancelled
with Ctrl-C (the running converter is stopped and its unfinished output removed). `--wheel` hands the files
to a running Clementine when there is one.

## File managers

On Wayland nothing can float over every app the way Tangerine does on macOS, so Clementine lives in
the file manager's right-click menu instead. The .deb installs all of these; each does nothing
unless its file manager is installed.

| File manager | What you get | Installed to (.deb) | Needs |
|---|---|---|---|
| GNOME Files (Nautilus) | **Clementine ▸** Open in Clementine wheel…, Convert to ▸ (the wheel's formats), Tools ▸ | `/usr/share/nautilus-python/extensions/clementine.py` | `python3-nautilus` |
| Caja (MATE) | the same menu, same file | `/usr/share/caja-python/extensions/clementine.py` | `python3-caja` |
| Dolphin (KDE) | **Clementine ▸** Open in wheel, Zip, and per type: images → PNG/JPG/WEBP/PDF, audio → MP3/M4A/FLAC/WAV, video → MP4/WEBM/GIF/MP3, PDF → PNG/JPG/TXT, Compress | `/usr/share/kio/servicemenus/io.github.sphanx.Clementine*.desktop` | KDE Frameworks 6 |
| Nemo (Cinnamon) | Open in Clementine wheel…, Convert to PNG/JPG/MP3/MP4, Compress | `/usr/share/nemo/actions/clementine-*.nemo_action` | — |
| Thunar (Xfce) | custom actions in a Clementine submenu | not installable: see below | Thunar 4.18+ for the submenu |
| GNOME Files, no python | Scripts ▸ Clementine opens the wheel | not installable: see below | — |

Restart the file manager after installing (`nautilus -q`, `caja -q`, `nemo -q`); Dolphin picks the
menus up when it next opens a menu. The Nautilus menu asks `clementine --list` for the selection, so it
always matches the wheel; Dolphin, Nemo and Thunar menus are fixed lists, and a choice that does not
fit the selection (PNG on a PNG) ends in a "failed" notification.

**Per-user installs** (from a source checkout, or for Flatpak):

```sh
# Nautilus extension (or ~/.local/share/caja-python/extensions/ for Caja)
mkdir -p ~/.local/share/nautilus-python/extensions
cp data/nautilus/clementine.py ~/.local/share/nautilus-python/extensions/
# Nautilus script fallback: right-click > Scripts > Clementine
mkdir -p ~/.local/share/nautilus/scripts
cp data/nautilus-scripts/Clementine ~/.local/share/nautilus/scripts/
# Dolphin: user service menus must be executable
mkdir -p ~/.local/share/kio/servicemenus
install -m 755 data/kio/servicemenus/*.desktop ~/.local/share/kio/servicemenus/
# Nemo
mkdir -p ~/.local/share/nemo/actions
cp data/nemo/actions/*.nemo_action ~/.local/share/nemo/actions/
```

**Thunar** keeps every custom action in one per-user file, `~/.config/Thunar/uca.xml`, so no package
can add to it cleanly. Close Thunar (`thunar -q`) and paste the `<action>` elements from
`data/thunar/uca.xml` (the .deb ships it as `/usr/share/clementine/thunar/uca.xml`) inside the
`<actions>` element of yours, or re-create them in *Edit ▸ Configure custom actions…*.

**Flatpak** cannot install anything into the host's file managers. Copy the files as above, then
point them at the sandbox: the Nautilus/Caja extension and the Nautilus script read
`CLEMENTINE_COMMAND` (and the extension falls back to `flatpak run io.github.sphanx.Clementine` by
itself when no `clementine` is on `PATH`); in the Dolphin, Nemo and Thunar files replace
`clementine ` in each `Exec=`/`<command>` with `flatpak run io.github.sphanx.Clementine `:

```sh
sed -i 's|Exec=clementine |Exec=flatpak run io.github.sphanx.Clementine |' \
    ~/.local/share/kio/servicemenus/io.github.sphanx.Clementine*.desktop \
    ~/.local/share/nemo/actions/clementine-*.nemo_action
```

The sandbox only sees your home folder, so files elsewhere cannot be converted from the Flatpak.

## Install

**Debian / Ubuntu (.deb).** Download `clementine_*.deb` from the GitHub releases page, or build it
(see below), then:

```sh
sudo apt install ./clementine_0.1.0-1_amd64.deb
```

apt pulls in GTK 4 and recommends the converters (ffmpeg, ImageMagick, pandoc, poppler-utils, zip,
unzip). LibreOffice, unrar, p7zip, ghostscript and qpdf are suggested, not installed.

**Flatpak.** The manifest is `flatpak/io.github.sphanx.Clementine.yml`; it is not on Flathub yet.

```sh
flatpak install --user flathub org.gnome.Platform//51 org.gnome.Sdk//51 \
    org.freedesktop.Sdk.Extension.rust-stable//26.08
flatpak-builder --user --install --force-clean build-dir flatpak/io.github.sphanx.Clementine.yml
```

The Flatpak bundles ffmpeg (with MP3 via lame), pandoc, zip, unzip and poppler-utils. It does **not**
bundle ImageMagick, LibreOffice, unrar, p7zip, ghostscript or qpdf, so those conversions stay greyed out
inside the sandbox. Clementine does not call host programs through `flatpak-spawn`.

**From source.** See *Building*.

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
| `src/progress.rs` | Progress reports, the cancel token, parsing ffmpeg's `-progress` output |
| `src/jobs_ui.rs` | Activity rows: progress bar, cancel button, the close-while-running question |
| `src/wheel.rs` | The radial menu: state, hit testing, drawing |
| `src/actions.rs` | The actions wheel: clipboard, copy/move, file manager, terminal |
| `src/config.rs` | Settings: the JSON file, defaults, modifier assignment |
| `src/settings_ui.rs` | The Settings window |
| `src/cli.rs` | The command line: `--convert`, `--tool`, `--list`, `--wheel` |
| `src/main.rs` | The window, drag and drop, keyboard |
| `data/nautilus/`, `data/kio/`, `data/nemo/`, `data/thunar/`, `data/nautilus-scripts/` | File manager menus |

## Not yet

- A visual range picker (waveform / frame strip) for Trim and Crop
- A global drop target outside the window (possible on X11 only); the file manager menus stand in for it
