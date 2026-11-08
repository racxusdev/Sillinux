//! Registration-mark geometry for print & cut.
//!
//! # Evidence
//!
//! * The mark layout is the one used by Silhouette Studio and re-implemented by
//!   the public `render_silhouette_regmarks.py` (inkscape-silhouette, GPL):
//!   a **filled square** at the top-left corner of the mark rectangle and
//!   **L-shaped** marks at the remaining corners, with the arms pointing
//!   inwards.
//! * Silhouette Studio 5's Registration Marks panel (manual p.56) lets the
//!   user set **length**, **thickness** and **margin**. The arm length and
//!   stroke are what the machine is told with `TB51` / `TB53` before a scan.
//! * This project's layout follows the public reference renderer: a 5 mm
//!   filled square at the top-left, 20 mm L arms at the remaining corners
//!   (three Ls, `FourCorner`) and a 0.3 mm stroke. Those are also Silhouette
//!   Studio's defaults (`TB51,400 TB53,6`) and the geometry its sensor is
//!   calibrated for.
//!
//! Everything here is **geometry only**; nothing has been scanned on the Cameo
//! 5 yet. The defaults are UNVERIFIED until a printed sheet is read.

/// Which corner marks to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RegMarkStyle {
    /// A filled square at the top-left plus an L at the top-right and at the
    /// bottom-left (classic three-point marks, `TB123`).
    Standard,
    /// A filled square at the top-left plus an L at every other corner.
    /// This is the default layout. The scan still uses the three documented
    /// reference marks (`TB123`); the fourth L is extra ink.
    #[default]
    FourCorner,
}

/// Registration-mark layout in page millimetres (origin top-left, y down).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RegMarkSpec {
    pub page_width_mm: f32,
    pub page_height_mm: f32,
    /// Distance from the page edge to the marks' outer corner.
    pub inset_mm: f32,
    /// Length of each arm of the L marks.
    pub arm_mm: f32,
    /// Side of the filled square (top-left mark).
    pub square_mm: f32,
    /// Printed stroke width of the L marks; also sent to the machine (`TB53`).
    pub stroke_mm: f32,
    /// Search starts this many mm before the top mark (default 10 mm).
    pub search_offset_top_mm: f32,
    /// Search starts this many mm before the left mark (default 10 mm).
    pub search_offset_left_mm: f32,
    /// Step size for segmented marks (long media), mm.
    pub step_size_mm: f32,
    /// Step direction for segmented marks (0 = vertical, 1 = horizontal).
    pub step_direction: i32,
    pub style: RegMarkStyle,
}

impl Default for RegMarkSpec {
    fn default() -> Self {
        Self::for_page(210.0, 297.0)
    }
}

impl RegMarkSpec {
    /// A4 portrait with the defaults above.
    #[must_use]
    pub fn a4() -> Self {
        Self::for_page(210.0, 297.0)
    }

    /// Mark layout for any page size, with the default inset/arm/square/stroke.
    #[must_use]
    pub fn for_page(page_width_mm: f32, page_height_mm: f32) -> Self {
        Self {
            page_width_mm,
            page_height_mm,
            inset_mm: 15.875,
            arm_mm: 20.0,
            square_mm: 10.0,
            stroke_mm: 0.5,
            search_offset_top_mm: 10.0,
            search_offset_left_mm: 10.0,
            step_size_mm: 0.0,
            step_direction: 0,
            style: RegMarkStyle::FourCorner,
        }
    }

    /// Distance between the left and right marks (outer corners).
    #[must_use]
    pub fn mark_width_mm(&self) -> f32 {
        self.page_width_mm - 2.0 * self.inset_mm
    }

    /// Distance between the top and bottom marks (outer corners).
    #[must_use]
    pub fn mark_height_mm(&self) -> f32 {
        self.page_height_mm - 2.0 * self.inset_mm
    }

    /// `TB123` search start (top): search_offset_top_mm before the marks, clamped at the page edge.
    #[must_use]
    pub fn scan_margin_top_mm(&self) -> f32 {
        (self.inset_mm - self.search_offset_top_mm).max(0.0)
    }

    /// `TB123` search start (left): search_offset_left_mm before the marks, clamped at the page edge.
    #[must_use]
    pub fn scan_margin_left_mm(&self) -> f32 {
        (self.inset_mm - self.search_offset_left_mm).max(0.0)
    }
}

/// One mark, as a polyline in page millimetres.
#[derive(Debug, Clone, PartialEq)]
pub struct RegMarkShape {
    pub points: Vec<(f32, f32)>,
    /// Closed shapes are filled; open shapes are stroked.
    pub closed: bool,
    pub filled: bool,
}

impl RegMarkShape {
    /// SVG path data (`M` / `L` / `Z`).
    #[must_use]
    pub fn to_svg_d(&self) -> String {
        let mut d = String::new();
        for (i, (x, y)) in self.points.iter().enumerate() {
            if i == 0 {
                d.push_str(&format!("M {x:.4} {y:.4}"));
            } else {
                d.push_str(&format!(" L {x:.4} {y:.4}"));
            }
        }
        if self.closed {
            d.push_str(" Z");
        }
        d
    }
}

/// Build the mark shapes for a page.
///
/// The top-left square and every L sit **inside** the mark rectangle; their
/// outer corners define the rectangle used by the scan (`TB123`).
#[must_use]
pub fn regmark_shapes(spec: &RegMarkSpec) -> Vec<RegMarkShape> {
    let left = spec.inset_mm;
    let top = spec.inset_mm;
    let right = spec.page_width_mm - spec.inset_mm;
    let bottom = spec.page_height_mm - spec.inset_mm;
    let arm = spec.arm_mm;
    let square = spec.square_mm;

    let mut shapes = vec![RegMarkShape {
        points: vec![
            (left, top),
            (left + square, top),
            (left + square, top + square),
            (left, top + square),
        ],
        closed: true,
        filled: true,
    }];

    // Arms point inwards from each outer corner.
    shapes.push(l_mark(right, top, -arm, arm));
    shapes.push(l_mark(left, bottom, arm, -arm));
    if spec.style == RegMarkStyle::FourCorner {
        shapes.push(l_mark(right, bottom, -arm, -arm));
    }
    shapes
}

fn l_mark(corner_x: f32, corner_y: f32, dx: f32, dy: f32) -> RegMarkShape {
    RegMarkShape {
        points: vec![
            (corner_x + dx, corner_y),
            (corner_x, corner_y),
            (corner_x, corner_y + dy),
        ],
        closed: false,
        filled: false,
    }
}

/// Printable SVG (millimetres) with a white background and black marks.
#[must_use]
pub fn regmarks_svg(spec: &RegMarkSpec) -> String {
    let w = spec.page_width_mm;
    let h = spec.page_height_mm;
    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}mm\" height=\"{h}mm\" \
         viewBox=\"0 0 {w} {h}\">\n"
    );
    svg.push_str(&format!(
        "  <rect x=\"0\" y=\"0\" width=\"{w}\" height=\"{h}\" fill=\"#fff\"/>\n"
    ));
    for shape in regmark_shapes(spec) {
        let d = shape.to_svg_d();
        if shape.filled {
            svg.push_str(&format!(
                "  <path d=\"{d}\" fill=\"#000\" stroke=\"none\"/>\n"
            ));
        } else {
            svg.push_str(&format!(
                "  <path d=\"{d}\" fill=\"none\" stroke=\"#000\" stroke-width=\"{}\" \
                 stroke-linecap=\"square\"/>\n",
                spec.stroke_mm
            ));
        }
    }
    svg.push_str("</svg>\n");
    svg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a4_has_square_and_three_ls() {
        let shapes = regmark_shapes(&RegMarkSpec::a4());
        assert_eq!(shapes.len(), 4);
        let square = &shapes[0];
        assert!(square.filled && square.closed);
        assert_eq!(
            square.points,
            vec![
                (15.875, 15.875),
                (25.875, 15.875),
                (25.875, 25.875),
                (15.875, 25.875)
            ]
        );
        assert_eq!(
            shapes[1].points,
            vec![(174.125, 15.875), (194.125, 15.875), (194.125, 35.875)]
        );
        assert_eq!(
            shapes[2].points,
            vec![(35.875, 281.125), (15.875, 281.125), (15.875, 261.125)]
        );
        assert_eq!(
            shapes[3].points,
            vec![(174.125, 281.125), (194.125, 281.125), (194.125, 261.125)]
        );
    }

    #[test]
    fn standard_style_has_two_ls() {
        let mut spec = RegMarkSpec::a4();
        spec.style = RegMarkStyle::Standard;
        let shapes = regmark_shapes(&spec);
        assert_eq!(shapes.len(), 3);
    }

    #[test]
    fn scan_parameters_follow_the_mark_rectangle() {
        let spec = RegMarkSpec::a4();
        assert_eq!(spec.mark_width_mm(), 178.25);
        assert_eq!(spec.mark_height_mm(), 265.25);
        assert_eq!(spec.scan_margin_top_mm(), 5.875);
        assert_eq!(spec.scan_margin_left_mm(), 5.875);

        let custom = RegMarkSpec {
            inset_mm: 20.0,
            search_offset_top_mm: 15.0,
            search_offset_left_mm: 12.0,
            ..RegMarkSpec::a4()
        };
        assert_eq!(custom.mark_width_mm(), 170.0);
        assert_eq!(custom.scan_margin_top_mm(), 5.0);
        assert_eq!(custom.scan_margin_left_mm(), 8.0);
    }

    #[test]
    fn svg_has_page_size_and_both_styles() {
        let svg = regmarks_svg(&RegMarkSpec::a4());
        assert!(svg.contains("width=\"210mm\" height=\"297mm\""));
        assert!(svg.contains("fill=\"#000\""));
        assert!(svg.contains("stroke-width=\"0.5\""));
        assert_eq!(svg.matches("<path").count(), 4);
    }
}
