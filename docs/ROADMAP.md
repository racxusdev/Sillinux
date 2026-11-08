# MVP roadmap

Each milestone has an explicit, testable exit criterion. Anything that cannot
be verified is kept out of the critical path.

## M0 — transport and job encoding ✅ (done)

Delivered:

- `silhouette-driver`: byte-exact GPGL encoder, `Job`, `Transport` trait,
  optional libusb transport.
- `golden_square_cut()`: the exact 19-command job that cut a closed 20 mm
  square on a Cameo 5 V1.07 (`re_session.md`, Round 6).
- Golden tests asserting each command byte-by-byte, including a regression test
  for the "ASCII digits, not raw bytes" class of error.

Exit criterion: `cargo test` passes and the job is byte-identical to the
recorded capture. **Met.**

## M1 — decode `.studio3` geometry

**Met.** The container and the vector payload decode: polyline node runs
(`create_line` / `create_rectangle`, with the validated affine matrix) and
bezier contours (`create_curve`, node lengths 17/25/33, flattened at 0.05 mm).
Raster designs (`tty`, `topo-4x`) map their pixel-frame contours with
the repeated/aligned raster transform. Verified by `cargo test --all` (fixtures
+ unit tests) and by the GUI overlay on `tty`/`multi-contornos`.

Plan:

1. Produce a minimal fixture: one rectangle, no raster, saved by Silhouette
   Studio itself.
2. Diff against an empty document; isolate the coordinate floats.
3. Recover the tag/length framing around them (the serializer is the same one
   that writes the metadata strings).
4. Implement `silhouette_core::geometry` returning paths in SU.
5. Validate: for every design, render the decoded paths to SVG and compare
   visually with the embedded thumbnail.

Exit criterion: at least 8 real documents decode to paths whose bounding box
matches the thumbnail within 1 %.

## M2 — SVG import and cut pipeline

- Parse SVG (paths, transforms, units) with `kurbo` / `usvg`.
- Offset/outline generation for closed shapes.
- Map design → SU, apply origin and mat type (`TG`).
- Emit `Job` with one command per write.
- Material presets (force/speed/depth) for the common Silhouette materials.

Exit criterion: cut a 100 mm calibration square and measure it with a ruler to
within 0.5 mm.

## M3 — UI (`silhouette-ui`)

**Implemented** (`cargo run -p silhouette-ui --features gui --bin silhouette-gui`,
optional path argument opens a design):

- Canvas with wheel zoom / drag pan / double-click fit, artwork, cut lines,
  registration-mark overlay and path selection.
- Layer list with per-path visibility; document info.
- Design placement (X/Y offset, scale) applied to art, cut and print sheet.
- Registration-mark settings (3/4 corners, inset).
- Material presets (paper/vinyl/card), tool, speed, force, blade depth.
- Cut-order preview animation.
- Export SVG, print sheet PDF (marks + art + paths), dry-run and send-to-cutter
  with a confirmation window.
- Mandatory registration scan before cutting (when enabled), fail-closed.
- Independent `M1` / `M2` cut targets, duplicate-USB refusal, two machine
  slots, USB and Bluetooth (rfcomm) transports.
- Per-element cut jobs with an `M` lift between elements and a final
  `M0,0 / J0 / FN0` return block; long paths are paced.
- Printer queue selection (auto-preferring a queue that exposes the Epson
  driver options), print toggles for art / marks / cut
  lines, test page and job-id status.
- Epson print controls: `cupsPrintQuality`, `ColorModel`, `print-color-mode`
  and `MediaType` from the installed driver (`lpoptions -l`). RGB forces
  `print-color-mode=color`; the driverless queue otherwise defaults to
  monochrome.
- Snapping for every element (paths, rasters and layers) to the grid and to
  alignment guides, with visible guides and an adjustable grid.
- Per-layer editing: each layer has its own colour, visibility, node editing
  and group membership; selecting a layer edits only that layer.
- Colour effects per element (hue, saturation, vibrance, contrast, brightness)
  with live preview, plus per-layer colour strokes on the canvas.
- Keyboard shortcuts: `V/N/T/I/H` tools, `Esc`, `Ctrl+A`, `Ctrl+G`,
  `Ctrl+Shift+G`, arrow nudge (1 mm, `Shift` 10 mm), `Delete`/`Backspace`,
  `Ctrl+0`, `F1`.
- Raster vectorization: outline (silhouette) or Sobel edge lines, per-contour
  layers, threshold/simplification/minimum-length refinement, and raster-based
  inward/outward offset that handles concave shapes. Offset is previewed as a
  ghost before applying; tracing replaces the previous result and can be
  deleted explicitly. The `Imagem` tool moves the raster and its traced lines
  together.
- Registration marks use Studio geometry by default: 5 mm filled square,
  20 mm L arms, 0.3 mm stroke (`TB51,400 TB53,6`). The scan is fail-closed and
  its reply is logged to `~/.cache/sillinux/cut.log`; «Testar marcas» reads the
  marks without cutting. Cuts are ordered top-to-bottom, closed contours start
  at their topmost point, the area is declared with `\0,0 Z{h},{w}`, and
  `FE0,0` keeps the blade down between points.

Exit criterion: a user can open a `.studio3`, adjust a setting, preview, and
cut without touching the CLI. See [`UI_DECISION.md`](UI_DECISION.md).

## M4 — packaging

- `AppImageBuilder.yml` producing a self-contained AppImage.
- udev rule installation and `usblp` guidance.
- Tested on Ubuntu 22.04+ and Arch.

Exit criterion: AppImage runs on a clean Ubuntu 22.04 VM and on Arch, lists the
Cameo through `inspect`/`cut --dry-run`, and cuts the golden square.

## Explicitly out of scope (for now)

- BLE transport (classic Bluetooth SPP over rfcomm is supported).
- Windows/macOS.
- `.studio3` **writing** (only reading).

## Risks

| Risk | Impact | Mitigation |
|---|---|---|
| Geometry schema unknown | M1 blocks the whole product | minimal fixtures + binary serializer analysis |
| Force/depth semantics partial | wrong cut depth | start conservative (depth 1, force 4) and calibrate per material |
| `U` meaning unclear | cannot trust reported bounds | do not use for control flow; rely on `TG` + explicit geometry |
| Multi-command transfers | partial execution | one write per command (enforced by `Job`) |
| Cutter command buffer | later elements or long paths truncated | one job per element, paced writes, wait between elements |
| Registration scan | no cut when marks are not found | scan is fail-closed; printed marks and `TB51`/`TB53`/`TB123` use the same spec |
| Status byte | reports media sensor, not motion on some firmware | paced writes plus a settle delay; do not require status 1 |
| libusb dependency | packaging | isolated behind the `usb` feature; pure-Rust core |
| Driverless Epson queue is 360 dpi only | faint banding on some solid colours | keep the PDF RGB, print at `cupsPrintQuality=High`; a vendor ESC/P-R PPD with higher resolution would be required for more |
