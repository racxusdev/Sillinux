# `.studio3` container: what is verified

Everything below was measured on the 51 `.studio3` files in this repository.
Commands used are shown next to the claims. Items that are not proven are under
**Unverified** and must not be relied on.

## 1. Outer header

```text
offset  size  content
0x00    13    "silhouette05;"            (ASCII)
0x0d     3    00 00 00                   (verified on all 51 files)
0x10    128   16 × little-endian u64     (header fields, semantics unknown)
0x90           ... zero padding ...
0x400         nested "silhouette05;" document
```

Verification:

```bash
python3 - <<'PY'
import struct, glob
for f in glob.glob("**/*.studio3", recursive=True):
    d = open(f, "rb").read()
    assert d[:13] == b"silhouette05;"
    assert d[13:16] == b"\0\0\0"
    assert d[1024:1037] == b"silhouette05;"
print("ok")
PY
```

Result: every one of the 51 files passes all three assertions.

**Header fields.** The 16 `u64` values are exposed verbatim as
`Container::header`. Their meaning is **not** known. Values seen:

- field 3 is `42656` in 45 files and `0` in 4;
- field 7 is always `4`;
- fields 10..13 are `94, 94, 0, 94` in most files, but calibration/font-template
  files carry much larger values (`560314`, `624224`, `920438`, …).

Because they are not constant, they are **not** treated as invariants.

## 2. Metadata strings

From the nested document up to the first embedded image, the file contains
`u32`-LE length-prefixed UTF-8 strings:

```text
0b 00 00 00   "Print Paper"
07 00 00 00   "(Light)"
0f 00 00 00   "PrintPaperLight"
2c 00 00 00   "Standard light-weight printer or copy paper."
```

They carry the material name and description, UI settings (`square`,
`12x12ff`, `constant:a4`, `rounded`, `cut`), layer names (`Camada 1`) and
32-character hex identifiers (e.g. `cde5afba1c65d593c756b0fae692163d`).

`silhouette-core` extracts them with a validated heuristic (printable ASCII,
maximum length 4096, minimum letter density) so that random binary runs are
rejected. A typical file yields ~400 strings.

## 3. Embedded rasters

Every one of the 51 files contains at least one PNG or JPEG, with the PNG
signature `89 50 4e 47 0d 0a 1a 0a`, or a JPEG starting `ff d8 ff`.

Examples from `tentativas anteriores/Frame_paper.studio3`:

```text
PNG 564x797 at 30279..31838
PNG 512x512 at 31846..64035
```

`silhouette-core` validates each image (PNG: IHDR + IEND; JPEG: SOI, marker
walk, SOF dimensions, EOI) instead of trusting the signature alone. Images are
returned with their exact byte ranges.

## 4. Trailing JSON metadata

At the end of 34 files there are three `u64`-LE length-prefixed chunks whose
payload is standard base64 wrapped with CRLF every 76 characters. Decoded, each
is:

```json
{"root_uuid":"309528981f064d78145a177096041379","children":{}}
```

The stored length includes the CRLF bytes. The 15 files without these chunks
are the resource/calibration files that also have header field 3 = 0 (except
`test_cut2.studio3`).

## 5. Geometry — verified for polylines and rectangles

The vector data is a binary object graph, but the **polyline vertices** have a
simple, verified encoding:

```text
01 11 00 00 00     marker (5 bytes)
<x: f32 LE>        millimetres
<y: f32 LE>        millimetres
```

- Origin is the **page top-left corner**, **y points down**.
- Consecutive vertices of the same path are **16–24 bytes apart**; a new path
  starts after a larger gap. `silhouette-core` splits groups at a gap > 32
  bytes.
- The object kind (`create_line`, `create_rectangle`, `create_curve`, …) is a
  length-prefixed ASCII string usually within a few hundred bytes **after** the
  last vertex. `silhouette-core` looks 512 bytes ahead; if no `create_*` token
  is found the path is reported as `PathKind::Other` rather than guessed.
- Rectangles are closed; lines are open. A path is also marked closed when its
  last point equals its first.

### 5.0 Object transform — verified

Vertices are in the object's **local** frame. After the vertex list, each object
stores a row-major 3×3 affine matrix (36 bytes) and then its centre in page
millimetres (two `f32`):

```text
<vertices> <a b tx c d ty 0 0 1: 9 × f32> <centre x: f32> <centre y: f32> <…>
```

Page coordinates are obtained with `p_page = M · p_local`:

```text
x_page = a·x_local + b·y_local + tx
y_page = c·x_local + d·y_local + ty
```

The matrix was found in three independent objects and its effect matches the
stored centre exactly:

| Object | Matrix | Local bbox centre | Page centre (`matrix + 36`) |
|---|---|---|---|
| rectangle fixture | `[1, 0, -0.4371, 0, 1, 9.3965]` | `(105.437, 139.104)` | `(105.0, 148.5)` |
| `Frame_paper` line | identity | `(105.839, 48.536)` | `(105.839, 48.536)` |
| `LeatheretteTray` freehand | `[0.9465, 0, 4.1198, 0, 0.9167, -11.2438]` | `(24.130, 32.849)` | `(26.959, 18.867)` |

`silhouette-core` scans for a 9-float window with bottom row `(0, 0, 1)` and a
non-zero determinant, stops at the next `create_*` marker, and accepts a
candidate only when the transformed bbox centre matches the stored centre within
0.05 mm. This rejects accidental matrices in neighbouring metadata.

Consequence for cutting: **ignoring the matrix shifts the design by
`(tx, ty)`** — the rectangle fixture landed `(-0.44, -9.40)` mm from its intended
position, which is exactly the physical offset reported by the owner.

Evidence (bytes and outputs reproduced in `re_session.md`, Rounds 8–10):

| Fixture | Result |
|---|---|
| `vazio  estilo folha A4.studio3` | 0 paths |
| `retangulo centralizado  estilo folha A4.studio3` | 1 rectangle, 4 vertices, **72.550 × 99.647 mm** at `(68.725, 98.677)`, centre `(105.0, 148.5)` (A4 centre) |
| `Frame_paper.studio3` | 16 `create_line` paths, 32 vertices (identity transforms) |
| `caixa-1.studio3` | 21 `create_line` paths, 42 vertices, plus 3 `Other` groups (two `create_curve` objects not decoded) |

The rectangle fixture's four stored (local) vertices are:

```text
(69.1621,  89.2801)
(69.1621, 188.9269)
(141.7120, 188.9269)
(141.7120,  89.2801)
```

Applying `(x - 0.4371, y + 9.3965)` gives the page rectangle
`(68.725, 98.677)…(141.275, 198.323)`.

Cross-check against the embedded 512×512 thumbnail: its outline spans
x 250..437, y 254..509 px. The scales computed independently from width and
height are 2.5775 and 2.5590 px/mm (0.72 % apart), i.e. a single uniform scale
fits all four edges. This confirms both the size and the y-down orientation.

### 5.1 Not decoded

- **`create_curve`** uses a different, delta-encoded bezier form
  (`01` + x, y + control-point offsets). It is stored far from its `create_*`
  string in some files, so curve nodes are currently reported as `Other` or
  omitted.
- **Text shapes** (`create_textshape`) are not converted to outlines.
- **Raster-only documents** contain no path objects at all: several real
  designs (`tty`, `multi-contornos`, `Novo-Projeto`, `topo-4x`,
  `caixa-2`, `molde topos`, `restauracao escuro`, `Final v2`) fall in this
  category. They must be traced (raster → contours) before they can be cut.

### 5.2 Page size — verified

Two `f32` LE values at offsets **1149** (width) and **1153** (height) hold the
page size in millimetres. Verified across all 51 `.studio3` files in the corpus:

| Stored value | Files | Meaning |
|---|---|---|
| `210.0 × 297.0` | 10 | A4 portrait |
| `297.0 × 210.0` | 1 | A4 landscape |
| `215.9 × 279.4` | 11 | US Letter |
| `279.4 × 215.9` | 1 | US Letter landscape |
| `304.8 × 304.8` | 6 | 12 × 12 in mat |
| `228.6 × 304.8` | 4 | 9 × 12 in mat |
| `177.8 × 127.0` / `127.0 × 177.8` | 3 | 5 × 7 in |
| `0.0 × 0.0` or `1.0 × 0.0` | 15 | calibration/template/font files |

Every decoded path in the checked vector fixtures fits inside the stored page
(`caixa-1`: max 251.1 × 287.2 mm on a 304.8 mm page). Objects may legitimately
sit partly outside the media — Silhouette allows dragging them off the page —
so decoded bounds are not clamped to the page. The parser returns `None` for the
page size when the fields are zero or outside 10..=2000 mm.

### 5.3 Object transform and centre — verified

After the vertex list, the record holds the 3×3 matrix described in §5.0 and
then the object's page centre. For the rectangle fixture:

```text
matrix   [1.0, 0.0, -0.437057,  0.0, 1.0, 9.396530,  0.0, 0.0, 1.0]
centre   (105.0, 148.5)                 # exact centre of A4
half     (36.274948, 49.823410)         # half of 72.55 × 99.647
```

The **centre is post-transform**: the local bbox centre is `(105.437, 139.104)`,
and applying the matrix gives exactly `(105.0, 148.5)`. The earlier reading of
these floats as an unverified anchor was wrong; they are the object's transform
and its resulting centre. Geometry still comes from the vertices, but only
**after** applying the matrix.

Note that compound/text objects can contain several vertex groups sharing one
stored matrix and centre; the centre then refers to the whole object, not to an
individual group. `silhouette-core` only applies a matrix when it validates
against a group's own bbox centre, so such groups are left untransformed and
classified `Other` (not cut).

## 6. What to do next

1. Decode `create_curve` (bezier): start from `caixa-1.studio3`, where two
   curve objects are present, and diff a curve-only fixture against an empty
   one.
2. Re-cut the rectangle fixture with the matrix applied and confirm it lands on
   the page centre (see §7 and `re_session.md`, Round 10).
3. Add contour tracing for raster designs — that, not geometry decoding, is
   what unlocks the user's current artwork.

## 7. Page coordinates → cutter coordinates

The verified GPGL job uses SU (1 SU = 0.05 mm) measured from the machine's own
origin. The conversion from decoded page coordinates is:

```text
x_su = round(x_mm * 20)
y_su = round(y_mm * 20)                  # default: y down
```

Hardware evidence (Round 9): the first cut of the rectangle fixture used the
**local** vertices — the object matrix was not applied yet — and the owner
reported the rectangle was "a little misaligned, higher up". The error direction
and magnitude match the missing translation exactly: local centre
`(105.437, 139.104)` vs intended `(105.0, 148.5)`, i.e. 9.40 mm too high and
0.44 mm to the right. With y-down this is the expected result, so the mirror
hypothesis (`--flip-y`, `y_su = round((page_height_mm - y_mm) * 20)`) is still
refuted.

After applying the matrix, the emitted job starts at page `(68.725, 98.677)`
and ends at `(141.275, 198.323)`, centred on A4. On hardware (Round 10) the
matrix fixed the size and the left/right position, but the shape still landed
**~2 mm too high** (owner's ruler measurement from the sheet's top edge). The
x axis needs no correction.

### 7.1 Empirical machine offset

`CutMapping::with_offset(x_mm, y_mm)` (CLI: `--x-offset-mm`,
`--y-offset-mm`) nudges the whole job after the page mapping. Positive `y`
moves the job **down**; the default is `(0, 0)`.

```bash
silhouette cut-file design.studio3 --yes --y-offset-mm 2
```

This is a **calibration**, not file-format data: it depends on media loading and
on where the machine origin sits, so it must not be baked into the geometry. A
ruler reading is good to about ±0.5 mm; if the value changes between loads, use
two marks at known page coordinates in one job (see Round 9) to measure the
offset and the scale independently of the final feed.

Caveats for precise measurements:

- the job ends with `L0 / \0,0 / M0,0 / J0 / FN0 / TB50,0`, so the blade lifts
  and returns to the cut origin (the top-left mark when scanning) before the
  media is released; the media can still shift slightly after the job;
- the rectangle is axis-aligned and symmetric, so this test proves the y
  direction and the scale, not the x handedness. Use an asymmetric shape to
  check mirroring.

For a sub-millimetre offset, cut two marks at known page coordinates in one job
and measure their distance on the sheet (independent of the final feed).

`silhouette cut-file <file> --dry-run` prints the exact GPGL commands without
touching the hardware; `--yes` performs the cut. Curves and unknown paths are
skipped, never approximated as chords.

### 7.2 Cut flow (GUI and `cut-file`)

Each element is sent as its own `M`/`D` job:

1. `ESC EOT` + prelude (`FN0`, `TG1`, tool, speed, force, `FY1`, `TF`, `FC`,
   `FE0`). There is **no** `SO0` here: the machine origin stays the media
   origin, and the scan (when enabled) moves it to the top-left mark.
2. Optional registration scan (`TB50,0`, `TB99`, `TB52,2`, `TB51`, `TB53`,
   `TB55,1`, `TB123`). If the scan fails, the cut is aborted before any `M`/`D`.
3. Cut area and continuous mode: `\0,0 / Z{h},{w} / L0 / FE0,0 / FF0,0,0`.
   `FE0,0` is the normal cut mode; `FE1` lifts the blade at every sharp angle,
   which on a traced contour looks like puncturing.
4. For every visible path, top-to-bottom, each closed contour rotated to start
   at its topmost point: one `M` (blade lifted) followed by its `D` segments.
   Consecutive points that round to the same SU position are dropped. Long
   paths are paced at 16 commands per 40 ms so the cutter's small command
   buffer does not truncate the element. The UI waits for the machine to become
   idle between elements, which also gives the blade time to lift.
5. One end block: `L0 / \0,0 / M0,0 / J0 / FN0 / TB50,0`. `M0,0` returns the
   carriage to the cut origin (the top-left mark when scanning).

`TB123` is used for the scan even when four marks are printed: the three
reference marks (top-left square, top-right L, bottom-left L) are the ones the
firmware documents. The fourth L is extra ink outside the scan path. `TB124`
appears in no verified Studio capture, so it is not used by default.

## 8. Printing the sheet with registration marks

`silhouette print-sheet` writes a one-page PDF whose **MediaBox is the exact
page size in millimetres** (A4 = 595.276 × 841.89 pt). Marks are PDF vector
paths; the embedded PNG artwork is an RGB `/FlateDecode` image XObject with
`Predictor 15`, composited over white. No raster printer driver is involved, so
the marks stay sharp.

```bash
silhouette print-sheet design.studio3 out.pdf \
  --inset-mm 10 --arm-mm 20 --stroke-mm 0.3
silhouette print-sheet design.studio3 out.pdf --print --printer NOME_DA_FILA
```

`--print` calls `lp` with `-o print-scaling=none -o fit-to-page=false`, so CUPS
does **not** shrink or enlarge the page. It also passes the Epson driver options
`cupsPrintQuality`, `ColorModel`, `print-color-mode` and `MediaType` (see
`lpoptions -l`); the GUI exposes the same values. The CLI prints the matching
`TB51`/`TB53`/`TB123` scan values; `cut-file --regmark` must be given the same
inset/arm/stroke.

A driverless Epson queue defaults to
`print-color-mode=monochrome` and `ColorModel=*Gray`, so a PDF that is RGB on
disk still prints in grayscale unless the job sets the mode explicitly. The app
maps RGB to `print-color-mode=color` and the gray modes to `monochrome` /
`auto-monochrome`, and sends `ColorModel` alongside. Both options are required
on this driver; `print-color-mode` alone was enough to fix colour output.

The GUI also prefers the queue that exposes `cupsPrintQuality` (the real Epson
driver) over a generic ESC/P queue for the same device, and refuses any queue
whose name contains `cameo` or `silhouette`.

The driverless Epson queue advertises only `printer-resolution-supported =
360dpi` (and `print-quality` normal/high). Some solid colours can show faint
banding at that resolution; this is a limit of the driverless raster path, not
of the PDF (which stays RGB). A vendor ESC/P-R PPD with higher resolutions
would be needed to improve it.

The default marks are a **5 mm filled square**, **20 mm L arms** and a
**0.3 mm stroke**, the geometry of Silhouette Studio and of the public
inkscape-silhouette renderer (`TB51,400 TB53,6`). The earlier 5 mm / 0.25 mm
marks were too small for the Cameo 5 sensor. `TB51` and `TB53` are derived from
the same spec (`arm × 20 SU`, `stroke × 20 SU`), so reprint the marks whenever
those values change.

### 8.1 Raster placement

The artwork's pixel frame is stored in the same object graph as the curves. In
`tentativas anteriores/tty.studio3` seven identical candidates were found:

```text
[[0.05707326, 0,          4.84730],
 [0,          0.05707333, 4.54251],
 [0,          0,          1      ]]
```

That maps the 3534 × 5000 px PNG to `201.70 × 285.37 mm` at `(4.85, 4.54)`, and
the non-white content bbox is `(14.09, 14.49)..(195.93, 283.31)` mm. With marks
at inset 10 mm, the 5 mm square and the L arms fall on white paper; this was
checked pixel-by-pixel against the decoded PNG.

`multi-contornos.studio3` has **no** repeated image transform. The CLI
therefore falls back to a fit placement: the largest image is scaled into the
page minus a 20 mm margin and centred. That file stores two 2828 × 4000 layers
(shadow then colour) and both are placed on the same rectangle, bottom-first.
`--art-inset-mm` changes the fallback margin; `--no-art` prints marks + decoded
vector paths only; `--marks-only` prints just the marks.

`raster_placement` is a heuristic: it accepts an axis-aligned, uniform scale
whose centre is on the page and whose transformed image is 20–150 % of the
page. A repeated matrix wins (tty: 7 identical candidates). A *single*
candidate is accepted only when it is the only group in the file
(`topo-4x`: scale 0.05528704, offset (0.8581, 1.0488)); when several
different candidates exist the result is ambiguous and the fit fallback is
used (`multi-contornos`: none).

### 8.2 Verification status

- `pdfinfo out.pdf` → `Page size: 595.276 x 841.89 pts (A4)` (verified locally).
- `pdftoppm -png -r 50 out.pdf preview` renders the marks and artwork correctly
  for `tty`, `multi-contornos` and the centre-rectangle fixture (verified
  locally; previews at `/tmp/opencode/sheet-*.png`).
- **UNVERIFIED — requires a physical print and scan**: whether the printer
  honours 100 % scale, whether the paper feed keeps the marks within the scan
  window, and whether `lp`/the queue accepts the A4 PDF without scaling. Do not
  cut until the printed sheet has been scanned once.
- The GUI now defaults to scanning before cutting. The scan is fail-closed: a
  missing or unreadable mark aborts that machine's job before the first `M`.
- The GUI auto-selects a print queue that exposes the Epson driver options
  (`cupsPrintQuality`) and refuses any queue whose name contains `cameo` or
  `silhouette`. Generic ESC/P queues print grayscale and should be avoided.
  `lp` acceptance is not proof of physical printing; the UI keeps the job
  id and offers a status check.

Raster designs are now cuttable: `create_curve` contours decode as bezier node
runs (anchors + in/out handle deltas, lengths 17/25/33), are flattened at
0.05 mm and, in raster documents, mapped from the artwork pixel grid with the
same raster transform. `--paths` and the GUI draw them; `cut-file` includes
them. Physical print → scan → cut of a raster design is still **UNVERIFIED**.

### 8.3 Raster vectorization and offset

The GUI traces the loaded raster (downscaled to 1400 px on the longest side)
with two modes:

- **Outline** thresholds luminance, traces the iso-contour of the content mask
  and keeps only even-depth contours, so holes are dropped and each disjoint
  shape becomes one closed path.
- **Edges** computes a Sobel magnitude, thresholds it, thins the mask with
  Zhang–Suen and traces the 1-px skeleton as open or closed polylines.

Every contour is simplified with Douglas–Peucker (`Simplificar`, mm) and
filtered by length (`Comprimento mín.`). With `Separar linhas` each contour is
an independent `Layer`; without it they share a group id but remain editable.
`Vetorizar` removes the previous traced layers and replaces them.

`Offset externo` / `Offset interno` first show a preview: the offset contours are
drawn as ghosts (orange for outward, purple for inward) and the status bar
reports the distance and contour count. `Aplicar offset` commits them,
`Cancelar` discards the preview. The offset rasterizes each selected closed
polygon at 0.25 mm, builds a signed distance field (two-pass chamfer), extracts
the iso-contour at ±distance and simplifies it. This is robust for concave
shapes and self-intersections; open paths and text contours are skipped.

`Apagar vetorização` removes every layer generated by tracing; `Apagar seleção`
(or the Delete key) removes the selected layers. Tracing again always drops the
previous traced layers, even when the new settings produce no contour.

The `Imagem` tool drags the raster on the canvas. All traced layers move with
it, and a pending offset preview follows, so the cut lines stay aligned with
the artwork before applying the offset. Point coordinates stay in page
millimetres, so the result is cut with the same mapping as any other layer.
