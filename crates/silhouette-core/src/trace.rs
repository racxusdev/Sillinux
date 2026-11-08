use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceMode {
    Outline,
    Edges,
}

#[derive(Debug, Clone, Copy)]
pub struct TraceOptions {
    pub mode: TraceMode,
    pub threshold: u8,
    pub edge_threshold: u8,
    pub simplify_mm: f32,
    pub min_length_mm: f32,
    pub mm_per_px: f32,
    /// Drop closed contours smaller than this area (despeckle). 0 disables.
    pub min_area_mm2: f32,
    /// Trace bright content instead of dark (set automatically from the
    /// background, so white-on-black templates trace their shapes).
    pub invert: bool,
    /// When set, the mask is "anything that differs from this colour by more
    /// than `color_tolerance`" — how coloured artwork on a light (or dark)
    /// background gets traced, pale pieces included. `invert` is ignored.
    pub bg_color: Option<[u8; 3]>,
    /// Colour distance tolerance for `bg_color` mode.
    pub color_tolerance: u8,
    /// Chaikin smoothing passes over contours before simplification —
    /// removes the pixel staircase the way Studio's curve fitting does.
    pub smooth_iterations: u8,
}

impl Default for TraceOptions {
    fn default() -> Self {
        Self {
            mode: TraceMode::Outline,
            threshold: 230,
            edge_threshold: 60,
            simplify_mm: 0.2,
            min_length_mm: 1.0,
            mm_per_px: 1.0,
            min_area_mm2: 0.0,
            invert: false,
            bg_color: None,
            color_tolerance: 25,
            smooth_iterations: 0,
        }
    }
}

/// Euclidean RGB distance between a pixel (alpha composited over white) and a
/// reference colour.
fn color_distance(px: &[u8], bg: [u8; 3]) -> f32 {
    let a = f32::from(px[3]) / 255.0;
    let r = f32::from(px[0]) * a + 255.0 * (1.0 - a);
    let g = f32::from(px[1]) * a + 255.0 * (1.0 - a);
    let b = f32::from(px[2]) * a + 255.0 * (1.0 - a);
    ((r - f32::from(bg[0])).powi(2)
        + (g - f32::from(bg[1])).powi(2)
        + (b - f32::from(bg[2])).powi(2))
    .sqrt()
}

#[derive(Debug, Clone, PartialEq)]
pub struct TracedPath {
    pub points: Vec<(f32, f32)>,
    pub closed: bool,
}

pub fn trace_rgba(rgba: &[u8], width: u32, height: u32, options: &TraceOptions) -> Vec<TracedPath> {
    let w = width as usize;
    let h = height as usize;
    if w < 3 || h < 3 || rgba.len() < w * h * 4 {
        return Vec::new();
    }
    let lum = luminance(rgba, w, h);
    let mut out = Vec::new();
    match options.mode {
        TraceMode::Outline => {
            let pw = w + 2;
            let ph = h + 2;
            let mut field = vec![0.0f32; pw * ph];
            for y in 0..h {
                for x in 0..w {
                    let hit = match options.bg_color {
                        Some(bg) => {
                            // Coloured-art mode: content = anything that does
                            // not match the background colour.
                            let o = (y * w + x) * 4;
                            color_distance(&rgba[o..o + 4], bg)
                                > f32::from(options.color_tolerance)
                        }
                        None => if options.invert {
                            lum[y * w + x] >= options.threshold as f32
                        } else {
                            lum[y * w + x] < options.threshold as f32
                        },
                    };
                    if hit {
                        field[(y + 1) * pw + x + 1] = 1.0;
                    }
                }
            }
            let contours = iso_contours(&field, pw, ph, 0.5);
            for contour in contours.iter() {
                // Every contour is kept — outer boundaries AND holes — because
                // both are cut lines (e.g. the slit inside a template piece).
                let shifted: Vec<(f32, f32)> =
                    contour.iter().map(|(x, y)| (x - 1.0, y - 1.0)).collect();
                if let Some(path) = finish(shifted, true, options) {
                    if options.min_area_mm2 > 0.0
                        && polyline_area(&path.points).abs() < options.min_area_mm2
                    {
                        continue;
                    }
                    out.push(path);
                }
            }
        }
        TraceMode::Edges => {
            let mag = sobel(&lum, w, h);
            let mut mask = vec![false; w * h];
            for (i, m) in mag.iter().enumerate() {
                mask[i] = *m >= options.edge_threshold as f32;
            }
            thin(&mut mask, w, h);
            let segments = join_segments(trace_skeleton(&mask, w, h), 3.0);
            for (points, closed) in segments {
                if let Some(path) = finish(points, closed, options) {
                    out.push(path);
                }
            }
        }
    }
    out
}

/// Signed shoelace area of a polyline (negative when wound the other way).
pub fn polyline_area(points: &[(f32, f32)]) -> f32 {
    let n = points.len();
    if n < 3 {
        return 0.0;
    }
    let mut sum = 0.0;
    for i in 0..n {
        let (x0, y0) = points[i];
        let (x1, y1) = points[(i + 1) % n];
        sum += x0 * y1 - x1 * y0;
    }
    sum * 0.5
}

pub fn offset_polygon(
    points: &[(f32, f32)],
    distance_mm: f32,
    resolution_mm: f32,
) -> Vec<Vec<(f32, f32)>> {
    if points.len() < 3 {
        return Vec::new();
    }
    if distance_mm.abs() < 1e-3 {
        return vec![points.to_vec()];
    }
    let res = resolution_mm.max(0.05);
    let (min_x, min_y, max_x, max_y) = bbox(points);
    let margin = distance_mm.abs() + res * 4.0;
    let ox = min_x - margin;
    let oy = min_y - margin;
    let w = ((((max_x - min_x) + 2.0 * margin) / res).ceil() as usize + 1).max(3);
    let h = ((((max_y - min_y) + 2.0 * margin) / res).ceil() as usize + 1).max(3);
    let max_pixels = 16_000_000usize;
    if w * h > max_pixels {
        let factor = ((w * h) as f32 / max_pixels as f32).sqrt();
        return offset_polygon(points, distance_mm, res * factor);
    }
    let mut mask = vec![false; w * h];
    for y in 0..h {
        let sy = oy + (y as f32 + 0.5) * res;
        let mut xs = Vec::new();
        for i in 0..points.len() {
            let (x1, y1) = points[i];
            let (x2, y2) = points[(i + 1) % points.len()];
            if (y1 <= sy && y2 > sy) || (y2 <= sy && y1 > sy) {
                let t = (sy - y1) / (y2 - y1);
                xs.push(x1 + t * (x2 - x1));
            }
        }
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mut k = 0;
        while k + 1 < xs.len() {
            let ia = (((xs[k] - ox) / res - 0.5).ceil() as i64).max(0);
            let ib = (((xs[k + 1] - ox) / res - 0.5).floor() as i64).min(w as i64 - 1);
            if ib >= ia {
                for x in ia..=ib {
                    mask[y * w + x as usize] = true;
                }
            }
            k += 2;
        }
    }
    let mut dist = vec![1e9f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let inside = mask[i];
            let left = if x > 0 { mask[i - 1] } else { false };
            let right = if x + 1 < w { mask[i + 1] } else { false };
            let up = if y > 0 { mask[i - w] } else { false };
            let down = if y + 1 < h { mask[i + w] } else { false };
            if left != inside || right != inside || up != inside || down != inside {
                dist[i] = 0.0;
            }
        }
    }
    let d1 = 1.0f32;
    let d2 = std::f32::consts::SQRT_2;
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let mut d = dist[i];
            if x > 0 {
                d = d.min(dist[i - 1] + d1);
            }
            if y > 0 {
                d = d.min(dist[i - w] + d1);
                if x > 0 {
                    d = d.min(dist[i - w - 1] + d2);
                }
                if x + 1 < w {
                    d = d.min(dist[i - w + 1] + d2);
                }
            }
            dist[i] = d;
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let i = y * w + x;
            let mut d = dist[i];
            if x + 1 < w {
                d = d.min(dist[i + 1] + d1);
            }
            if y + 1 < h {
                d = d.min(dist[i + w] + d1);
                if x + 1 < w {
                    d = d.min(dist[i + w + 1] + d2);
                }
                if x > 0 {
                    d = d.min(dist[i + w - 1] + d2);
                }
            }
            dist[i] = d;
        }
    }
    let mut sdf = vec![0.0f32; w * h];
    for i in 0..w * h {
        let v = dist[i] * res;
        sdf[i] = if mask[i] { -v } else { v };
    }
    let mut out = Vec::new();
    for contour in iso_contours(&sdf, w, h, distance_mm) {
        let raw: Vec<(f32, f32)> = contour
            .iter()
            .map(|(x, y)| (ox + x * res, oy + y * res))
            .collect();
        let pts = simplify(&raw, res * 0.75, true);
        if pts.len() >= 3 && polyline_length(&pts, true) >= res * 4.0 {
            out.push(pts);
        }
    }
    out
}

fn luminance(rgba: &[u8], w: usize, h: usize) -> Vec<f32> {
    let mut lum = vec![255.0f32; w * h];
    for i in 0..w * h {
        let p = &rgba[i * 4..i * 4 + 4];
        let a = p[3] as f32 / 255.0;
        let l = 0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32;
        lum[i] = l * a + 255.0 * (1.0 - a);
    }
    lum
}

fn sobel(lum: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h];
    if w < 3 || h < 3 {
        return out;
    }
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let i = y * w + x;
            let gx = -lum[i - w - 1] + lum[i - w + 1] - 2.0 * lum[i - 1] + 2.0 * lum[i + 1]
                - lum[i + w - 1]
                + lum[i + w + 1];
            let gy = -lum[i - w - 1] - 2.0 * lum[i - w] - lum[i - w + 1]
                + lum[i + w - 1]
                + 2.0 * lum[i + w]
                + lum[i + w + 1];
            out[i] = (gx * gx + gy * gy).sqrt();
        }
    }
    out
}

fn thin(mask: &mut [bool], w: usize, h: usize) {
    if w < 3 || h < 3 {
        return;
    }
    for _ in 0..100 {
        let mut changed = false;
        for step in 0..2 {
            let mut remove = Vec::new();
            for y in 1..h - 1 {
                for x in 1..w - 1 {
                    let i = y * w + x;
                    if !mask[i] {
                        continue;
                    }
                    let p2 = mask[i - w] as u8;
                    let p3 = mask[i - w + 1] as u8;
                    let p4 = mask[i + 1] as u8;
                    let p5 = mask[i + w + 1] as u8;
                    let p6 = mask[i + w] as u8;
                    let p7 = mask[i + w - 1] as u8;
                    let p8 = mask[i - 1] as u8;
                    let p9 = mask[i - w - 1] as u8;
                    let b = p2 + p3 + p4 + p5 + p6 + p7 + p8 + p9;
                    if !(2..=6).contains(&b) {
                        continue;
                    }
                    let seq = [p2, p3, p4, p5, p6, p7, p8, p9, p2];
                    let mut transitions = 0;
                    for k in 0..8 {
                        if seq[k] == 0 && seq[k + 1] == 1 {
                            transitions += 1;
                        }
                    }
                    if transitions != 1 {
                        continue;
                    }
                    let (c1, c2) = if step == 0 {
                        (p2 * p4 * p6, p4 * p6 * p8)
                    } else {
                        (p2 * p4 * p8, p2 * p6 * p8)
                    };
                    if c1 == 0 && c2 == 0 {
                        remove.push(i);
                    }
                }
            }
            if !remove.is_empty() {
                changed = true;
            }
            for i in remove {
                mask[i] = false;
            }
        }
        if !changed {
            break;
        }
    }
}

fn trace_skeleton(mask: &[bool], w: usize, h: usize) -> Vec<(Vec<(f32, f32)>, bool)> {
    let mut visited: HashSet<(usize, usize)> = HashSet::new();
    let mut out = Vec::new();
    let mut endpoints = Vec::new();
    for i in 0..w * h {
        if mask[i] && pixel_neighbors(i, mask, w, h).len() == 1 {
            endpoints.push(i);
        }
    }
    endpoints.sort_unstable();
    for start in endpoints {
        for next in pixel_neighbors(start, mask, w, h) {
            let key = edge_key(start, next);
            if visited.contains(&key) {
                continue;
            }
            let (path, closed) = walk(start, next, mask, w, h, &mut visited);
            if path.len() >= 2 {
                out.push((path, closed));
            }
        }
    }
    for start in 0..w * h {
        if !mask[start] {
            continue;
        }
        for next in pixel_neighbors(start, mask, w, h) {
            let key = edge_key(start, next);
            if visited.contains(&key) {
                continue;
            }
            let (path, closed) = walk(start, next, mask, w, h, &mut visited);
            if path.len() >= 2 {
                out.push((path, closed));
            }
        }
    }
    out
}

fn walk(
    start: usize,
    next: usize,
    mask: &[bool],
    w: usize,
    h: usize,
    visited: &mut HashSet<(usize, usize)>,
) -> (Vec<(f32, f32)>, bool) {
    let mut path = vec![start, next];
    visited.insert(edge_key(start, next));
    let mut prev = start;
    let mut current = next;
    loop {
        if current == start {
            path.push(start);
            let points = path
                .iter()
                .map(|&i| ((i % w) as f32, (i / w) as f32))
                .collect();
            return (points, true);
        }
        let mut candidates: Vec<usize> = pixel_neighbors(current, mask, w, h)
            .into_iter()
            .filter(|&n| n != prev && !visited.contains(&edge_key(current, n)))
            .collect();
        if candidates.is_empty() {
            break;
        }
        let incoming = direction(prev, current, w);
        candidates.sort_by(|a, b| {
            let da = direction(current, *a, w);
            let db = direction(current, *b, w);
            let sa = incoming.0 * da.0 + incoming.1 * da.1;
            let sb = incoming.0 * db.0 + incoming.1 * db.1;
            sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
        });
        let candidate = candidates[0];
        visited.insert(edge_key(current, candidate));
        path.push(candidate);
        prev = current;
        current = candidate;
    }
    let points = path
        .iter()
        .map(|&i| ((i % w) as f32, (i / w) as f32))
        .collect();
    (points, false)
}

fn direction(from: usize, to: usize, w: usize) -> (f32, f32) {
    let fx = (from % w) as f32;
    let fy = (from / w) as f32;
    let tx = (to % w) as f32;
    let ty = (to / w) as f32;
    let dx = tx - fx;
    let dy = ty - fy;
    let len = (dx * dx + dy * dy).sqrt();
    if len > 1e-6 {
        (dx / len, dy / len)
    } else {
        (0.0, 0.0)
    }
}

fn join_segments(
    mut segments: Vec<(Vec<(f32, f32)>, bool)>,
    max_gap: f32,
) -> Vec<(Vec<(f32, f32)>, bool)> {
    let mut out = Vec::new();
    let mut open: Vec<Vec<(f32, f32)>> = Vec::new();
    for (points, closed) in segments.drain(..) {
        if closed {
            out.push((points, true));
        } else if points.len() >= 2 {
            open.push(points);
        }
    }
    let gap2 = max_gap * max_gap;
    while let Some(mut pts) = open.pop() {
        loop {
            let end = *pts.last().unwrap();
            let start = pts[0];
            let mut best_end: Option<(usize, bool, f32)> = None;
            let mut best_start: Option<(usize, bool, f32)> = None;
            for (i, other) in open.iter().enumerate() {
                let a = other[0];
                let b = *other.last().unwrap();
                for (reverse, p) in [(false, a), (true, b)] {
                    let de = (p.0 - end.0).powi(2) + (p.1 - end.1).powi(2);
                    if de <= gap2 && best_end.map_or(true, |(_, _, bd)| de < bd) {
                        best_end = Some((i, reverse, de));
                    }
                    let ds = (p.0 - start.0).powi(2) + (p.1 - start.1).powi(2);
                    if ds <= gap2 && best_start.map_or(true, |(_, _, bd)| ds < bd) {
                        best_start = Some((i, reverse, ds));
                    }
                }
            }
            if let Some((i, reverse, _)) = best_end {
                let mut other = open.remove(i);
                if reverse {
                    other.reverse();
                }
                if other[0] == end {
                    other.remove(0);
                }
                pts.extend(other);
                continue;
            }
            let Some((i, reverse, _)) = best_start else {
                break;
            };
            let mut other = open.remove(i);
            if !reverse {
                other.reverse();
            }
            if *other.last().unwrap() == start {
                other.pop();
            }
            other.extend(pts);
            pts = other;
        }
        let first = pts[0];
        let last = *pts.last().unwrap();
        let closed =
            pts.len() > 4 && (first.0 - last.0).powi(2) + (first.1 - last.1).powi(2) <= gap2;
        if closed {
            pts.push(first);
        }
        out.push((pts, closed));
    }
    out
}

fn pixel_neighbors(idx: usize, mask: &[bool], w: usize, h: usize) -> Vec<usize> {
    let x = idx % w;
    let y = idx / w;
    let mut out = Vec::new();
    for dy in -1..=1i32 {
        for dx in -1..=1i32 {
            if dx == 0 && dy == 0 {
                continue;
            }
            let nx = x as i32 + dx;
            let ny = y as i32 + dy;
            if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                continue;
            }
            let ni = ny as usize * w + nx as usize;
            if mask[ni] {
                out.push(ni);
            }
        }
    }
    out
}

fn edge_key(a: usize, b: usize) -> (usize, usize) {
    if a < b {
        (a, b)
    } else {
        (b, a)
    }
}

fn iso_contours(field: &[f32], w: usize, h: usize, threshold: f32) -> Vec<Vec<(f32, f32)>> {
    if w < 2 || h < 2 || field.len() < w * h {
        return Vec::new();
    }
    let mut coords: HashMap<usize, (f32, f32)> = HashMap::new();
    let mut adj: HashMap<usize, Vec<usize>> = HashMap::new();
    for y in 0..h - 1 {
        for x in 0..w - 1 {
            let i = y * w + x;
            let a = field[i];
            let b = field[i + 1];
            let c = field[i + w + 1];
            let d = field[i + w];
            let mut case = 0u8;
            if a >= threshold {
                case |= 1;
            }
            if b >= threshold {
                case |= 2;
            }
            if c >= threshold {
                case |= 4;
            }
            if d >= threshold {
                case |= 8;
            }
            if case == 0 || case == 15 {
                continue;
            }
            let top = 2 * (y * w + x);
            let bottom = 2 * ((y + 1) * w + x);
            let left = 2 * (y * w + x) + 1;
            let right = 2 * (y * w + x + 1) + 1;
            let top_p = (x as f32 + interp(a, b, threshold), y as f32);
            let bottom_p = (x as f32 + interp(d, c, threshold), (y + 1) as f32);
            let left_p = (x as f32, y as f32 + interp(a, d, threshold));
            let right_p = ((x + 1) as f32, y as f32 + interp(b, c, threshold));
            let center = (a + b + c + d) / 4.0;
            let mut segment = |u: usize, v: usize, pu: (f32, f32), pv: (f32, f32)| {
                coords.insert(u, pu);
                coords.insert(v, pv);
                adj.entry(u).or_default().push(v);
                adj.entry(v).or_default().push(u);
            };
            match case {
                1 | 14 => segment(left, top, left_p, top_p),
                2 | 13 => segment(top, right, top_p, right_p),
                3 | 12 => segment(left, right, left_p, right_p),
                4 | 11 => segment(right, bottom, right_p, bottom_p),
                6 | 9 => segment(top, bottom, top_p, bottom_p),
                7 | 8 => segment(left, bottom, left_p, bottom_p),
                5 => {
                    if center >= threshold {
                        segment(left, top, left_p, top_p);
                        segment(right, bottom, right_p, bottom_p);
                    } else {
                        segment(left, bottom, left_p, bottom_p);
                        segment(top, right, top_p, right_p);
                    }
                }
                10 => {
                    if center >= threshold {
                        segment(left, bottom, left_p, bottom_p);
                        segment(top, right, top_p, right_p);
                    } else {
                        segment(left, top, left_p, top_p);
                        segment(right, bottom, right_p, bottom_p);
                    }
                }
                _ => {}
            }
        }
    }
    let mut keys: Vec<usize> = adj.keys().copied().collect();
    keys.sort_by_key(|k| adj.get(k).map_or(0, Vec::len));
    let mut visited: HashSet<usize> = HashSet::new();
    let mut out = Vec::new();
    for start in keys {
        if visited.contains(&start) {
            continue;
        }
        let mut path = Vec::new();
        let mut current = start;
        let mut prev: Option<usize> = None;
        loop {
            visited.insert(current);
            if let Some(p) = coords.get(&current) {
                path.push(*p);
            }
            let nexts = adj.get(&current).map(Vec::as_slice).unwrap_or(&[]);
            let next = nexts
                .iter()
                .copied()
                .find(|n| Some(*n) != prev && !visited.contains(n));
            match next {
                Some(n) => {
                    prev = Some(current);
                    current = n;
                }
                None => break,
            }
        }
        if path.len() >= 3 {
            out.push(path);
        }
    }
    out
}

fn interp(a: f32, b: f32, threshold: f32) -> f32 {
    let d = b - a;
    if d.abs() < 1e-9 {
        0.5
    } else {
        ((threshold - a) / d).clamp(0.0, 1.0)
    }
}

pub fn point_in_polygon(point: (f32, f32), polygon: &[(f32, f32)]) -> bool {
    if polygon.len() < 3 {
        return false;
    }
    let (px, py) = point;
    let mut inside = false;
    let mut j = polygon.len() - 1;
    for i in 0..polygon.len() {
        let (xi, yi) = polygon[i];
        let (xj, yj) = polygon[j];
        if ((yi > py) != (yj > py)) && px < (xj - xi) * (py - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Turn angle at a vertex, in degrees (0 = perfectly straight).
fn vertex_turn(prev: (f32, f32), v: (f32, f32), next: (f32, f32)) -> f32 {
    let (ax, ay) = (v.0 - prev.0, v.1 - prev.1);
    let (bx, by) = (next.0 - v.0, next.1 - v.1);
    let (la, lb) = (ax.hypot(ay), bx.hypot(by));
    if la < 1e-9 || lb < 1e-9 {
        return 0.0;
    }
    let dot = ((ax * bx + ay * by) / (la * lb)).clamp(-1.0, 1.0);
    dot.acos().to_degrees()
}

/// Relative roughness: mean absolute turn angle over the vertices,
/// normalised by 90° (0 = smooth, 1 = sharp turn at every vertex — pixel
/// staircases and other jagged contours score high even when every segment
/// has the same length).
fn roughness(points: &[(f32, f32)], closed: bool) -> f32 {
    let n = points.len();
    if n < 3 {
        return 0.0;
    }
    let last = if closed { n } else { n - 1 };
    let mut sum = 0.0f32;
    let mut count = 0u32;
    for i in 0..last {
        let prev = if i == 0 { n - 1 } else { i - 1 };
        let next = (i + 1) % n;
        if !closed && (i == 0) {
            continue;
        }
        let turn = vertex_turn(points[prev], points[i], points[next]);
        if turn.is_finite() {
            sum += turn;
            count += 1;
        }
    }
    if count == 0 {
        return 0.0;
    }
    (sum / count as f32 / 90.0).clamp(0.0, 2.0)
}

/// Resample so consecutive points are about `spacing` apart; endpoints are
/// kept exactly. Uneven input spacing (marching squares, spline flattening)
/// makes smoothing wobble — uniform spacing is what keeps contours uniform.
fn resample_uniform(
    points: &[(f32, f32)],
    closed: bool,
    spacing: f32,
) -> Vec<(f32, f32)> {
    let spacing = spacing.max(1e-4);
    let n = points.len();
    if n < 2 {
        return points.to_vec();
    }
    let mut out = vec![points[0]];
    let mut carry = 0.0f32;
    let last = if closed { n } else { n - 1 };
    for i in 0..last {
        let a = points[i];
        let b = points[(i + 1) % n];
        let seg = (b.0 - a.0).hypot(b.1 - a.1);
        if seg < 1e-9 {
            continue;
        }
        let (dx, dy) = ((b.0 - a.0) / seg, (b.1 - a.1) / seg);
        let mut d = carry;
        while d < seg {
            out.push((a.0 + dx * d, a.1 + dy * d));
            d += spacing;
        }
        carry = d - seg;
    }
    if !closed {
        out.push(points[n - 1]);
    } else if out.len() > 3 {
        out.pop(); // the loop closure point duplicates the first
    }
    out
}

/// One corner-preserving smoothing pass: non-corner vertices pull toward the
/// neighbour average; marked corners stay exactly where they are.
fn smooth_pass(points: &[(f32, f32)], closed: bool, corners: &[bool]) -> Vec<(f32, f32)> {
    let n = points.len();
    if n < 3 {
        return points.to_vec();
    }
    let mut next = Vec::with_capacity(n);
    for i in 0..n {
        if corners[i] {
            next.push(points[i]);
            continue;
        }
        if !closed && (i == 0 || i == n - 1) {
            next.push(points[i]);
            continue;
        }
        let prev = if i == 0 { n - 1 } else { i - 1 };
        let following = (i + 1) % n;
        let (p, c, q) = (points[prev], points[i], points[following]);
        next.push((
            0.25 * p.0 + 0.5 * c.0 + 0.25 * q.0,
            0.25 * p.1 + 0.5 * c.1 + 0.25 * q.1,
        ));
    }
    next
}

/// Full contour polish: denoise, resample uniformly, then corner-preserving
/// smoothing with auto-correction — the rougher the input, the more passes it
/// gets (bounded), so jagged contours even out without user intervention.
/// `tolerance` and the points share one unit (px or mm).
pub fn polish(
    points: &[(f32, f32)],
    closed: bool,
    tolerance: f32,
    corner_deg: f32,
    passes: u8,
) -> Vec<(f32, f32)> {
    if points.len() < 3 {
        return points.to_vec();
    }
    // Denoise lightly before measuring, so single-pixel spikes do not fake
    // roughness.
    let denoised = simplify(points, tolerance.max(0.02) * 0.5, closed);
    // Auto-correction: the rougher the input, the more smoothing passes it
    // gets (0.35+ triggers, scaling up to 3 extra passes).
    let r = roughness(&denoised, closed);
    let extra = if r > 0.35 {
        (((r - 0.35) * 4.0).ceil() as u8).clamp(1, 3)
    } else {
        0
    };
    let passes = (passes + extra).min(6);
    let spacing = (tolerance * 2.0).max(0.2);
    let resampled = resample_uniform(&denoised, closed, spacing);
    let corners: Vec<bool> = (0..resampled.len())
        .map(|i| {
            let n = resampled.len();
            let prev = if i == 0 { n - 1 } else { i - 1 };
            let next = (i + 1) % n;
            vertex_turn(resampled[prev], resampled[i], resampled[next]) >= corner_deg
        })
        .collect();
    let mut pts = resampled;
    for _ in 0..passes {
        pts = smooth_pass(&pts, closed, &corners);
    }
    simplify(&pts, tolerance, closed)
}

fn finish(points: Vec<(f32, f32)>, closed: bool, options: &TraceOptions) -> Option<TracedPath> {
    let scale = options.mm_per_px.max(1e-6);
    // Polish in pixel space: uniform resampling + corner-preserving smoothing
    // with auto-correction (a 110° corner guard treats pixel staircases as
    // noise while keeping genuine spikes), then simplify.
    let tolerance = options.simplify_mm / scale;
    let polished = polish(&points, closed, tolerance, 110.0, options.smooth_iterations);
    if polished.len() < 2 {
        return None;
    }
    let mm: Vec<(f32, f32)> = polished
        .into_iter()
        .map(|(x, y)| (x * scale, y * scale))
        .collect();
    if polyline_length(&mm, closed) < options.min_length_mm {
        return None;
    }
    Some(TracedPath { points: mm, closed })
}

/// Ramer–Douglas–Peucker simplification. `tolerance` is in the same unit as
/// the points (pixels when called from [`trace_rgba`], millimetres when
/// applied to placed [`TracedPath`]s).
pub fn simplify(
    points: &[(f32, f32)],
    tolerance: f32,
    closed: bool,
) -> Vec<(f32, f32)> {
    if points.len() <= 2 || tolerance <= 0.0 {
        return points.to_vec();
    }
    if closed {
        let mut looped = points.to_vec();
        looped.push(points[0]);
        let mut result = simplify_open(&looped, tolerance);
        if result.len() > 1 {
            result.pop();
        }
        result
    } else {
        simplify_open(points, tolerance)
    }
}

fn simplify_open(points: &[(f32, f32)], tolerance: f32) -> Vec<(f32, f32)> {
    if points.len() <= 2 {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut stack = vec![(0usize, points.len() - 1)];
    while let Some((a, b)) = stack.pop() {
        if b <= a + 1 {
            continue;
        }
        let (ax, ay) = points[a];
        let (bx, by) = points[b];
        let dx = bx - ax;
        let dy = by - ay;
        let len2 = dx * dx + dy * dy;
        let mut max_d = -1.0f32;
        let mut max_i = a;
        for (i, (px, py)) in points
            .iter()
            .enumerate()
            .take(b)
            .skip(a + 1)
            .map(|(i, p)| (i, *p))
        {
            let d = if len2 <= 1e-12 {
                ((px - ax).powi(2) + (py - ay).powi(2)).sqrt()
            } else {
                let t = (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0);
                let qx = ax + t * dx;
                let qy = ay + t * dy;
                ((px - qx).powi(2) + (py - qy).powi(2)).sqrt()
            };
            if d > max_d {
                max_d = d;
                max_i = i;
            }
        }
        if max_d > tolerance {
            keep[max_i] = true;
            stack.push((a, max_i));
            stack.push((max_i, b));
        }
    }
    points
        .iter()
        .enumerate()
        .filter(|(i, _)| keep[*i])
        .map(|(_, p)| *p)
        .collect()
}

fn polyline_length(points: &[(f32, f32)], closed: bool) -> f32 {
    if points.len() < 2 {
        return 0.0;
    }
    let mut len = 0.0;
    for i in 1..points.len() {
        let dx = points[i].0 - points[i - 1].0;
        let dy = points[i].1 - points[i - 1].1;
        len += (dx * dx + dy * dy).sqrt();
    }
    if closed && points.len() > 2 {
        let dx = points[0].0 - points[points.len() - 1].0;
        let dy = points[0].1 - points[points.len() - 1].1;
        len += (dx * dx + dy * dy).sqrt();
    }
    len
}

fn bbox(points: &[(f32, f32)]) -> (f32, f32, f32, f32) {
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for (x, y) in points {
        min_x = min_x.min(*x);
        min_y = min_y.min(*y);
        max_x = max_x.max(*x);
        max_y = max_y.max(*y);
    }
    (min_x, min_y, max_x, max_y)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas(w: usize, h: usize) -> Vec<u8> {
        let mut rgba = vec![255u8; w * h * 4];
        for p in rgba.chunks_exact_mut(4) {
            p[3] = 255;
        }
        rgba
    }

    fn black_rect(rgba: &mut [u8], w: usize, x0: usize, y0: usize, x1: usize, y1: usize) {
        for y in y0..y1 {
            for x in x0..x1 {
                let i = (y * w + x) * 4;
                rgba[i] = 0;
                rgba[i + 1] = 0;
                rgba[i + 2] = 0;
            }
        }
    }

    fn path_bbox(points: &[(f32, f32)]) -> (f32, f32, f32, f32) {
        bbox(points)
    }

    #[test]
    fn outline_finds_one_closed_rectangle() {
        let w = 100;
        let h = 100;
        let mut rgba = canvas(w, h);
        black_rect(&mut rgba, w, 20, 20, 80, 80);
        let paths = trace_rgba(
            &rgba,
            w as u32,
            h as u32,
            &TraceOptions {
                mode: TraceMode::Outline,
                threshold: 128,
                simplify_mm: 0.0,
                min_length_mm: 0.0,
                mm_per_px: 1.0,
                ..TraceOptions::default()
            },
        );
        assert_eq!(paths.len(), 1);
        assert!(paths[0].closed);
        let (x0, y0, x1, y1) = path_bbox(&paths[0].points);
        assert!((x0 - 19.5).abs() < 1.5, "{x0}");
        assert!((y0 - 19.5).abs() < 1.5, "{y0}");
        assert!((x1 - 79.5).abs() < 1.5, "{x1}");
        assert!((y1 - 79.5).abs() < 1.5, "{y1}");
    }

    #[test]
    fn outline_keeps_the_hole_of_a_donut() {
        let w = 100;
        let h = 100;
        let mut rgba = canvas(w, h);
        black_rect(&mut rgba, w, 10, 10, 90, 90);
        black_rect(&mut rgba, w, 35, 35, 65, 65);
        for y in 35..65 {
            for x in 35..65 {
                let i = (y * w + x) * 4;
                rgba[i] = 255;
                rgba[i + 1] = 255;
                rgba[i + 2] = 255;
            }
        }
        let paths = trace_rgba(
            &rgba,
            w as u32,
            h as u32,
            &TraceOptions {
                mode: TraceMode::Outline,
                threshold: 128,
                simplify_mm: 0.0,
                min_length_mm: 0.0,
                mm_per_px: 1.0,
                smooth_iterations: 0,
                ..TraceOptions::default()
            },
        );
        // Outer boundary + the hole: both are cut lines.
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn invert_traces_bright_content_on_dark_background() {
        let w = 100;
        let h = 100;
        let mut rgba = canvas(w, h); // white canvas
        // Bright square on a dark background.
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                rgba[i] = 20;
                rgba[i + 1] = 20;
                rgba[i + 2] = 20;
            }
        }
        for y in 30..70 {
            for x in 30..70 {
                let i = (y * w + x) * 4;
                rgba[i] = 240;
                rgba[i + 1] = 240;
                rgba[i + 2] = 240;
            }
        }
        let paths = trace_rgba(
            &rgba,
            w as u32,
            h as u32,
            &TraceOptions {
                mode: TraceMode::Outline,
                threshold: 128,
                simplify_mm: 0.0,
                min_length_mm: 0.0,
                mm_per_px: 1.0,
                invert: true,
                smooth_iterations: 0,
                ..TraceOptions::default()
            },
        );
        assert_eq!(paths.len(), 1);
        let (x0, y0, x1, y1) = path_bbox(&paths[0].points);
        assert!((x0 - 30.0).abs() < 1.5 && (x1 - 70.0).abs() < 1.5);
        assert!((y0 - 30.0).abs() < 1.5 && (y1 - 70.0).abs() < 1.5);
    }

    #[test]
    fn colour_distance_traces_pale_coloured_content() {
        // A pale-cyan square on white: luminance ~230 would miss it with a
        // 230 threshold; colour distance catches it easily.
        let w = 100;
        let h = 100;
        let mut rgba = canvas(w, h); // white
        for y in 30..70 {
            for x in 30..70 {
                let i = (y * w + x) * 4;
                rgba[i] = 190;
                rgba[i + 1] = 235;
                rgba[i + 2] = 245;
            }
        }
        let paths = trace_rgba(
            &rgba,
            w as u32,
            h as u32,
            &TraceOptions {
                mode: TraceMode::Outline,
                threshold: 230,
                bg_color: Some([255, 255, 255]),
                color_tolerance: 25,
                simplify_mm: 0.0,
                min_length_mm: 0.0,
                mm_per_px: 1.0,
                smooth_iterations: 0,
                ..TraceOptions::default()
            },
        );
        assert_eq!(paths.len(), 1, "pale coloured square must be traced");
        let (x0, y0, x1, y1) = path_bbox(&paths[0].points);
        assert!((x0 - 30.0).abs() < 1.5 && (x1 - 70.0).abs() < 1.5);
        assert!((y0 - 30.0).abs() < 1.5 && (y1 - 70.0).abs() < 1.5);
    }

    #[test]
    fn polish_smooths_jagged_contours_but_keeps_sharp_corners() {
        // A 90° zigzag with a 110° guard: the turns are treated as pixel noise
        // and smoothed away — and the auto-correction kicks in even with zero
        // passes requested, because the input is rough.
        let zig: Vec<(f32, f32)> = (0..40)
            .flat_map(|i| [(i as f32, if i % 2 == 0 { 0.0 } else { 1.0 })])
            .collect();
        let len = |ps: &[(f32, f32)]| polyline_length(ps, false);
        let auto = polish(&zig, false, 0.05, 110.0, 0);
        assert!(
            len(&auto) < len(&zig) * 0.9,
            "raw {:.2} auto-polished {:.2}",
            len(&zig),
            len(&auto)
        );
        // With a 35° guard the same 90° turn is a real corner and must stay.
        let corner = vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)];
        let kept = polish(&corner, false, 0.05, 35.0, 2);
        assert!(
            kept.iter()
                .any(|p| (p.0 - 10.0).abs() < 0.2 && p.1.abs() < 0.2)
        );
        // …and with a 110° guard it is smoothed off.
        let rounded = polish(&corner, false, 0.05, 110.0, 2);
        assert!(
            !rounded
                .iter()
                .any(|p| (p.0 - 10.0).abs() < 0.05 && p.1.abs() < 0.05),
            "the corner should have been rounded"
        );
    }

    #[test]
    fn polish_denoises_micro_steps() {
        // Two long edges plus 0.05 mm micro-steps: the polish must remove the
        // noise while keeping the overall shape (RDP thins the straight runs).
        let mut pts = vec![(0.0, 0.0), (10.0, 0.0)];
        for i in 0..20 {
            let jitter = if i % 2 == 0 { 0.05 } else { 0.0 };
            pts.push((10.0 + i as f32 * 0.05, 1.0 + jitter));
        }
        let out = polish(&pts, false, 0.2, 110.0, 1);
        assert!(out.len() < pts.len(), "{} -> {}", pts.len(), out.len());
        let (mut x1, mut y1) = (f32::MIN, f32::MIN);
        for (x, y) in &out {
            x1 = x1.max(*x);
            y1 = y1.max(*y);
        }
        assert!((x1 - 10.95).abs() < 0.5, "x1 {x1}");
        assert!((y1 - 1.05).abs() < 0.3, "y1 {y1}");
    }

    #[test]
    fn edges_produce_at_least_one_path() {
        let w = 80;
        let h = 80;
        let mut rgba = canvas(w, h);
        black_rect(&mut rgba, w, 20, 20, 60, 60);
        let paths = trace_rgba(
            &rgba,
            w as u32,
            h as u32,
            &TraceOptions {
                mode: TraceMode::Edges,
                edge_threshold: 40,
                simplify_mm: 0.0,
                min_length_mm: 0.0,
                mm_per_px: 1.0,
                ..TraceOptions::default()
            },
        );
        assert!(!paths.is_empty());
        assert!(paths.iter().all(|p| p.points.len() >= 2));
        let total: f32 = paths
            .iter()
            .map(|p| polyline_length(&p.points, p.closed))
            .sum();
        assert!(total > 100.0, "{total}");
    }

    #[test]
    fn edges_of_a_thick_ring_stay_few() {
        let w = 200;
        let h = 200;
        let mut rgba = canvas(w, h);
        for y in 0..h {
            for x in 0..w {
                let dx = x as f32 - 100.0;
                let dy = y as f32 - 100.0;
                let d = (dx * dx + dy * dy).sqrt();
                if (60.0..72.0).contains(&d) {
                    let i = (y * w + x) * 4;
                    rgba[i] = 0;
                    rgba[i + 1] = 0;
                    rgba[i + 2] = 0;
                }
            }
        }
        let paths = trace_rgba(
            &rgba,
            w as u32,
            h as u32,
            &TraceOptions {
                mode: TraceMode::Edges,
                edge_threshold: 80,
                simplify_mm: 0.2,
                min_length_mm: 2.0,
                mm_per_px: 1.0,
                ..TraceOptions::default()
            },
        );
        assert!(paths.len() < 30, "{} paths", paths.len());
    }

    #[test]
    fn external_offset_grows_the_rectangle() {
        let square = vec![(0.0, 0.0), (20.0, 0.0), (20.0, 20.0), (0.0, 20.0)];
        let out = offset_polygon(&square, 2.0, 0.25);
        assert_eq!(out.len(), 1);
        let (x0, y0, x1, y1) = path_bbox(&out[0]);
        assert!((x0 + 2.0).abs() < 0.6, "{x0}");
        assert!((y0 + 2.0).abs() < 0.6, "{y0}");
        assert!((x1 - 22.0).abs() < 0.6, "{x1}");
        assert!((y1 - 22.0).abs() < 0.6, "{y1}");
    }

    #[test]
    fn internal_offset_shrinks_the_rectangle() {
        let square = vec![(0.0, 0.0), (20.0, 0.0), (20.0, 20.0), (0.0, 20.0)];
        let out = offset_polygon(&square, -2.0, 0.25);
        assert_eq!(out.len(), 1);
        let (x0, y0, x1, y1) = path_bbox(&out[0]);
        assert!((x0 - 2.0).abs() < 0.6, "{x0}");
        assert!((y0 - 2.0).abs() < 0.6, "{y0}");
        assert!((x1 - 18.0).abs() < 0.6, "{x1}");
        assert!((y1 - 18.0).abs() < 0.6, "{y1}");
    }
}
