//! Decoder for the path objects in a `.studio3` document.
//!
//! # What is verified
//!
//! Vector geometry is stored as a stream of binary node records:
//!
//! ```text
//! 01  <payload len u32 LE>  <x f32 LE>  <y f32 LE>  <handle data…>
//! ```
//!
//! The record spans `1 + len` bytes; the observed payload lengths are
//!
//! * **17** — anchor only (polyline vertex, `create_line` / `create_rectangle`),
//! * **25** — anchor + one 8-byte handle (smooth node with one handle),
//! * **33** — anchor + two `01`-tagged 8-byte handles (cubic node:
//!   incoming delta, then outgoing delta).
//!
//! A path object is a run of **contiguous** node records (`next offset ==
//! prev offset + 1 + len`); between objects the stream is interrupted by
//! metadata. Anchors are in **millimetres, top-left origin, y down** — except
//! for traced contours in raster documents (`tty.studio3`), whose anchors are
//! on the artwork's pixel grid and are mapped to the page with the repeated
//! raster transform (see [`raster_placement`]).
//!
//! Handles are **deltas relative to the anchor**: the bezier for the segment
//! `i -> i+1` uses node `i`'s outgoing handle and node `i+1`'s incoming
//! handle. Nodes with a single handle get it assigned to the side whose chord
//! it aligns with; the other side falls back to the anchor. Cubic beziers are
//! flattened adaptively (0.05 mm tolerance) into the polyline [`Path`],
//! which is what the cutter and the printer consume.
//!
//! Verified against:
//!
//! * `caixa-1.studio3`: 21 two-node lines + one 149-node contour + two
//!   `create_curve` contours on a 304.8×304.8 mm page,
//! * `multi-contornos.studio3`: 12 closed contour objects in page millimetres,
//! * `tty.studio3`: 20 contour objects in the 3534×5000-pixel artwork frame.

/// A vertex in millimetres, in page coordinates (origin at the top-left, y down).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x_mm: f32,
    pub y_mm: f32,
}

impl Point {
    pub fn new(x_mm: f32, y_mm: f32) -> Self {
        Self { x_mm, y_mm }
    }

    pub fn plus(self, other: Point) -> Point {
        Point::new(self.x_mm + other.x_mm, self.y_mm + other.y_mm)
    }

    pub fn minus(self, other: Point) -> Point {
        Point::new(self.x_mm - other.x_mm, self.y_mm - other.y_mm)
    }

    fn len(self) -> f32 {
        (self.x_mm * self.x_mm + self.y_mm * self.y_mm).sqrt()
    }
}

/// The kind of shape a path belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    Line,
    Rectangle,
    /// `create_curve` / traced contour (bezier nodes).
    Curve,
    Other,
}

/// A row-major affine transform mapping the object's local coordinates to
/// page millimetres:
///
/// ```text
/// | a b tx |   | x |
/// | c d ty | · | y |
/// | 0 0 1  |   | 1 |
/// ```
pub type Affine = [[f32; 3]; 3];

/// One decoded path, flattened to a polyline in page millimetres.
#[derive(Debug, Clone, PartialEq)]
pub struct Path {
    pub kind: PathKind,
    /// Vertices in page millimetres.
    pub points: Vec<Point>,
    /// True when the shape is a rectangle or the contour closes on itself.
    pub closed: bool,
    /// Local-to-page matrix stored with the object, when one was found.
    ///
    /// `None` means no plausible matrix could be validated; `points` are then
    /// the raw stored coordinates.
    pub transform: Option<Affine>,
}

impl Path {
    /// Bounding box as `(min_x, min_y, max_x, max_y)` in millimetres.
    pub fn bounds(&self) -> Option<(f32, f32, f32, f32)> {
        let first = self.points.first()?;
        let mut b = (first.x_mm, first.y_mm, first.x_mm, first.y_mm);
        for p in &self.points[1..] {
            b.0 = b.0.min(p.x_mm);
            b.1 = b.1.min(p.y_mm);
            b.2 = b.2.max(p.x_mm);
            b.3 = b.3.max(p.y_mm);
        }
        Some(b)
    }

    pub fn width_mm(&self) -> f32 {
        self.bounds().map(|b| b.2 - b.0).unwrap_or(0.0)
    }

    pub fn height_mm(&self) -> f32 {
        self.bounds().map(|b| b.3 - b.1).unwrap_or(0.0)
    }

    /// Render this path as an SVG `d` attribute (millimetre coordinates).
    pub fn to_svg_d(&self) -> String {
        let mut d = String::new();
        for (i, p) in self.points.iter().enumerate() {
            if i == 0 {
                d.push_str(&format!("M {:.4} {:.4}", p.x_mm, p.y_mm));
            } else {
                d.push_str(&format!(" L {:.4} {:.4}", p.x_mm, p.y_mm));
            }
        }
        if self.closed {
            d.push_str(" Z");
        }
        d
    }
}

/// The observed node payload lengths.
const NODE_LENGTHS: [u32; 3] = [17, 25, 33];
/// How far after the last node to look for the `create_*` object type.
const TYPE_LOOKAHEAD: usize = 512;
/// How far after the last node to look for the object's affine matrix.
const TRANSFORM_LOOKAHEAD: usize = 256;
/// A matrix is accepted only when the transformed bounding-box centre matches
/// the centre stored immediately after it (the 2 `f32` at `matrix + 36`).
const CENTRE_TOLERANCE_MM: f32 = 0.05;
/// Bezier-flattening tolerance in millimetres.
const FLATTEN_TOL_MM: f32 = 0.05;
/// Adaptive subdivision depth cap.
const FLATTEN_MAX_DEPTH: u8 = 10;

/// A decoded geometry node record.
#[derive(Debug, Clone, Copy)]
struct Node {
    /// Declared payload length (17, 25 or 33).
    length: u32,
    /// Byte offset of the `01` tag byte.
    offset: usize,
    anchor: Point,
    handle_in: Option<Point>,
    handle_out: Option<Point>,
    /// Node carries a single handle (its side is resolved from geometry).
    single_handle: bool,
}

/// Read one node record at `off`.
fn read_node(data: &[u8], off: usize) -> Option<Node> {
    let length = u32::from_le_bytes(data.get(off + 1..off + 5)?.try_into().ok()?);
    if !NODE_LENGTHS.contains(&length) {
        return None;
    }
    let end = off + 1 + length as usize;
    if end > data.len() {
        return None;
    }
    let coordinate = |o: usize| -> Option<f32> {
        let v = f32::from_le_bytes(data.get(o..o + 4)?.try_into().ok()?);
        (v.is_finite() && v.abs() <= 100_000.0).then_some(v)
    };
    let x = coordinate(off + 5)?;
    let y = coordinate(off + 9)?;

    let mut node = Node {
        length,
        offset: off,
        anchor: Point::new(x, y),
        handle_in: None,
        handle_out: None,
        single_handle: false,
    };

    if length == 33 {
        // Layout A: `01 <in 8> 01 <out 8>` at +13 and +22.
        // Layout B: one flag byte in front, tags at +14 and +23.
        let (base_in, base_out) =
            if data.get(off + 13) == Some(&0x01) && data.get(off + 22) == Some(&0x01) {
                (off + 14, off + 23)
            } else if data.get(off + 14) == Some(&0x01) && data.get(off + 23) == Some(&0x01) {
                (off + 15, off + 24)
            } else {
                (off, off) // no handles recognised: treated as a corner node
            };
        let delta = |o: usize| -> Option<Point> {
            let dx = coordinate(o)?;
            let dy = coordinate(o + 4)?;
            (dx.abs() <= 200.0 && dy.abs() <= 200.0).then_some(Point::new(dx, dy))
        };
        if base_in != off {
            node.handle_in = delta(base_in);
            node.handle_out = delta(base_out);
        }
    } else if length == 25 {
        node.single_handle = true;
        // One 8-byte handle after the anchor, either directly at +14 or
        // behind a `01` marker at +15 (observed both ways on real contours).
        if data.get(off + 13) == Some(&0x01) {
            if let Some(h) = read_handle(data, off + 14) {
                node.handle_in = Some(h);
                node.handle_out = Some(h);
            }
        }
        if node.handle_in.is_none() && data.get(off + 14) == Some(&0x01) {
            if let Some(h) = read_handle(data, off + 15) {
                node.handle_in = Some(h);
                node.handle_out = Some(h);
            }
        }
    }

    Some(node)
}

fn read_handle(data: &[u8], slot: usize) -> Option<Point> {
    let dx = f32::from_le_bytes(data.get(slot..slot + 4)?.try_into().ok()?);
    let dy = f32::from_le_bytes(data.get(slot + 4..slot + 8)?.try_into().ok()?);
    (dx.is_finite() && dy.is_finite() && dx.abs() <= 200.0 && dy.abs() <= 200.0)
        .then_some(Point::new(dx, dy))
}

/// Chain contiguous node records into runs; each run is one path object.
fn node_runs(data: &[u8]) -> Vec<Vec<Node>> {
    let mut runs: Vec<Vec<Node>> = Vec::new();
    let mut chain: Vec<Node> = Vec::new();
    let mut i = 0usize;
    while i < data.len() {
        if let Some(node) = read_node(data, i) {
            let contiguous = chain
                .last()
                .map(|last| last.offset + 1 + last.length as usize == node.offset)
                .unwrap_or(true);
            if !contiguous {
                runs.push(std::mem::take(&mut chain));
            }
            chain.push(node);
            i += 1 + node.length as usize;
        } else {
            if !chain.is_empty() {
                runs.push(std::mem::take(&mut chain));
            }
            i += 1;
        }
    }
    if !chain.is_empty() {
        runs.push(chain);
    }
    runs
}

/// Decode every path in a `.studio3` file.
pub fn decode(data: &[u8]) -> Vec<Path> {
    decode_with_hints(data, DecodeHints::default())
}

/// Context that distinguishes contour points in page millimetres from contour
/// points on the artwork's pixel grid.
///
/// Traced contours in raster documents (`tty.studio3`) live in the same pixel
/// frame as the artwork; the identical transform is repeated in the file and
/// recovered by [`raster_placement`]. Give the decoder the page size and the
/// artwork dimensions so it can apply that transform to oversized contours.
#[derive(Debug, Clone, Copy, Default)]
pub struct DecodeHints {
    /// Page size in millimetres, when known.
    pub page_mm: Option<(f32, f32)>,
    /// Dimensions of the artwork raster, when present.
    pub image_px: Option<(u32, u32)>,
}

impl DecodeHints {
    pub fn from_page_and_image(page_mm: Option<(f32, f32)>, image_px: Option<(u32, u32)>) -> Self {
        Self { page_mm, image_px }
    }
}

/// Decode every path, using context to resolve pixel-space contours.
pub fn decode_with_hints(data: &[u8], hints: DecodeHints) -> Vec<Path> {
    let raster_fallback = match (hints.page_mm, hints.image_px) {
        (Some((pw, ph)), Some((iw, ih))) => raster_placement(data, iw, ih, pw, ph).map(|p| {
            [
                [p.scale, 0.0, p.x_mm],
                [0.0, p.scale, p.y_mm],
                [0.0, 0.0, 1.0],
            ]
        }),
        _ => None,
    };

    let mut paths = Vec::new();
    for run in node_runs(data) {
        if let Some(path) = build_run_path(data, &run, raster_fallback) {
            paths.push(path);
        }
    }
    paths
}

fn build_run_path(data: &[u8], run: &[Node], raster_fallback: Option<Affine>) -> Option<Path> {
    if run.len() < 2 {
        // A single node is a frame sentinel, never a path.
        return None;
    }
    let (min_x, min_y, _max_x, _max_y) = run_bbox(run);
    // Clip rectangles / frame sentinels live at negative coordinates; page
    // geometry never does.
    if min_x < -0.5 || min_y < -0.5 {
        return None;
    }

    let last_offset = run[run.len() - 1].offset;
    let last_len = run[run.len() - 1].length as usize;
    let run_end = last_offset + 1 + last_len;
    let kind_at = object_kind(data, run_end, TYPE_LOOKAHEAD);

    let is_curve = run.iter().any(|n| n.length != 17)
        || matches!(kind_at.map(|(k, _)| k), Some(PathKind::Curve));

    let kind = if is_curve {
        PathKind::Curve
    } else {
        match kind_at.map(|(k, _)| k) {
            Some(PathKind::Line)
            | Some(PathKind::Rectangle)
            | Some(PathKind::Curve)
            | Some(PathKind::Other) => kind_at.unwrap().0,
            None => PathKind::Line,
        }
    };

    let transform = find_transform(
        data,
        run_end,
        limit_of(kind_at, data.len()),
        &raw_anchors(run),
    );

    if is_curve {
        // Contours. The per-object matrix is normally absent for traced
        // contours; oversized contours are pixel-space and use the raster
        // transform instead.
        let matrix = transform.or(raster_fallback_for(run, raster_fallback));
        let points = contour_points(run, matrix)?;
        Some(Path {
            kind,
            closed: contour_is_closed(run, matrix),
            points,
            transform: matrix,
        })
    } else {
        let local: Vec<Point> = run.iter().map(|n| n.anchor).collect();
        let points = match transform {
            Some(m) => local.iter().map(|p| apply(&m, *p)).collect(),
            None => local,
        };
        let first = points[0];
        let last = points[points.len() - 1];
        let closed = matches!(kind, PathKind::Rectangle)
            || ((first.x_mm - last.x_mm).abs() < 0.01 && (first.y_mm - last.y_mm).abs() < 0.01);
        Some(Path {
            kind,
            points,
            closed,
            transform,
        })
    }
}

/// The pixel-transform fallback only applies to contours that overshoot the
/// page (traced contours in image pixels). Page-space contours keep their
/// stored frame.
fn raster_fallback_for(run: &[Node], fallback: Option<Affine>) -> Option<Affine> {
    let (max_x, max_y) = {
        let (_, _, max_x, max_y) = run_bbox(run);
        (max_x, max_y)
    };
    // A traced contour is always larger than its page could ever be in
    // millimetres (304.8 mm on the biggest mat): treat anything beyond
    // ~450 mm as a different frame.
    (fallback.is_some() && (max_x > 450.0 || max_y > 450.0)).then(|| fallback.unwrap())
}

fn limit_of(kind_at: Option<(PathKind, usize)>, fallback: usize) -> usize {
    kind_at.map(|(_, at)| at).unwrap_or(fallback)
}

fn run_bbox(run: &[Node]) -> (f32, f32, f32, f32) {
    let mut b = (
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    );
    for n in run {
        b.0 = b.0.min(n.anchor.x_mm);
        b.1 = b.1.min(n.anchor.y_mm);
        b.2 = b.2.max(n.anchor.x_mm);
        b.3 = b.3.max(n.anchor.y_mm);
    }
    b
}

fn raw_anchors(run: &[Node]) -> Vec<Point> {
    run.iter().map(|n| n.anchor).collect()
}

/// Flatten one contour run to page-millimetre points.
///
/// `None` when the contour is degenerate.
fn contour_points(run: &[Node], transform: Option<Affine>) -> Option<Vec<Point>> {
    let t = transform.unwrap_or(IDENTITY);
    let n = run.len();
    if n < 2 {
        return None;
    }
    let closed = contour_is_closed(run, transform);

    // Transformed anchors and handles.
    let mut anchors = Vec::with_capacity(n);
    let mut ins = Vec::with_capacity(n);
    let mut outs = Vec::with_capacity(n);
    for node in run {
        let anchor = apply(&t, node.anchor);
        let handle = |h: Option<Point>| {
            h.map(|h| {
                let tip = apply(&t, node.anchor.plus(h));
                tip.minus(anchor)
            })
        };
        anchors.push(anchor);
        ins.push(if node.single_handle {
            None
        } else {
            handle(node.handle_in)
        });
        outs.push(if node.single_handle {
            None
        } else {
            handle(node.handle_out)
        });
    }

    // Assign single handles to the side their chord aligns with. Handles lie
    // roughly along the local tangent, so the chord with the larger
    // alignment wins; the other side falls back to the anchor (zero delta).
    let zero_chord = Point::new(0.0, 0.0);
    for i in 0..n {
        if !run[i].single_handle {
            continue;
        }
        let Some(h) = run[i]
            .handle_in
            .map(|h| apply(&t, run[i].anchor.plus(h)).minus(anchors[i]))
        else {
            continue;
        };
        let before = if i > 0 {
            anchors[i].minus(anchors[i - 1])
        } else if closed {
            anchors[i].minus(anchors[n - 1])
        } else {
            zero_chord
        };
        let after = if i + 1 < n {
            anchors[i + 1].minus(anchors[i])
        } else if closed {
            anchors[0].minus(anchors[i])
        } else {
            zero_chord
        };
        let dot_next = after.x_mm * h.x_mm + after.y_mm * h.y_mm;
        let dot_prev = before.x_mm * h.x_mm + before.y_mm * h.y_mm;
        if dot_next.abs() >= dot_prev.abs() {
            outs[i] = Some(h);
        } else {
            ins[i] = Some(h);
        }
    }

    let zero = Point::new(0.0, 0.0);
    let segments = n.saturating_sub(1) + usize::from(closed && n >= 3);
    let mut line: Vec<Point> = Vec::new();
    for s in 0..segments {
        let i = s % n;
        let j = (s + 1) % n;
        let p0 = anchors[i];
        let p1 = anchors[j];
        let c1 = p0.plus(outs[i].unwrap_or(zero));
        let c2 = p1.plus(ins[j].unwrap_or(zero));
        if s == 0 {
            line.push(p0);
        }
        flatten_cubic(p0, c1, c2, p1, 0, &mut line);
        // Skip the anchor the previous segment already emitted.
        if line.last() != Some(&p1) {
            line.push(p1);
        }
    }
    if line.len() < 2 {
        return None;
    }
    // Closed contours end on their first point.
    if closed && line.last() != Some(&anchors[0]) {
        line.push(anchors[0]);
    }
    Some(line)
}

const IDENTITY: Affine = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// Whether the contour closes back onto its first node: the closing chord is
/// no longer than 1.5× the median node spacing. Traced contours are closed;
/// long open strokes would have a giant closing chord.
fn contour_is_closed(run: &[Node], transform: Option<Affine>) -> bool {
    if run.len() < 3 {
        return false;
    }
    let t = transform.unwrap_or(IDENTITY);
    let first = apply(&t, run[0].anchor);
    let last = apply(&t, run[run.len() - 1].anchor);
    let mut lengths: Vec<f32> = run
        .windows(2)
        .map(|w| apply(&t, w[0].anchor).minus(apply(&t, w[1].anchor)).len())
        .collect();
    if lengths.is_empty() {
        return false;
    }
    lengths.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = lengths[lengths.len() / 2];
    first.minus(last).len() <= 1.5 * median.max(0.25)
}

/// Flatten one cubic bezier into `line` (interior points only, in order).
fn flatten_cubic(p0: Point, c1: Point, c2: Point, p1: Point, depth: u8, line: &mut Vec<Point>) {
    let mid = Point::new((p0.x_mm + p1.x_mm) / 2.0, (p0.y_mm + p1.y_mm) / 2.0);
    let flat = depth >= FLATTEN_MAX_DEPTH
        || (c1.minus(mid).len() < FLATTEN_TOL_MM && c2.minus(mid).len() < FLATTEN_TOL_MM);
    if flat {
        return;
    }
    let ab = midpoint(p0, c1);
    let bc = midpoint(c1, c2);
    let cd = midpoint(c2, p1);
    let abc = midpoint(ab, bc);
    let bcd = midpoint(bc, cd);
    let abcd = midpoint(abc, bcd);
    // Left half first, then the on-curve split point, then the right half:
    // this keeps the emitted points in curve order.
    flatten_cubic(p0, ab, abc, abcd, depth + 1, line);
    line.push(abcd);
    flatten_cubic(abcd, bcd, cd, p1, depth + 1, line);
}

fn midpoint(a: Point, b: Point) -> Point {
    Point::new((a.x_mm + b.x_mm) / 2.0, (a.y_mm + b.y_mm) / 2.0)
}

/// Apply an affine matrix to a point: `p' = M · p`.
fn apply(m: &Affine, p: Point) -> Point {
    Point::new(
        m[0][0] * p.x_mm + m[0][1] * p.y_mm + m[0][2],
        m[1][0] * p.x_mm + m[1][1] * p.y_mm + m[1][2],
    )
}

/// Find the object's local-to-page matrix.
///
/// The matrix is a run of nine `f32` values with a bottom row of `(0, 0, 1)`,
/// stored somewhere after the last node. A candidate is only accepted when
/// the centre of the transformed bounding box equals the pair of `f32` values
/// stored at `candidate + 36` (the object's page-space centre). That check
/// rejects accidental matches in the surrounding metadata.
fn find_transform(data: &[u8], from: usize, limit: usize, points: &[Point]) -> Option<Affine> {
    let end = (from + TRANSFORM_LOOKAHEAD).min(limit).min(data.len());
    for start in from..end {
        if start + 36 + 8 > data.len() {
            break;
        }
        let Some(m) = read_affine(data, start) else {
            continue;
        };
        let (cx, cy) = transformed_centre(&m, points);
        let sx = f32::from_le_bytes(data[start + 36..start + 40].try_into().unwrap());
        let sy = f32::from_le_bytes(data[start + 40..start + 44].try_into().unwrap());
        if (cx - sx).abs() <= CENTRE_TOLERANCE_MM && (cy - sy).abs() <= CENTRE_TOLERANCE_MM {
            return Some(m);
        }
    }
    None
}

fn read_affine(data: &[u8], start: usize) -> Option<Affine> {
    let mut m = [[0.0f32; 3]; 3];
    for (r, row) in m.iter_mut().enumerate() {
        for (c, cell) in row.iter_mut().enumerate() {
            let o = start + 4 * (3 * r + c);
            *cell = f32::from_le_bytes(data[o..o + 4].try_into().ok()?);
            if !cell.is_finite() {
                return None;
            }
        }
    }

    // Affine 2-D matrix: the last row must be (0, 0, 1).
    if m[2][0].abs() > 1e-4 || m[2][1].abs() > 1e-4 || (m[2][2] - 1.0).abs() > 1e-4 {
        return None;
    }

    let det = m[0][0] * m[1][1] - m[0][1] * m[1][0];
    if det.abs() < 1e-9 {
        return None;
    }

    // Sane physical bounds: no pathological scale or translation.
    if m[0][2].abs() > 2000.0
        || m[1][2].abs() > 2000.0
        || m[0][0].abs() > 1000.0
        || m[1][1].abs() > 1000.0
    {
        return None;
    }

    Some(m)
}

fn transformed_centre(m: &Affine, points: &[Point]) -> (f32, f32) {
    let first = apply(m, points[0]);
    let mut min_x = first.x_mm;
    let mut min_y = first.y_mm;
    let mut max_x = first.x_mm;
    let mut max_y = first.y_mm;
    for p in &points[1..] {
        let q = apply(m, *p);
        min_x = min_x.min(q.x_mm);
        min_y = min_y.min(q.y_mm);
        max_x = max_x.max(q.x_mm);
        max_y = max_y.max(q.y_mm);
    }
    ((min_x + max_x) / 2.0, (min_y + max_y) / 2.0)
}

fn object_kind(data: &[u8], from: usize, window: usize) -> Option<(PathKind, usize)> {
    let end = (from + window).min(data.len());
    let haystack = &data[from..end];

    let find = |needle: &[u8]| {
        haystack
            .windows(needle.len())
            .position(|w| w == needle)
            .map(|p| (p, needle.len()))
    };

    // Pick the earliest `create_*` token in the window.
    let candidates = [
        (b"create_rectangle".as_slice(), PathKind::Rectangle),
        (b"create_line".as_slice(), PathKind::Line),
        (b"create_curve".as_slice(), PathKind::Curve),
        (b"create_".as_slice(), PathKind::Other),
    ];

    let mut best: Option<(usize, PathKind)> = None;
    for (needle, kind) in candidates {
        if let Some((pos, _)) = find(needle) {
            if best.map_or(true, |(best_pos, _)| pos < best_pos) {
                best = Some((pos, kind));
            }
        }
    }
    best.map(|(pos, kind)| (kind, from + pos))
}

/// A raster image's pixel-to-page transform.
///
/// `.studio3` stores the artwork as a PNG/JPEG plus one or more objects whose
/// local frame is the image's pixel grid. For the designs inspected so far the
/// image object uses a uniform, axis-aligned scale with no rotation:
///
/// ```text
/// x_mm = scale * x_px + x_mm
/// y_mm = scale * y_px + y_mm
/// ```
///
/// This is deliberately a *heuristic*: the matrix is only accepted when the
/// same `(scale, x, y)` triple occurs more than once and when the transformed
/// image rectangle is physically plausible on the page. When nothing is found
/// the caller must fall back to an explicit/fit placement (see the CLI).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RasterPlacement {
    /// Millimetres per image pixel.
    pub scale: f32,
    /// Page position of the image's top-left pixel.
    pub x_mm: f32,
    pub y_mm: f32,
}

impl RasterPlacement {
    pub fn width_mm(&self, image_width_px: u32) -> f32 {
        self.scale * image_width_px as f32
    }

    pub fn height_mm(&self, image_height_px: u32) -> f32 {
        self.scale * image_height_px as f32
    }
}

/// Find the image transform in `data`.
///
/// Evidence: `tentativas anteriores/tty.studio3` contains seven identical
/// candidates `scale = 0.05707326`, `x = 4.8473 mm`, `y = 4.5425 mm`; they map
/// the 3534 × 5000 px artwork to 201.7 × 285.4 mm. `topo-4x`
/// stores a *single* candidate (`scale = 0.05528704`, offset (0.8581, 1.0488));
/// a lone candidate is accepted only when it is the only group found —
/// ambiguity wins nothing. `multi-contornos` contains no such matrix, so
/// this returns `None` there and the CLI uses a fit placement instead.
pub fn raster_placement(
    data: &[u8],
    image_width_px: u32,
    image_height_px: u32,
    page_width_mm: f32,
    page_height_mm: f32,
) -> Option<RasterPlacement> {
    use std::collections::HashMap;

    const MIN_SCALE: f32 = 0.002;
    const MAX_SCALE: f32 = 2.0;
    const ORTHO_TOL: f32 = 1e-4;
    const UNIFORM_TOL: f32 = 1e-3;

    // (quantised scale, x, y) -> (count, sum scale, sum x, sum y)
    type GroupKey = (i64, i64, i64);
    type GroupStats = (u32, f64, f64, f64);
    let mut groups: HashMap<GroupKey, GroupStats> = HashMap::new();

    let mut i = 0usize;
    while i + 44 <= data.len() {
        let start = i;
        if let Some(m) = read_affine(data, start) {
            let a = m[0][0];
            let d = m[1][1];
            let tx = m[0][2];
            let ty = m[1][2];

            let ortho = m[0][1].abs() <= ORTHO_TOL && m[1][0].abs() <= ORTHO_TOL;
            let uniform = (a - d).abs() <= UNIFORM_TOL * a.abs().max(d.abs()).max(1.0);
            let scale_ok = (MIN_SCALE..=MAX_SCALE).contains(&a) && a > 0.0 && d > 0.0;
            let page_ok = tx > -page_width_mm
                && ty > -page_height_mm
                && tx < page_width_mm
                && ty < page_height_mm;

            if ortho && uniform && scale_ok && page_ok {
                // The object's page-space centre follows the matrix. Requiring
                // it to be on the page rejects random float runs in image data.
                let (cx, cy) = match (
                    data[start + 36..start + 40].try_into(),
                    data[start + 40..start + 44].try_into(),
                ) {
                    (Ok(x), Ok(y)) => (f32::from_le_bytes(x), f32::from_le_bytes(y)),
                    _ => (f32::NAN, f32::NAN),
                };
                let centre_ok = cx.is_finite()
                    && cy.is_finite()
                    && (-1.0..=page_width_mm + 1.0).contains(&cx)
                    && (-1.0..=page_height_mm + 1.0).contains(&cy);

                let w_mm = a * image_width_px as f32;
                let h_mm = d * image_height_px as f32;
                let size_ok = w_mm >= 0.2 * page_width_mm
                    && w_mm <= 1.5 * page_width_mm
                    && h_mm >= 0.2 * page_height_mm
                    && h_mm <= 1.5 * page_height_mm;
                let intersects =
                    tx + w_mm > 0.0 && ty + h_mm > 0.0 && tx < page_width_mm && ty < page_height_mm;

                if centre_ok && size_ok && intersects {
                    let key = (
                        (a * 10_000.0).round() as i64,
                        (tx * 100.0).round() as i64,
                        (ty * 100.0).round() as i64,
                    );
                    let e = groups.entry(key).or_insert((0, 0.0, 0.0, 0.0));
                    e.0 += 1;
                    e.1 += f64::from(a);
                    e.2 += f64::from(tx);
                    e.3 += f64::from(ty);
                }
            }
        }
        i += 1;
    }

    // Prefer a repeated transform (strong evidence); a single candidate is
    // only trusted when it is the *only* one in the file.
    let repeated = groups
        .values()
        .filter(|(count, ..)| *count >= 2)
        .max_by_key(|(count, ..)| *count);
    let best = match repeated {
        Some(best) => best,
        None => {
            if groups.len() == 1 {
                groups.values().next().unwrap()
            } else {
                return None;
            }
        }
    };

    let n = f64::from(best.0);
    Some(RasterPlacement {
        scale: (best.1 / n) as f32,
        x_mm: (best.2 / n) as f32,
        y_mm: (best.3 / n) as f32,
    })
}

/// Render decoded paths as a standalone SVG document (millimetres).
pub fn to_svg(paths: &[Path], page_width_mm: f32, page_height_mm: f32) -> String {
    let mut svg = String::new();
    svg.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}mm\" height=\"{h}mm\" \
         viewBox=\"0 0 {w} {h}\">\n",
        w = page_width_mm,
        h = page_height_mm
    ));
    svg.push_str("  <g fill=\"none\" stroke=\"#000\" stroke-width=\"0.2\">\n");
    for path in paths {
        let color = match path.kind {
            PathKind::Rectangle => "#d00",
            PathKind::Line => "#000",
            PathKind::Curve => "#06c",
            PathKind::Other => "#888",
        };
        svg.push_str(&format!(
            "    <path d=\"{}\" stroke=\"{}\"/>\n",
            path.to_svg_d(),
            color
        ));
    }
    svg.push_str("  </g>\n</svg>\n");
    svg
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build the bytes of one polyline vertex exactly as stored.
    fn vertex(x: f32, y: f32, tail: usize) -> Vec<u8> {
        let mut v = [0x01, 0x11, 0, 0, 0].to_vec();
        v.extend_from_slice(&x.to_le_bytes());
        v.extend_from_slice(&y.to_le_bytes());
        v.extend(std::iter::repeat(0u8).take(tail));
        v
    }

    /// One cubic node record (payload length 33): anchor + `01` + in delta +
    /// `01` + out delta + 3 bytes of trailing flags.
    fn cubic(x: f32, y: f32, in_h: (f32, f32), out_h: (f32, f32)) -> Vec<u8> {
        let mut v = [0x01, 0x21, 0, 0, 0].to_vec();
        v.extend_from_slice(&x.to_le_bytes());
        v.extend_from_slice(&y.to_le_bytes());
        v.push(0x01);
        v.extend_from_slice(&in_h.0.to_le_bytes());
        v.extend_from_slice(&in_h.1.to_le_bytes());
        v.push(0x01);
        v.extend_from_slice(&out_h.0.to_le_bytes());
        v.extend_from_slice(&out_h.1.to_le_bytes());
        v.extend_from_slice(&[0, 0, 0]);
        assert_eq!(v.len(), 34);
        v
    }

    #[test]
    fn decodes_a_rectangle() {
        let mut data = Vec::new();
        data.extend_from_slice(b"\x01\x51\x00\x00\x00"); // container header
        data.extend_from_slice(&vertex(69.1621, 89.2801, 5));
        data.extend_from_slice(&vertex(69.1621, 188.9269, 5));
        data.extend_from_slice(&vertex(141.712, 188.9269, 5));
        data.extend_from_slice(&vertex(141.712, 89.2801, 5));
        data.extend_from_slice(b"\0\0\0\0create_rectangle");

        let paths = decode(&data);
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].kind, PathKind::Rectangle);
        assert!(paths[0].closed);
        assert_eq!(paths[0].points.len(), 4);
        assert!((paths[0].width_mm() - 72.5499).abs() < 0.01);
        assert!((paths[0].height_mm() - 99.6468).abs() < 0.01);
    }

    #[test]
    fn decodes_a_line_as_open() {
        let mut data = Vec::new();
        data.extend_from_slice(&vertex(64.185, 48.338, 5));
        data.extend_from_slice(&vertex(147.494, 48.734, 5));
        data.extend_from_slice(b"\0\0\0create_line");

        let paths = decode(&data);
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].kind, PathKind::Line);
        assert!(!paths[0].closed);
        assert_eq!(paths[0].points.len(), 2);
    }

    #[test]
    fn large_gap_splits_paths() {
        let mut data = Vec::new();
        data.extend_from_slice(&vertex(10.0, 10.0, 5));
        data.extend_from_slice(&vertex(20.0, 10.0, 5));
        data.extend_from_slice(&[0u8; 200]);
        data.extend_from_slice(&vertex(30.0, 30.0, 5));
        data.extend_from_slice(&vertex(40.0, 30.0, 5));
        data.extend_from_slice(b"create_line");

        let paths = decode(&data);
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn single_vertices_are_ignored() {
        let mut data = Vec::new();
        data.extend_from_slice(&vertex(10.0, 10.0, 5));
        data.extend_from_slice(&[0u8; 200]);
        data.extend_from_slice(b"create_line");
        assert!(decode(&data).is_empty());
    }

    #[test]
    fn non_finite_coordinates_are_rejected() {
        let mut data = Vec::new();
        data.extend_from_slice(&vertex(f32::NAN, 10.0, 5));
        data.extend_from_slice(&vertex(10.0, f32::INFINITY, 5));
        data.extend_from_slice(b"create_line");
        assert!(decode(&data).is_empty());
    }

    #[test]
    fn svg_contains_closing_command() {
        let path = Path {
            kind: PathKind::Rectangle,
            points: vec![
                Point::new(1.0, 2.0),
                Point::new(3.0, 2.0),
                Point::new(3.0, 4.0),
                Point::new(1.0, 4.0),
            ],
            closed: true,
            transform: None,
        };
        let d = path.to_svg_d();
        assert!(d.starts_with("M 1.0000 2.0000"));
        assert!(d.ends_with('Z'));
    }

    /// Append nine `f32` values (a row-major affine matrix).
    fn affine(a: f32, b: f32, tx: f32, c: f32, d: f32, ty: f32) -> Vec<u8> {
        [a, b, tx, c, d, ty, 0.0, 0.0, 1.0]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect()
    }

    #[test]
    fn applies_the_object_transform() {
        let mut data = Vec::new();
        data.extend_from_slice(&vertex(69.1621, 89.2801, 5));
        data.extend_from_slice(&vertex(69.1621, 188.9269, 5));
        data.extend_from_slice(&vertex(141.712, 188.9269, 5));
        data.extend_from_slice(&vertex(141.712, 89.2801, 5));
        // Translation observed in the centred-rectangle fixture.
        data.extend_from_slice(&affine(1.0, 0.0, -0.437057, 0.0, 1.0, 9.39653));
        data.extend_from_slice(&105.0f32.to_le_bytes());
        data.extend_from_slice(&148.5f32.to_le_bytes());
        data.extend_from_slice(b"create_rectangle");

        let paths = decode(&data);
        assert_eq!(paths.len(), 1);
        let path = &paths[0];
        assert!(
            path.transform.is_some(),
            "matrix must be validated and applied"
        );

        let (min_x, min_y, max_x, max_y) = path.bounds().unwrap();
        assert!((min_x - 68.7251).abs() < 0.01, "min_x {min_x}");
        assert!((min_y - 98.6766).abs() < 0.01, "min_y {min_y}");
        assert!(((min_x + max_x) / 2.0 - 105.0).abs() < 0.01);
        assert!(((min_y + max_y) / 2.0 - 148.5).abs() < 0.01);
        // The shape keeps its size; only its position changes.
        assert!((path.width_mm() - 72.5499).abs() < 0.01);
        assert!((path.height_mm() - 99.6468).abs() < 0.01);
    }

    /// A closed two-node contour: start (10,10) heading up with the outgoing
    /// handle, end (50,10) arriving from the left.
    fn two_cubic_nodes_closed_blob() -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&cubic(10.0, 10.0, (0.0, 5.0), (5.0, 3.0)));
        data.extend_from_slice(&cubic(50.0, 10.0, (-5.0, 3.0), (0.0, 5.0)));
        data.extend_from_slice(b"create_curve");
        data
    }

    #[test]
    fn decodes_a_cubic_contour_and_closes_it() {
        let paths = decode(&two_cubic_nodes_closed_blob());
        // Two nodes alone form a segment; the closing chord is 40 mm while the
        // median spacing is 40 mm: treated as open, not as a closed blob.
        assert_eq!(paths.len(), 1);
        let path = &paths[0];
        assert_eq!(path.kind, PathKind::Curve);
        assert!(!path.closed);
        // Flattening subdivides; a straight-ish cubic with 5 mm handles stays
        // short but must contain at least the two anchors.
        assert!(path.points.len() >= 2);
        let (min_x, _, max_x, _) = path.bounds().unwrap();
        assert!((min_x - 10.0).abs() < 1.5, "{min_x}");
        assert!((max_x - 50.0).abs() < 1.5, "{max_x}");
    }

    #[test]
    fn decodes_a_cubic_blob_as_closed() {
        let mut data = Vec::new();
        data.extend_from_slice(&cubic(10.0, 10.0, (0.0, 5.0), (5.0, 3.0)));
        data.extend_from_slice(&cubic(50.0, 10.0, (-5.0, 3.0), (0.0, 5.0)));
        data.extend_from_slice(&cubic(50.0, 50.0, (0.0, -3.0), (-5.0, -4.0)));
        data.extend_from_slice(b"create_curve");

        let paths = decode(&data);
        assert_eq!(paths.len(), 1);
        let path = &paths[0];
        assert_eq!(path.kind, PathKind::Curve);
        assert!(path.closed, "the closing chord is in scale with the nodes");
        assert!(
            path.points.len() > 3,
            "beziers must be flattened, got {path:?}"
        );
        // The closing segment brings the polyline back to its start.
        let first = path.points[0];
        let last = path.points[path.points.len() - 1];
        assert!((first.x_mm - last.x_mm).abs() < 0.001);
        assert!((first.y_mm - last.y_mm).abs() < 0.001);
    }

    #[test]
    fn flattened_cubic_stays_within_a_tolerance_of_the_true_curve() {
        // Three anchors (0,0), (60,0), (30,40) with ~20 mm handles; the arcs
        // bow towards the anchor extremes but never past them by much.
        let mut data = Vec::new();
        data.extend_from_slice(&cubic(0.0, 0.0, (0.0, 12.0), (30.0, 20.0)));
        data.extend_from_slice(&cubic(60.0, 0.0, (-30.0, 20.0), (0.0, 12.0)));
        data.extend_from_slice(&cubic(30.0, 40.0, (30.0, -20.0), (-30.0, -20.0)));

        let paths = decode(&data);
        assert_eq!(paths.len(), 1);
        let path = &paths[0];
        let xs: Vec<f32> = path.points.iter().map(|p| p.x_mm).collect();
        let ys: Vec<f32> = path.points.iter().map(|p| p.y_mm).collect();
        // Every sample stays inside the anchor bounding box (the handles pull
        // the curves inwards here), plus a small margin for sampling.
        assert!(
            xs.iter().cloned().fold(f32::INFINITY, f32::min) > -2.0,
            "xs {xs:?}"
        );
        assert!(
            xs.iter().cloned().fold(f32::NEG_INFINITY, f32::max) < 62.0,
            "xs {xs:?}"
        );
        assert!(
            ys.iter().cloned().fold(f32::INFINITY, f32::min) > -2.0,
            "ys {ys:?}"
        );
        assert!(
            ys.iter().cloned().fold(f32::NEG_INFINITY, f32::max) < 45.0,
            "ys {ys:?}"
        );
    }

    #[test]
    fn finds_a_repeated_raster_transform() {
        let mut data = Vec::new();
        for _ in 0..3 {
            data.extend_from_slice(&affine(0.21, 0.0, 0.0, 0.0, 0.21, 0.0));
            data.extend_from_slice(&105.0f32.to_le_bytes());
            data.extend_from_slice(&148.5f32.to_le_bytes());
            data.extend_from_slice(&[0u8; 7]);
        }

        let p = raster_placement(&data, 1000, 1414, 210.0, 297.0).unwrap();
        assert!((p.scale - 0.21).abs() < 1e-6);
        assert!((p.x_mm - 0.0).abs() < 1e-6);
        assert!((p.y_mm - 0.0).abs() < 1e-6);
        assert!((p.width_mm(1000) - 210.0).abs() < 0.01);
    }

    #[test]
    fn a_single_raster_matrix_is_trusted_only_when_alone() {
        let mut data = Vec::new();
        data.extend_from_slice(&affine(0.21, 0.0, 0.0, 0.0, 0.21, 0.0));
        data.extend_from_slice(&105.0f32.to_le_bytes());
        data.extend_from_slice(&148.5f32.to_le_bytes());
        // The only candidate in the file: accepted (already validated by the
        // on-page/size checks around it).
        assert!(raster_placement(&data, 1000, 1414, 210.0, 297.0).is_some());

        // Two *different* candidates: ambiguity — trust neither.
        data.extend_from_slice(&affine(0.3, 0.0, 5.0, 0.0, 0.3, 5.0));
        data.extend_from_slice(&100.0f32.to_le_bytes());
        data.extend_from_slice(&140.0f32.to_le_bytes());
        assert!(raster_placement(&data, 1000, 1414, 210.0, 297.0).is_none());
    }

    #[test]
    fn rejects_a_raster_matrix_that_does_not_fit_the_page() {
        let mut data = Vec::new();
        for _ in 0..3 {
            data.extend_from_slice(&affine(10.0, 0.0, 0.0, 0.0, 10.0, 0.0));
            data.extend_from_slice(&105.0f32.to_le_bytes());
            data.extend_from_slice(&148.5f32.to_le_bytes());
            data.extend_from_slice(&[0u8; 7]);
        }
        assert!(raster_placement(&data, 1000, 1414, 210.0, 297.0).is_none());
    }

    #[test]
    fn rejects_a_matrix_whose_centre_does_not_match() {
        let mut data = Vec::new();
        data.extend_from_slice(&vertex(10.0, 10.0, 5));
        data.extend_from_slice(&vertex(20.0, 10.0, 5));
        data.extend_from_slice(&affine(1.0, 0.0, 0.0, 0.0, 1.0, 0.0));
        // Centre that does not match the vertices: the matrix must be ignored.
        data.extend_from_slice(&999.0f32.to_le_bytes());
        data.extend_from_slice(&999.0f32.to_le_bytes());
        data.extend_from_slice(b"create_line");

        let paths = decode(&data);
        assert_eq!(paths.len(), 1);
        assert!(paths[0].transform.is_none());
        assert!((paths[0].points[0].x_mm - 10.0).abs() < 1e-6);
    }

    /// A traced contour in artwork pixels plus the repeated raster transform
    /// that the real `tty.studio3` file stores for image-space objects.
    #[test]
    fn pixel_contours_use_the_raster_transform() {
        let mut data = Vec::new();
        for _ in 0..3 {
            data.extend_from_slice(&affine(0.05707326, 0.0, 4.8473, 0.0, 0.05707326, 4.5425));
            data.extend_from_slice(&105.0f32.to_le_bytes());
            data.extend_from_slice(&148.5f32.to_le_bytes());
            data.extend_from_slice(&[0u8; 7]);
        }
        // Contour around (100..900, 200..800) px, well past the A4 page.
        data.extend_from_slice(&cubic(100.0, 200.0, (100.0, 0.0), (100.0, 100.0)));
        data.extend_from_slice(&cubic(900.0, 800.0, (-100.0, 100.0), (-100.0, -100.0)));
        data.extend_from_slice(&cubic(400.0, 500.0, (100.0, 100.0), (-100.0, -50.0)));

        let hints = DecodeHints::from_page_and_image(Some((210.0, 297.0)), Some((1000, 1200)));
        let paths = decode_with_hints(&data, hints);
        assert_eq!(paths.len(), 1);
        let path = &paths[0];
        assert_eq!(path.kind, PathKind::Curve);
        let (min_x, _, _, _) = path.bounds().unwrap();
        // 100 px * 0.057 + 4.8473 = 10.55 mm: on-page now, not in pixels.
        assert!((min_x - 10.55).abs() < 0.1, "min_x {min_x}");
    }
}
