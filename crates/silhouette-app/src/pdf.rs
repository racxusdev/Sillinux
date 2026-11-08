//! Printable sheet: registration marks + optional artwork / vector paths.
//!
//! The output is a **PDF with an exact millimetre page size**, so CUPS prints
//! it at 100 % (`print-scaling=none`). The artwork is decoded from the PNG
//! embedded in the `.studio3`, composited over white and embedded as a
//! `/FlateDecode` RGB image using the PNG predictor (`Predictor 15`).
//!
//! This module deliberately uses no rasteriser: marks and cut paths are PDF
//! vector operators, and the artwork is an image XObject. That keeps the output
//! sharp at any printer resolution and avoids a 300 DPI intermediate bitmap.

use std::io::{Cursor, Write};

use flate2::write::ZlibEncoder;
use flate2::Compression;
use silhouette_core::{regmark_shapes, Path, RegMarkSpec};

/// PDF points per millimetre (1 pt = 1/72 in).
const PT_PER_MM: f32 = 72.0 / 25.4;

/// One raster to place on the sheet, in page millimetres (top-left origin).
pub struct SheetImage<'a> {
    /// Human-readable label used in error messages.
    pub label: String,
    /// Encoded PNG bytes.
    pub png: &'a [u8],
    pub x_mm: f32,
    pub y_mm: f32,
    pub width_mm: f32,
    pub height_mm: f32,
}

/// Everything that goes on the printed page.
pub struct Sheet<'a> {
    pub page_width_mm: f32,
    pub page_height_mm: f32,
    pub marks: RegMarkSpec,
    /// Draw the registration marks.
    pub draw_marks: bool,
    /// Decoded vector paths (drawn as thin black lines when non-empty).
    pub paths: &'a [Path],
    /// Raster layers, bottom-first. Empty for vector-only sheets.
    pub images: &'a [SheetImage<'a>],
}

/// Render the sheet as a single-page PDF.
pub fn render(sheet: &Sheet<'_>) -> Result<Vec<u8>, String> {
    if !(sheet.page_width_mm.is_finite()
        && sheet.page_height_mm.is_finite()
        && sheet.page_width_mm > 0.0
        && sheet.page_height_mm > 0.0)
    {
        return Err("invalid page size".to_string());
    }

    let page_w_pt = sheet.page_width_mm * PT_PER_MM;
    let page_h_pt = sheet.page_height_mm * PT_PER_MM;

    let mut decoded = Vec::with_capacity(sheet.images.len());
    for img in sheet.images {
        decoded.push(decode_png(&img.label, img.png)?);
    }

    let mut content = String::new();
    content.push_str("q\n1 1 1 rg\n");
    content.push_str(&format!("0 0 {page_w_pt:.4} {page_h_pt:.4} re f\nQ\n"));

    for (i, img) in sheet.images.iter().enumerate() {
        let x = img.x_mm * PT_PER_MM;
        // PDF's origin is bottom-left; the document's is top-left.
        let y = (sheet.page_height_mm - img.y_mm - img.height_mm) * PT_PER_MM;
        let w = img.width_mm * PT_PER_MM;
        let h = img.height_mm * PT_PER_MM;
        content.push_str("q\n");
        content.push_str(&format!("{w:.4} 0 0 {h:.4} {x:.4} {y:.4} cm\n"));
        content.push_str(&format!("/Im{i} Do\nQ\n"));
    }

    if sheet.draw_marks {
        let width_pt = sheet.marks.stroke_mm * PT_PER_MM;
        for shape in regmark_shapes(&sheet.marks) {
            if shape.filled {
                content.push_str("0 0 0 rg\n");
            } else {
                content.push_str("0 0 0 RG\n");
                // 2 J = projecting square cap, matching the SVG's square caps.
                content.push_str(&format!("{width_pt:.4} w\n2 J\n0 j\n"));
            }
            for (i, (x_mm, y_mm)) in shape.points.iter().enumerate() {
                let x = x_mm * PT_PER_MM;
                let y = (sheet.page_height_mm - y_mm) * PT_PER_MM;
                let op = if i == 0 { 'm' } else { 'l' };
                content.push_str(&format!("{x:.4} {y:.4} {op}\n"));
            }
            if shape.closed {
                content.push_str("h\n");
            }
            content.push_str(if shape.filled { "f\n" } else { "S\n" });
        }
    }

    if !sheet.paths.is_empty() {
        content.push_str("0 0 0 RG\n");
        content.push_str(&format!("{:.4} w\n", 0.2 * PT_PER_MM));
        for path in sheet.paths {
            for (i, p) in path.points.iter().enumerate() {
                let x = p.x_mm * PT_PER_MM;
                let y = (sheet.page_height_mm - p.y_mm) * PT_PER_MM;
                let op = if i == 0 { 'm' } else { 'l' };
                content.push_str(&format!("{x:.4} {y:.4} {op}\n"));
            }
            if path.closed {
                content.push_str("h\n");
            }
            content.push_str("S\n");
        }
    }

    // Fixed object layout: 1 catalog, 2 pages, 3 page, 4 contents, then one
    // image XObject per raster (5, 6, ...).
    let resources = if decoded.is_empty() {
        "<< /ProcSet [/PDF] >>".to_string()
    } else {
        let mut xobjects = String::new();
        for i in 0..decoded.len() {
            xobjects.push_str(&format!("/Im{i} {} 0 R ", i + 5));
        }
        format!("<< /XObject << {xobjects}>> /ProcSet [/PDF /ImageC] >>")
    };

    let mut objects: Vec<Vec<u8>> = Vec::new();
    objects.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
    objects.push(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec());
    objects.push(
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {page_w_pt:.4} {page_h_pt:.4}] \
             /Resources {resources} /Contents 4 0 R >>"
        )
        .into_bytes(),
    );
    objects.push(stream_object(content.as_bytes()));
    for pix in &decoded {
        objects.push(image_object(pix)?);
    }

    write_pdf(&objects)
}

struct DecodedImage {
    width: u32,
    height: u32,
    /// zlib-compressed, filter-0 RGB scanlines (PNG predictor rows).
    compressed: Vec<u8>,
}

fn decode_png(label: &str, png: &[u8]) -> Result<DecodedImage, String> {
    let mut decoder = png::Decoder::new(Cursor::new(png));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("{label}: PNG header: {e}"))?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("{label}: PNG data: {e}"))?;

    if info.bit_depth != png::BitDepth::Eight {
        return Err(format!(
            "{label}: unsupported PNG bit depth {:?}",
            info.bit_depth
        ));
    }

    let width = info.width as usize;
    let height = info.height as usize;
    let stride = info.line_size;
    if width == 0 || height == 0 || stride == 0 {
        return Err(format!("{label}: empty PNG"));
    }
    let data = &buf[..info.buffer_size()];

    let channels = match info.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => {
            return Err(format!("{label}: indexed PNG was not expanded"));
        }
    };

    let mut filtered = Vec::with_capacity((width * 3 + 1) * height);
    for row in data.chunks(stride).take(height) {
        filtered.push(0); // PNG filter type: None
        for x in 0..width {
            let p = &row[x * channels..x * channels + channels];
            let (r, g, b, a) = match channels {
                1 => (p[0], p[0], p[0], 255),
                2 => (p[0], p[0], p[0], p[1]),
                3 => (p[0], p[1], p[2], 255),
                _ => (p[0], p[1], p[2], p[3]),
            };
            // Composite over white; the PDF image has no alpha channel.
            let a = u32::from(a);
            let mix = |v: u8| -> u8 { ((u32::from(v) * a + 255 * (255 - a)) / 255) as u8 };
            filtered.push(mix(r));
            filtered.push(mix(g));
            filtered.push(mix(b));
        }
    }

    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(&filtered)
        .map_err(|e| format!("{label}: compress: {e}"))?;
    let compressed = enc
        .finish()
        .map_err(|e| format!("{label}: compress: {e}"))?;

    Ok(DecodedImage {
        width: width as u32,
        height: height as u32,
        compressed,
    })
}

fn stream_object(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 64);
    out.extend_from_slice(format!("<< /Length {} >>\nstream\n", data.len()).as_bytes());
    out.extend_from_slice(data);
    out.extend_from_slice(b"\nendstream");
    out
}

fn image_object(pix: &DecodedImage) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(pix.compressed.len() + 256);
    out.extend_from_slice(
        format!(
            "<< /Type /XObject /Subtype /Image /Width {} /Height {} \
             /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode \
             /DecodeParms << /Predictor 15 /Colors 3 /BitsPerComponent 8 /Columns {} >> \
             /Length {} >>\nstream\n",
            pix.width,
            pix.height,
            pix.width,
            pix.compressed.len()
        )
        .as_bytes(),
    );
    out.extend_from_slice(&pix.compressed);
    out.extend_from_slice(b"\nendstream");
    Ok(out)
}

fn write_pdf(objects: &[Vec<u8>]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n");

    let mut offsets = Vec::with_capacity(objects.len());
    for (i, obj) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(obj);
        out.extend_from_slice(b"\nendobj\n");
    }

    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            objects.len() + 1,
            xref
        )
        .as_bytes(),
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use silhouette_core::RegMarkSpec;

    #[test]
    fn marks_only_pdf_has_an_a4_page_and_three_marks() {
        let paths: &[Path] = &[];
        let images: &[SheetImage<'_>] = &[];
        let sheet = Sheet {
            page_width_mm: 210.0,
            page_height_mm: 297.0,
            marks: RegMarkSpec::a4(),
            draw_marks: true,
            paths,
            images,
        };
        let pdf = render(&sheet).unwrap();
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.starts_with("%PDF-1.4"));
        assert!(text.contains("/MediaBox [0 0 595.2756 841.8898]"));
        assert!(!text.contains("/Im0"));
        // The square is a filled path, the two Ls are strokes.
        assert!(text.contains("h\nf\n"));
        assert!(text.contains("\nS\n"));
        assert!(text.ends_with("%%EOF\n"));
    }

    #[test]
    fn image_is_embedded_as_an_xobject() {
        // 2x2 RGB PNG, encoded in memory.
        let mut raw = Vec::new();
        for color in [[255u8, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 255]] {
            raw.extend_from_slice(&color);
        }
        let mut png = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut png, 2, 2);
            enc.set_color(png::ColorType::Rgb);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().unwrap();
            writer.write_image_data(&raw).unwrap();
        }
        let images = [SheetImage {
            label: "test".to_string(),
            png: &png,
            x_mm: 10.0,
            y_mm: 10.0,
            width_mm: 20.0,
            height_mm: 20.0,
        }];
        let sheet = Sheet {
            page_width_mm: 210.0,
            page_height_mm: 297.0,
            marks: RegMarkSpec::a4(),
            draw_marks: false,
            paths: &[],
            images: &images,
        };
        let pdf = render(&sheet).unwrap();
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.contains("/XObject << /Im0 5 0 R >>"));
        assert!(text.contains("/Im0 Do"));
        assert!(text.contains("/Subtype /Image"));
        assert!(text.contains("/Width 2 /Height 2"));
    }

    #[test]
    fn no_marks_no_paths_is_a_blank_page() {
        let paths: &[Path] = &[];
        let images: &[SheetImage<'_>] = &[];
        let sheet = Sheet {
            page_width_mm: 210.0,
            page_height_mm: 297.0,
            marks: RegMarkSpec::a4(),
            draw_marks: false,
            paths,
            images,
        };
        let pdf = render(&sheet).unwrap();
        assert!(!String::from_utf8_lossy(&pdf).contains("/Im0"));
    }
}
