# Sillinux

[Português](README.md) · **English**

A **native Linux tool** (Rust, no Wine) for **Silhouette / Graphtec** cutters:
import images and SVG, edit in layers, **vectorize** with four engines
(including Studio-quality curves), configure and send the cut over **USB**
directly using the **GPGL** protocol (1 unit = 0.05 mm) or over **Bluetooth**
without root. Includes **registration marks** for the *print & cut* flow with
CUPS printing.

Two programs:

| binary | what it does |
|---|---|
| `silhouette` | command line: inspect `.studio3`/SVG, print sheet, cut with registration marks, golden test |
| `silhouette-gui` | graphical window: zoom/pan canvas, image and vector layers, vectorization, text, print & cut, cut with progress |

## AppImage (portable, no install)

```bash
chmod +x Sillinux-x86_64.AppImage
./Sillinux-x86_64.AppImage
```

No FUSE? `./Sillinux-x86_64.AppImage --appimage-extract-and-run`.

The AppImage targets **glibc 2.17** (Ubuntu 14.04+/Debian 8+/CentOS 7+ and
newer) and the **libusb is compiled in, statically** — the binary depends only
on glibc; no libudev, libusb or X11 required on the target system. Build with:

```bash
./packaging/build-appimage.sh   # needs cargo-zigbuild + zig on PATH
# output: dist/Sillinux-x86_64.AppImage
```

## USB permissions (required, once)

Without this the device lists but will not open. From the GUI: the
**"Install udev rule (USB, once)"** button under **Connections** (asks for the
password via polkit). From the terminal:

```bash
sudo cp packaging/udev/99-silhouette.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules && sudo udevadm trigger
# unplug and plug the machine back
```

The rule grants the desktop user access (`uaccess`, with a `plugdev` fallback)
to any Silhouette/Graphtec device (`VID 0b4d`).

**Bluetooth needs none of this**: the GUI opens the SPP serial port with a
direct RFCOMM socket as the unprivileged user — no root, no `rfcomm`, no
polkit.

## The GUI

### Vectorization (the heart of the app)

Four methods, all using a mask by **colour distance from the background** —
coloured artwork on a light (or dark) background is traced whole, including
pale pieces any luminance threshold would miss:

| Method | what it does |
|---|---|
| **Auto (recommended)** | vtracer + Otsu threshold + automatic tolerance; tunes itself |
| **Outline only** | marching squares by threshold, holes included, uniform polish |
| **All lines (edges)** | Sobel skeleton for line art |
| **Studio quality (curves)** | vtracer (colour clustering + Bézier curve fitting) |

Features around vectorization:

* **⬚ Trace area** — drag an area over the image and trace only it at **double
  resolution**: how you rescue small loose details the full trace missed.
* **Per-image colour** — hue, saturation, vibrance, contrast and brightness per
  image (not global); the cut follows the adjusted colour.
* **Automatic polish** — uniform resampling + corner-preserving smoothing +
  roughness-proportional auto-correction: smooth curves, crisp corners.
* **Speckles (%)** — Studio-style noise removal; **Outer contour only** and
  **baked external offset** as options.
* **Live preview** on a worker thread — the UI never freezes.
* **⌒ Round up** — post-trace: simplify the rounded way, not square, keeping
  real corners.
* **Node editing** — add (double-click), remove (right-click) and drag points;
  **Split into objects** to ungroup piece by piece; **clip inside/outside** a
  dragged area.

### Layers and images

* **Multiple images**, each as a layer: show/hide, reorder (▲▼), delete; the
  **vector is born independent** from the image (and can be **detached** later
  from the right-click menu).
* **Text** with **custom fonts** (import TTF/OTF — they persist), each font
  previewed in itself, live colour and size.
* **60-level undo/redo** (Ctrl+Z / Ctrl+Y) — including restoring **deleted
  images**; `Ctrl+D` duplicates; `Delete` deletes; `Ctrl+G` groups.
* **PT / EN** language switcher in the toolbar; preferences saved to
  `~/.config/sillinux/prefs.json`.
* **USB hotplug detection**: plug/unplug the machine and the app tells you.

### Cutting and machines

* **Two simultaneous machines** (USB or Bluetooth) with live firmware/status,
  cutting on a separate thread with real progress.
* **Print & cut**: Studio-pattern marks (10×10 mm square + "L"), automatic
  `TB123` search (or manual `TB23`), print-distortion compensation; print
  sheet exported and sent via **CUPS** (Epson and generic).
* Session settings saved automatically (trace method, printing, cutting,
  printer, language).
* Connection actions use direct sockets/libusb — **no `bluetoothctl`, no
  `rfcomm`, no sudo** in the Bluetooth flow.

### CLI

```bash
silhouette inspect file.studio3        # page, paths, embedded image
silhouette svg file.studio3 -o out.svg
silhouette cut design.svg --page 210x297 --force 4 --speed 3
silhouette cut sheet.svg --regmarks --reg-width 180 --reg-length 230
silhouette regmarks --page A4 --square-mm 10 --arm-mm 10 -o marks.svg
silhouette print-sheet design.png -o sheet.pdf
silhouette golden                      # validated 30 mm square test
silhouette probe                       # firmware/status
```

`cut --dry-run` validates the whole pipeline and writes the GPGL program as
text (`FG`, `TB50,0`, `M/D`, `TB123`…) without touching the machine.

## Building

```bash
cargo build --release        # binaries in target/release/
cargo test                   # 31 core + 10 driver + GUI
```

libusb is compiled in (`rusb` with `vendored`) — no `libusb-1.0-dev` needed.
For the portable AppImage (glibc 2.17): `cargo-zigbuild` + `zig 0.14` on
`PATH` and `./packaging/build-appimage.sh`.

## Supported machines

**Cameo 5** (`0b4d:1140`) is the hardware where the protocol was **validated
with a real cut** (golden square, regmarks, status). The `0b4d` family
(Graphtec/Silhouette: Cameo 1–5, Portrait, SD, Craft Robo) speaks the same
GPGL and should work — reports welcome.

## Layout

```
crates/silhouette-core    GPGL encoder, geometry, tracing (marching squares + vtracer), marks
crates/silhouette-driver  transports: USB (static libusb), Bluetooth RFCOMM, serial + session
crates/silhouette-app     CLI
crates/silhouette-ui      GUI (egui), import (SVG/PDF/PSD/raster/Studio3), i18n, preferences
packaging/                AppImage build, udev rule, desktop, metainfo
docs/                     format notes and GPGL reverse-engineering
```

## Honest limitations

* **Print & cut**: always print at **100 %**; the `TB123` mark search is
  implemented but **not yet physically validated** on the Cameo 5 (the golden
  square and status were validated with a real cut).
* **`.studio3`**: reads paths and the largest embedded image; not everything
  the Studio saves.
* **No DXF** and no SVG `<text>` — use the GUI Text tool for text.
* **Cameo 4/5 multi-toolholder**: sends to tool 1 (`J1`).
* **Score/sketch** not yet exposed in the GUI (the protocol has `FN`/tool 2).

## License

MIT OR Apache-2.0. Protocol facts come from GPL project documentation
(inkscape-silhouette) and our own reverse-engineering sessions (`docs/`); the
implementation is original.
