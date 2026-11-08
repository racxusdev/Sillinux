use std::path::{Path as FsPath, PathBuf};
use std::process::Command;
use std::sync::Arc;

use base64::Engine;
use resvg::usvg;
use silhouette_core::{raster_placement, Container, Path as CorePath, PathKind, Point};

const MM_PER_PX: f32 = 25.4 / 96.0;

pub struct Raster {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub rect_mm: (f32, f32, f32, f32),
    pub name: String,
}

pub struct Imported {
    pub page_mm: (f32, f32),
    pub paths: Vec<CorePath>,
    pub art: Option<Raster>,
    pub notes: Vec<String>,
}

pub fn load(path: &FsPath, fonts: &[FontEntry]) -> Result<Imported, String> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "studio3" => load_studio3(path),
        "svg" => {
            let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
            load_svg(&data, path, fonts)
        }
        "pdf" => load_pdf(path),
        "psd" => {
            let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
            load_psd(&data, path)
        }
        "png" | "jpg" | "jpeg" | "tif" | "tiff" | "webp" => {
            let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
            load_raster(&data, path)
        }
        other => Err(format!("formato não suportado: .{other}")),
    }
}

fn load_studio3(path: &FsPath) -> Result<Imported, String> {
    let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let doc = Container::parse(&data).map_err(|e| format!("{}: {e}", path.display()))?;
    let page = doc.page_size_mm.unwrap_or((210.0, 297.0));
    let mut notes = vec![format!("{} traços", doc.paths.len())];
    let art = doc
        .images
        .iter()
        .max_by_key(|i| u64::from(i.width) * u64::from(i.height))
        .and_then(|img| {
            let (rgba, w, h) = decode_image(img.data).ok()?;
            let (scale, x, y) = match raster_placement(&data, img.width, img.height, page.0, page.1)
            {
                Some(p) => (p.scale, p.x_mm, p.y_mm),
                None => fit_to_page(page, w, h, 10.0),
            };
            notes.push(format!("arte {}×{} px", w, h));
            Some(Raster {
                rgba,
                width: w,
                height: h,
                rect_mm: (x, y, w as f32 * scale, h as f32 * scale),
                name: "arte embebida".to_string(),
            })
        });
    Ok(Imported {
        page_mm: page,
        paths: doc.paths,
        art,
        notes,
    })
}

fn load_svg(data: &[u8], path: &FsPath, fonts: &[FontEntry]) -> Result<Imported, String> {
    let mut opt = usvg::Options {
        resources_dir: path.parent().map(|p| p.to_path_buf()),
        ..Default::default()
    };
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    for font in fonts {
        db.load_font_data(font.bytes.clone());
    }
    opt.fontdb = Arc::new(db);
    let tree = usvg::Tree::from_data(data, &opt).map_err(|e| format!("SVG inválido: {e}"))?;
    let size = tree.size();
    let natural = (size.width() * MM_PER_PX, size.height() * MM_PER_PX);
    let page = if natural.0 >= 150.0 && natural.1 >= 150.0 {
        natural
    } else {
        (210.0, 297.0)
    };

    let (rgba, w, h) = render_svg(&tree, natural)?;
    let (scale, x, y) = if page == natural {
        (natural.0 / w as f32, 0.0, 0.0)
    } else {
        fit_to_page(page, w, h, 10.0)
    };

    let mut candidates = Vec::new();
    collect_svg_paths(tree.root(), &mut candidates, size.width(), size.height());
    let stroked_any = candidates.iter().any(|c| c.stroked);
    let mut paths: Vec<CorePath> = candidates
        .into_iter()
        .filter(|c| {
            if c.stroked {
                true
            } else if stroked_any {
                false
            } else {
                c.filled && !c.background
            }
        })
        .map(|c| c.path)
        .collect();
    if paths.is_empty() {
        paths = svg_all_paths(tree.root(), size.width(), size.height());
    }

    Ok(Imported {
        page_mm: page,
        notes: vec![format!("{} traços vetoriais", paths.len())],
        paths,
        art: Some(Raster {
            rgba,
            width: w,
            height: h,
            rect_mm: (x, y, w as f32 * scale, h as f32 * scale),
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "svg".to_string()),
        }),
    })
}

fn load_raster(data: &[u8], path: &FsPath) -> Result<Imported, String> {
    let (rgba, w, h) = decode_image(data)?;
    let page = (210.0, 297.0);
    let (scale, x, y) = fit_to_page(page, w, h, 10.0);
    Ok(Imported {
        page_mm: page,
        paths: Vec::new(),
        notes: vec![format!("imagem {w}×{h} px")],
        art: Some(Raster {
            rgba,
            width: w,
            height: h,
            rect_mm: (x, y, w as f32 * scale, h as f32 * scale),
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "imagem".to_string()),
        }),
    })
}

fn load_psd(data: &[u8], path: &FsPath) -> Result<Imported, String> {
    let psd = psd::Psd::from_bytes(data).map_err(|e| format!("PSD inválido: {e}"))?;
    let w = psd.width();
    let h = psd.height();
    let mut rgba = psd.rgba();
    composite_white(&mut rgba);
    let page = (210.0, 297.0);
    let (scale, x, y) = fit_to_page(page, w, h, 10.0);
    let layers = psd.layers().len();
    Ok(Imported {
        page_mm: page,
        paths: Vec::new(),
        notes: vec![format!(
            "PSD achatado: {w}×{h} px, {layers} camadas de origem"
        )],
        art: Some(Raster {
            rgba,
            width: w,
            height: h,
            rect_mm: (x, y, w as f32 * scale, h as f32 * scale),
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "psd".to_string()),
        }),
    })
}

fn load_pdf(path: &FsPath) -> Result<Imported, String> {
    let output = Command::new("pdftoppm")
        .args(["-png", "-singlefile", "-r", "200", "-f", "1", "-l", "1"])
        .arg(path)
        .arg("-")
        .output()
        .map_err(|e| format!("pdftoppm: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "pdftoppm falhou: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let (rgba, w, h) = decode_image(&output.stdout)?;
    let page = (w as f32 / 200.0 * 25.4, h as f32 / 200.0 * 25.4);
    Ok(Imported {
        page_mm: page,
        paths: Vec::new(),
        notes: vec![format!("PDF rasterizado a 200 DPI: {w}×{h} px")],
        art: Some(Raster {
            rgba,
            width: w,
            height: h,
            rect_mm: (0.0, 0.0, page.0, page.1),
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "pdf".to_string()),
        }),
    })
}

fn fit_to_page(page: (f32, f32), w: u32, h: u32, margin: f32) -> (f32, f32, f32) {
    let mm_per_px = 25.4 / 300.0;
    let nat_w = w as f32 * mm_per_px;
    let nat_h = h as f32 * mm_per_px;
    let max_w = (page.0 - margin * 2.0).max(1.0);
    let max_h = (page.1 - margin * 2.0).max(1.0);
    let factor = (max_w / nat_w).min(max_h / nat_h).min(1.0);
    let scale = mm_per_px * factor;
    let x = (page.0 - w as f32 * scale) * 0.5;
    let y = (page.1 - h as f32 * scale) * 0.5;
    (scale, x, y)
}

fn decode_image(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    let image = image::load_from_memory(bytes).map_err(|e| format!("imagem inválida: {e}"))?;
    let mut rgba = image.to_rgba8();
    composite_white(&mut rgba);
    let (w, h) = rgba.dimensions();
    Ok((rgba.into_raw(), w, h))
}

fn composite_white(rgba: &mut [u8]) {
    for pixel in rgba.chunks_exact_mut(4) {
        let a = u32::from(pixel[3]);
        if a == 255 {
            continue;
        }
        let inv = 255 - a;
        for c in &mut pixel[..3] {
            *c = ((u32::from(*c) * a + u32::from(255u8) * inv) / 255) as u8;
        }
        pixel[3] = 255;
    }
}

fn render_svg(tree: &usvg::Tree, natural_mm: (f32, f32)) -> Result<(Vec<u8>, u32, u32), String> {
    let size = tree.size();
    let mut scale = 300.0 / 96.0;
    let max_dim = 4200.0;
    let projected = size.width().max(size.height()) * scale;
    if projected > max_dim {
        scale *= max_dim / projected;
    }
    let w = (size.width() * scale).round().max(1.0) as u32;
    let h = (size.height() * scale).round().max(1.0) as u32;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h)
        .ok_or_else(|| "SVG demasiado grande para rasterizar".to_string())?;
    pixmap.fill(resvg::tiny_skia::Color::WHITE);
    resvg::render(
        tree,
        resvg::tiny_skia::Transform::from_scale(
            w as f32 / natural_mm.0 * MM_PER_PX,
            h as f32 / natural_mm.1 * MM_PER_PX,
        ),
        &mut pixmap.as_mut(),
    );
    Ok((pixmap.data().to_vec(), w, h))
}

struct Candidate {
    path: CorePath,
    stroked: bool,
    filled: bool,
    background: bool,
}

fn collect_svg_paths(group: &usvg::Group, out: &mut Vec<Candidate>, page_w: f32, page_h: f32) {
    for node in group.children() {
        match node {
            usvg::Node::Group(g) => collect_svg_paths(g, out, page_w, page_h),
            usvg::Node::Path(p) if p.is_visible() => {
                let stroked = p.stroke().is_some();
                let filled = p.fill().is_some();
                let white = p
                    .fill()
                    .map(|f| match f.paint() {
                        usvg::Paint::Color(c) => c.red > 240 && c.green > 240 && c.blue > 240,
                        _ => false,
                    })
                    .unwrap_or(false);
                let bb = p.abs_bounding_box();
                let background = white && bb.width() >= page_w * 0.9 && bb.height() >= page_h * 0.9;
                for (points, closed) in flatten_path(p) {
                    if points.len() < 2 {
                        continue;
                    }
                    let kind = if points.len() == 2 {
                        PathKind::Line
                    } else {
                        PathKind::Curve
                    };
                    out.push(Candidate {
                        path: CorePath {
                            kind,
                            points,
                            closed,
                            transform: None,
                        },
                        stroked,
                        filled,
                        background,
                    });
                }
            }
            _ => {}
        }
    }
}

fn svg_all_paths(group: &usvg::Group, page_w: f32, page_h: f32) -> Vec<CorePath> {
    let mut out = Vec::new();
    collect_svg_paths(group, &mut out, page_w, page_h);
    out.into_iter().map(|c| c.path).collect()
}

fn flatten_path(path: &usvg::Path) -> Vec<(Vec<Point>, bool)> {
    let Some(transformed) = path.data().clone().transform(path.abs_transform()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut current: Vec<Point> = Vec::new();
    let (mut cx, mut cy) = (0.0f32, 0.0f32);
    let tol = 0.25f32;

    for segment in transformed.segments() {
        match segment {
            resvg::tiny_skia::PathSegment::MoveTo(pt) => {
                flush(&mut out, &mut current, false);
                cx = pt.x;
                cy = pt.y;
                current.push(mm(pt.x, pt.y));
            }
            resvg::tiny_skia::PathSegment::LineTo(pt) => {
                cx = pt.x;
                cy = pt.y;
                current.push(mm(pt.x, pt.y));
            }
            resvg::tiny_skia::PathSegment::QuadTo(c, pt) => {
                let n = subdivisions(tol, &[(cx, cy), (c.x, c.y), (pt.x, pt.y)]);
                for i in 1..=n {
                    let t = i as f32 / n as f32;
                    let u = 1.0 - t;
                    let x = u * u * cx + 2.0 * u * t * c.x + t * t * pt.x;
                    let y = u * u * cy + 2.0 * u * t * c.y + t * t * pt.y;
                    current.push(mm(x, y));
                }
                cx = pt.x;
                cy = pt.y;
            }
            resvg::tiny_skia::PathSegment::CubicTo(c1, c2, pt) => {
                let n = subdivisions(tol, &[(cx, cy), (c1.x, c1.y), (c2.x, c2.y), (pt.x, pt.y)]);
                for i in 1..=n {
                    let t = i as f32 / n as f32;
                    let u = 1.0 - t;
                    let x = u * u * u * cx
                        + 3.0 * u * u * t * c1.x
                        + 3.0 * u * t * t * c2.x
                        + t * t * t * pt.x;
                    let y = u * u * u * cy
                        + 3.0 * u * u * t * c1.y
                        + 3.0 * u * t * t * c2.y
                        + t * t * t * pt.y;
                    current.push(mm(x, y));
                }
                cx = pt.x;
                cy = pt.y;
            }
            resvg::tiny_skia::PathSegment::Close => {
                flush(&mut out, &mut current, true);
            }
        }
    }
    flush(&mut out, &mut current, false);
    out
}

fn subdivisions(tol: f32, pts: &[(f32, f32)]) -> u32 {
    let mut len = 0.0;
    for pair in pts.windows(2) {
        len += ((pair[1].0 - pair[0].0).powi(2) + (pair[1].1 - pair[0].1).powi(2)).sqrt();
    }
    ((len / tol).sqrt().ceil() as u32).clamp(2, 96)
}

fn flush(out: &mut Vec<(Vec<Point>, bool)>, current: &mut Vec<Point>, closed: bool) {
    dedup(current);
    if current.len() >= 2 {
        out.push((std::mem::take(current), closed));
    } else {
        current.clear();
    }
}

fn dedup(points: &mut Vec<Point>) {
    points.dedup_by(|a, b| (a.x_mm - b.x_mm).abs() < 1e-4 && (a.y_mm - b.y_mm).abs() < 1e-4);
}

fn mm(x: f32, y: f32) -> Point {
    Point::new(x * MM_PER_PX, y * MM_PER_PX)
}

pub fn adjust_rgba(
    src: &[u8],
    hue: f32,
    saturation: f32,
    vibrance: f32,
    contrast: f32,
    brightness: f32,
) -> Vec<u8> {
    let mut out = src.to_vec();
    let contrast = contrast.clamp(0.0, 4.0);
    let brightness = brightness.clamp(-1.0, 1.0);
    if hue.abs() < 0.01
        && (saturation - 1.0).abs() < 0.001
        && vibrance.abs() < 0.001
        && (contrast - 1.0).abs() < 0.001
        && brightness.abs() < 0.001
    {
        return out;
    }
    let hue = hue / 360.0;
    let shift = brightness * 255.0;
    for pixel in out.chunks_exact_mut(4) {
        let (h, s, v) = rgb_to_hsv(pixel[0], pixel[1], pixel[2]);
        let mut h = (h + hue).rem_euclid(1.0);
        let mut s = (s * saturation).clamp(0.0, 1.0);
        s = (s * (1.0 + vibrance * (1.0 - s))).clamp(0.0, 1.0);
        if v <= f32::EPSILON {
            s = 0.0;
            h = 0.0;
        }
        let (r, g, b) = hsv_to_rgb(h, s, v);
        let apply = |c: u8| -> u8 {
            (((f32::from(c) - 128.0) * contrast + 128.0 + shift).clamp(0.0, 255.0)).round() as u8
        };
        pixel[0] = apply(r);
        pixel[1] = apply(g);
        pixel[2] = apply(b);
    }
    out
}

fn rgb_to_hsv(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let r = f32::from(r) / 255.0;
    let g = f32::from(g) / 255.0;
    let b = f32::from(b) / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let mut h = if delta.abs() < 1e-6 {
        0.0
    } else if max == r {
        ((g - b) / delta).rem_euclid(6.0)
    } else if max == g {
        (b - r) / delta + 2.0
    } else {
        (r - g) / delta + 4.0
    };
    h /= 6.0;
    let s = if max <= f32::EPSILON {
        0.0
    } else {
        delta / max
    };
    (h, s, max)
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let h6 = (h.rem_euclid(1.0)) * 6.0;
    let i = h6.floor() as i32;
    let f = h6 - i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    let (r, g, b) = match i.rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    (
        (r * 255.0).round().clamp(0.0, 255.0) as u8,
        (g * 255.0).round().clamp(0.0, 255.0) as u8,
        (b * 255.0).round().clamp(0.0, 255.0) as u8,
    )
}

pub fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let image = image::RgbaImage::from_raw(width, height, rgba.to_vec())
        .ok_or_else(|| "tamanho de imagem inválido".to_string())?;
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .map_err(|e| format!("PNG: {e}"))?;
    Ok(out)
}

pub fn downscale(rgba: &[u8], w: u32, h: u32, max_dim: u32) -> (Vec<u8>, u32, u32) {
    if w <= max_dim && h <= max_dim {
        return (rgba.to_vec(), w, h);
    }
    let factor = (w.max(h) as f32 / max_dim as f32).ceil() as u32;
    let dw = (w / factor).max(1);
    let dh = (h / factor).max(1);
    // Box-average instead of point sampling: averaging each block keeps edges
    // stable, so the trace threshold does not flicker with single-pixel noise.
    let mut out = Vec::with_capacity((dw * dh * 4) as usize);
    for y in 0..dh {
        let y0 = (y * h / dh) as usize;
        let y1 = (((y + 1) * h / dh) as usize).max(y0 + 1).min(h as usize);
        for x in 0..dw {
            let x0 = (x * w / dw) as usize;
            let x1 = (((x + 1) * w / dw) as usize).max(x0 + 1).min(w as usize);
            let (mut r, mut g, mut b, mut a) = (0u32, 0u32, 0u32, 0u32);
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let o = ((sy * w as usize + sx) * 4) as usize;
                    r += rgba[o] as u32;
                    g += rgba[o + 1] as u32;
                    b += rgba[o + 2] as u32;
                    a += rgba[o + 3] as u32;
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            out.push((r / n) as u8);
            out.push((g / n) as u8);
            out.push((b / n) as u8);
            out.push((a / n) as u8);
        }
    }
    (out, dw, dh)
}

pub type ArtPlacement = (f32, f32, f32, f32);

pub fn project_svg(
    page: (f32, f32),
    paths: &[CorePath],
    art: Option<(&[u8], ArtPlacement)>,
) -> String {
    let mut svg = String::new();
    svg.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    svg.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" \
         width=\"{:.4}mm\" height=\"{:.4}mm\" viewBox=\"0 0 {:.4} {:.4}\">\n",
        page.0, page.1, page.0, page.1
    ));
    if let Some((png, (x, y, w, h))) = art {
        let encoded = base64::engine::general_purpose::STANDARD.encode(png);
        svg.push_str(&format!(
            "  <image x=\"{x:.4}\" y=\"{y:.4}\" width=\"{w:.4}\" height=\"{h:.4}\" \
             preserveAspectRatio=\"none\" href=\"data:image/png;base64,{encoded}\"/>\n"
        ));
    }
    svg.push_str("  <g fill=\"none\" stroke=\"#111\" stroke-width=\"0.2\" data-cut=\"1\">\n");
    for path in paths {
        svg.push_str(&format!("    <path d=\"{}\"/>\n", path.to_svg_d()));
    }
    svg.push_str("  </g>\n</svg>\n");
    svg
}

pub struct FontEntry {
    pub name: String,
    pub bytes: Vec<u8>,
}

pub fn fonts_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".local/share/silhouette-studio/fonts")
}

pub fn load_fonts() -> Vec<FontEntry> {
    let mut fonts = Vec::new();
    if let Some(default) = default_font() {
        fonts.push(default);
    }
    let dir = fonts_dir();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            if ext != "ttf" && ext != "otf" {
                continue;
            }
            if let Ok(bytes) = std::fs::read(&path) {
                let name = face_name(&bytes)
                    .or_else(|| path.file_stem().map(|s| s.to_string_lossy().to_string()))
                    .unwrap_or_else(|| "fonte".to_string());
                if !fonts.iter().any(|f| f.name == name) {
                    fonts.push(FontEntry { name, bytes });
                }
            }
        }
    }
    fonts
}

fn default_font() -> Option<FontEntry> {
    for path in [
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            return Some(FontEntry {
                name: face_name(&bytes).unwrap_or_else(|| "DejaVu Sans".to_string()),
                bytes,
            });
        }
    }
    None
}

pub fn install_font(src: &FsPath) -> Result<FontEntry, String> {
    let bytes = std::fs::read(src).map_err(|e| format!("{}: {e}", src.display()))?;
    let name = face_name(&bytes)
        .or_else(|| src.file_stem().map(|s| s.to_string_lossy().to_string()))
        .unwrap_or_else(|| "fonte".to_string());
    let dir = fonts_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let ext = src
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_else(|| "ttf".to_string());
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let dest = dir.join(format!("{safe}.{ext}"));
    std::fs::write(&dest, &bytes).map_err(|e| format!("{}: {e}", dest.display()))?;
    Ok(FontEntry { name, bytes })
}

pub fn face_name(bytes: &[u8]) -> Option<String> {
    let face = ttf_parser::Face::parse(bytes, 0).ok()?;
    for entry in face.names() {
        if entry.name_id == 1 {
            if let Some(name) = entry.to_string() {
                return Some(name);
            }
        }
    }
    None
}

pub fn register_ui_font(ctx: &egui::Context, fonts: &[FontEntry]) {
    if fonts.is_empty() {
        return;
    }
    let mut defs = egui::FontDefinitions::default();
    for (i, font) in fonts.iter().enumerate() {
        let key = format!("custom-{i}");
        defs.font_data
            .insert(key.clone(), egui::FontData::from_owned(font.bytes.clone()));
        defs.families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .push(key.clone());
        defs.families
            .entry(egui::FontFamily::Monospace)
            .or_default()
            .push(key.clone());
        // Each font also gets its own family, so the font list can show every
        // name rendered in the font itself (live preview, Canva-style).
        defs.families.insert(
            egui::FontFamily::Name(format!("font-{i}").into()),
            vec![key],
        );
    }
    ctx.set_fonts(defs);
}

pub fn text_paths(
    font: &FontEntry,
    text: &str,
    size_mm: f32,
    origin: (f32, f32),
) -> Result<Vec<CorePath>, String> {
    let face =
        ttf_parser::Face::parse(&font.bytes, 0).map_err(|e| format!("fonte inválida: {e}"))?;
    let units = f32::from(face.units_per_em());
    let scale = size_mm / units;
    let line_height = f32::from(face.ascender() - face.descender() + face.line_gap()) * scale;
    let ascender = f32::from(face.ascender()) * scale;
    let mut pen_x = origin.0;
    let mut baseline = origin.1 + ascender;
    let mut out = Vec::new();

    for ch in text.chars() {
        if ch == '\n' {
            pen_x = origin.0;
            baseline += line_height;
            continue;
        }
        let Some(gid) = face.glyph_index(ch) else {
            continue;
        };
        let mut outline = Outline::default();
        if face.outline_glyph(gid, &mut outline).is_some() {
            for contour in outline.contours {
                if contour.len() < 2 {
                    continue;
                }
                let points = contour
                    .iter()
                    .map(|(x, y)| Point::new(pen_x + x * scale, baseline - y * scale))
                    .collect();
                out.push(CorePath {
                    kind: PathKind::Curve,
                    points,
                    closed: true,
                    transform: None,
                });
            }
        }
        pen_x += f32::from(face.glyph_hor_advance(gid).unwrap_or(0)) * scale;
    }
    Ok(out)
}

#[derive(Default)]
struct Outline {
    contours: Vec<Vec<(f32, f32)>>,
    current: Vec<(f32, f32)>,
    start: (f32, f32),
}

impl Outline {
    fn flush(&mut self) {
        if self.current.len() >= 2 {
            self.contours.push(std::mem::take(&mut self.current));
        } else {
            self.current.clear();
        }
    }

    fn curve(&mut self, from: (f32, f32), c1: (f32, f32), c2: (f32, f32), to: (f32, f32)) {
        let n = subdivisions(0.05, &[from, c1, c2, to]);
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            self.current.push((
                u * u * u * from.0
                    + 3.0 * u * u * t * c1.0
                    + 3.0 * u * t * t * c2.0
                    + t * t * t * to.0,
                u * u * u * from.1
                    + 3.0 * u * u * t * c1.1
                    + 3.0 * u * t * t * c2.1
                    + t * t * t * to.1,
            ));
        }
    }
}

impl ttf_parser::OutlineBuilder for Outline {
    fn move_to(&mut self, x: f32, y: f32) {
        self.flush();
        self.start = (x, y);
        self.current.push((x, y));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.current.push((x, y));
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let from = *self.current.last().unwrap_or(&self.start);
        let c = (x1, y1);
        let to = (x, y);
        let mid1 = ((from.0 + 2.0 * c.0) / 3.0, (from.1 + 2.0 * c.1) / 3.0);
        let mid2 = ((to.0 + 2.0 * c.0) / 3.0, (to.1 + 2.0 * c.1) / 3.0);
        self.curve(from, mid1, mid2, to);
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let from = *self.current.last().unwrap_or(&self.start);
        self.curve(from, (x1, y1), (x2, y2), (x, y));
    }

    fn close(&mut self) {
        self.flush();
    }
}
