//! A GPGL job: an ordered list of commands, one per USB transfer.

use crate::command;

/// An ordered list of already-encoded GPGL commands.
///
/// The transport sends each entry in its own USB write. [`Job::concatenated`]
/// exists only for inspection and must not be written as a single transfer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Job {
    commands: Vec<Vec<u8>>,
}

impl Job {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a command (builder style).
    #[must_use]
    pub fn with(mut self, cmd: Vec<u8>) -> Self {
        self.commands.push(cmd);
        self
    }

    /// Append a command.
    pub fn push(&mut self, cmd: Vec<u8>) {
        self.commands.push(cmd);
    }

    pub fn commands(&self) -> &[Vec<u8>] {
        &self.commands
    }

    pub fn len(&self) -> usize {
        self.commands.len()
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    pub fn total_bytes(&self) -> usize {
        self.commands.iter().map(Vec::len).sum()
    }

    /// Concatenate all commands. **For display only** — sending this as one
    /// bulk transfer was observed to execute only the first move.
    pub fn concatenated(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.total_bytes());
        for c in &self.commands {
            out.extend_from_slice(c);
        }
        out
    }

    /// Human-readable hex dump, one command per line.
    pub fn hex_dump(&self) -> String {
        let mut out = String::new();
        for cmd in &self.commands {
            out.push_str(&hex(cmd));
            out.push('\n');
        }
        out
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The exact job that cut a closed 20 mm square on a Cameo 5 (firmware V1.07).
///
/// Recorded in `re_session.md`, Round 6. Coordinates are `(y, x)` in SU,
/// 200..600 = 10..30 mm.
///
/// The order of the two force commands matters: force is lowered to 1 while the
/// AutoBlade resets its 10 clicks, and only raised to 4 after the depth is set.
#[must_use]
pub fn golden_square_cut() -> Job {
    Job::new()
        .with(command::init())
        .with(command::reset()) // FN0
        .with(command::origin_current()) // SO0
        .with(command::mat(1)) // TG1 — 12x12 mat
        .with(command::select_tool(1)) // J1
        .with(command::speed(3, 1)) // !3,1
        .with(command::force(1, 1)) // FX1,1 — low force during blade reset
        .with(command::blade_reset()) // FY1
        .with(command::blade_depth(1, 1)) // TF1,1
        .with(command::force(4, 1)) // FX4,1
        .with(command::cutter_offset(18, 18, 1)) // FC18,18,1
        .with(command::lift_between_paths(1, 1)) // FE1,1
        .with(command::move_to(200, 200)) // M200,200
        .with(command::draw_to(200, 600)) // D200,600
        .with(command::draw_to(600, 600)) // D600,600
        .with(command::draw_to(600, 200)) // D600,200
        .with(command::draw_to(200, 200)) // D200,200 (closes)
        .with(command::select_tool(0)) // J0
        .with(command::reset()) // FN0
}

/// Blade settings for a job built from page geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CutSettings {
    pub tool: u8,
    pub speed: u8,
    pub force: u8,
    pub depth: u8,
}

impl Default for CutSettings {
    /// The settings that cut paper with the golden job.
    fn default() -> Self {
        Self {
            tool: 1,
            speed: 3,
            force: 4,
            depth: 1,
        }
    }
}

/// How page millimetres map to the machine's SU frame.
///
/// `1 SU = 0.05 mm`, coordinates are `(y, x)`.
///
/// The protocol reference states the origin is the **media top-left with y
/// increasing downwards** (matching SVG), so `flip_y` defaults to `false`.
/// That is still a secondary source: the golden square is symmetric and cannot
/// distinguish y-up from y-down. `--flip-y` selects the mirror hypothesis.
/// See `docs/FORMAT.md` §7.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CutMapping {
    pub page_height_mm: f32,
    pub flip_y: bool,
    /// Page-space point that becomes the machine origin (the registration
    /// mark's outer corner). Subtracted before the flip.
    pub origin_mm: (f32, f32),
    /// Empirical calibration offset in millimetres, machine frame
    /// (`x` right, `y` down). It nudges the whole job after the page mapping.
    pub offset_mm: (f32, f32),
}

impl CutMapping {
    /// Page height only; y is used as stored (y down).
    #[must_use]
    pub fn new(page_height_mm: f32) -> Self {
        Self {
            page_height_mm,
            flip_y: false,
            origin_mm: (0.0, 0.0),
            offset_mm: (0.0, 0.0),
        }
    }

    /// Mirror y using the page height (the "machine y-up" hypothesis).
    #[must_use]
    pub fn flipped(page_height_mm: f32) -> Self {
        Self {
            page_height_mm,
            flip_y: true,
            origin_mm: (0.0, 0.0),
            offset_mm: (0.0, 0.0),
        }
    }

    /// Set the page-space registration origin (subtracted before mapping).
    #[must_use]
    pub fn with_origin(mut self, x_mm: f32, y_mm: f32) -> Self {
        self.origin_mm = (x_mm, y_mm);
        self
    }

    /// Apply an empirical calibration offset, in millimetres of the machine
    /// frame. Positive `y` moves the job down (larger y SU).
    #[must_use]
    pub fn with_offset(mut self, x_mm: f32, y_mm: f32) -> Self {
        self.offset_mm = (x_mm, y_mm);
        self
    }

    /// Convert one page point to SU `(y, x)`.
    #[must_use]
    pub fn to_su(&self, x_mm: f32, y_mm: f32) -> (i32, i32) {
        let x_mm = x_mm - self.origin_mm.0;
        let y_mm = y_mm - self.origin_mm.1;
        let y_mm = if self.flip_y {
            self.page_height_mm - y_mm
        } else {
            y_mm
        };
        let x = ((x_mm + self.offset_mm.0) * 20.0).round() as i32;
        let y = ((y_mm + self.offset_mm.1) * 20.0).round() as i32;
        (y, x)
    }
}

/// Front matter of a cut job: initialise the machine, set the tool and the
/// blade sequence. It ends before the first `M`/`D` so a registration scan can
/// be inserted between setup and motion.
#[must_use]
pub fn polyline_prelude(settings: CutSettings) -> Job {
    Job::new()
        .with(command::init())
        .with(command::reset())
        .with(command::mat(1))
        .with(command::select_tool(settings.tool))
        .with(command::speed(settings.speed, settings.tool))
        .with(command::force(1, settings.tool))
        .with(command::blade_reset())
        .with(command::blade_depth(settings.depth, settings.tool))
        .with(command::force(settings.force, settings.tool))
        .with(command::cutter_offset(18, 18, settings.tool))
        .with(command::lift_between_paths(0, settings.tool))
}

/// Cutting area relative to the current origin, then continuous-cut mode.
///
/// `\0,0 Z{h},{w}` clips motion to the registration rectangle (or the page when
/// no scan is used). `FE0,0` disables lifting at sharp angles: with `FE1` the
/// blade lifts and lowers on every corner of a traced contour, which looks like
/// puncturing instead of cutting.
#[must_use]
pub fn polyline_area(height_su: i32, width_su: i32, tool: u8) -> Job {
    Job::new()
        .with(command::lower_left(0, 0))
        .with(command::upper_right(height_su, width_su))
        .with(command::line_type(0))
        .with(command::lift_between_paths(0, tool))
        .with(command::corner_sharpen(0, 0, tool))
}

/// Put paths in cutting order and make each closed contour start at its
/// topmost point.
///
/// Cutting top-to-bottom keeps the blade inside already-cut material instead of
/// dragging loose pieces, and starting a closed contour at its highest point
/// means the entry cut is the one the operator can see first.
#[must_use]
pub fn order_for_cut(mut polylines: Vec<(Vec<(f32, f32)>, bool)>) -> Vec<(Vec<(f32, f32)>, bool)> {
    for (points, closed) in &mut polylines {
        if *closed && points.len() >= 3 {
            if let Some(index) = points
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| a.1.total_cmp(&b.1).then(a.0.total_cmp(&b.0)))
                .map(|(i, _)| i)
            {
                points.rotate_left(index);
            }
        }
    }
    polylines.sort_by(|a, b| {
        let key = |p: &(Vec<(f32, f32)>, bool)| {
            let y = p.0.iter().map(|q| q.1).fold(f32::INFINITY, f32::min);
            let x = p.0.iter().map(|q| q.0).fold(f32::INFINITY, f32::min);
            (y, x)
        };
        let (ay, ax) = key(a);
        let (by, bx) = key(b);
        ay.total_cmp(&by).then(ax.total_cmp(&bx))
    });
    polylines
}

/// The `M`/`D` motion commands for the decoded polylines.
///
/// Consecutive points that round to the same SU position are dropped: a
/// zero-length `D` only lowers the blade in place and looks like a puncture,
/// which is exactly what dense traced contours produce.
#[must_use]
pub fn polyline_paths(polylines: &[(Vec<(f32, f32)>, bool)], mapping: CutMapping) -> Job {
    let mut job = Job::new();
    for (points, closed) in polylines {
        if points.len() < 2 {
            continue;
        }
        let (first_y, first_x) = mapping.to_su(points[0].0, points[0].1);
        let mut last = (first_y, first_x);
        let mut segments = 0usize;
        let mut body = Vec::new();
        for (px, py) in &points[1..] {
            let (y, x) = mapping.to_su(*px, *py);
            if (y, x) == last {
                continue;
            }
            last = (y, x);
            segments += 1;
            body.push(command::draw_to(y, x));
        }
        if *closed {
            let (y, x) = mapping.to_su(points[0].0, points[0].1);
            if (y, x) != last {
                segments += 1;
                body.push(command::draw_to(y, x));
            }
        }
        if segments == 0 {
            continue;
        }
        job.push(command::move_to(first_y, first_x));
        for cmd in body {
            job.push(cmd);
        }
    }
    job
}

/// End matter: lift the blade, return to the origin and park the tool.
#[must_use]
pub fn polyline_epilogue() -> Job {
    let mut job = Job::new();
    for cmd in command::end_job() {
        job.push(cmd);
    }
    job
}

/// Build a cut job from polylines in page millimetres.
///
/// Each entry is `(points, closed)`. Paths with fewer than two points are
/// skipped. The preamble and epilogue mirror [`golden_square_cut`] so the same
/// blade sequence applies.
#[must_use]
pub fn polyline_job(
    polylines: &[(Vec<(f32, f32)>, bool)],
    mapping: CutMapping,
    settings: CutSettings,
) -> Job {
    let mut job = polyline_prelude(settings);
    for cmd in polyline_paths(polylines, mapping).commands() {
        job.push(cmd.clone());
    }
    for cmd in polyline_epilogue().commands() {
        job.push(cmd.clone());
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_corners_map_to_su() {
        // 1 SU = 0.05 mm; default is y-down (protocol reference).
        let down = CutMapping::new(297.0);
        assert_eq!(down.to_su(0.0, 0.0), (0, 0));
        assert_eq!(down.to_su(210.0, 297.0), (5940, 4200));
        assert_eq!(down.to_su(105.0, 148.5), (2970, 2100));

        // Mirror hypothesis.
        let up = CutMapping::flipped(297.0);
        assert_eq!(up.to_su(0.0, 0.0), (5940, 0));
        assert_eq!(up.to_su(210.0, 297.0), (0, 4200));
    }

    #[test]
    fn calibration_offset_shifts_the_job() {
        let m = CutMapping::new(297.0).with_offset(0.5, -2.0);
        assert_eq!(m.to_su(0.0, 0.0), (-40, 10));
        assert_eq!(m.to_su(105.0, 148.5), (2930, 2110));
    }

    #[test]
    fn registration_origin_shifts_the_page() {
        // After a scan the machine's origin is the top-left mark; page points
        // are expressed relative to it.
        let m = CutMapping::new(297.0).with_origin(10.0, 10.0);
        assert_eq!(m.to_su(10.0, 10.0), (0, 0));
        assert_eq!(m.to_su(105.0, 148.5), (2770, 1900));
    }

    #[test]
    fn each_path_starts_with_a_lift_move() {
        let polylines = [
            (vec![(10.0, 10.0), (20.0, 10.0), (20.0, 20.0)], true),
            (vec![(50.0, 50.0), (60.0, 50.0), (60.0, 60.0)], true),
        ];
        let jobs: Vec<Job> = polylines
            .iter()
            .map(|path| polyline_paths(std::slice::from_ref(path), CutMapping::new(297.0)))
            .collect();
        assert_eq!(jobs.len(), 2);
        for job in jobs {
            assert_eq!(job.commands()[0][0], b'M');
            assert!(job.commands()[1..].iter().all(|cmd| cmd[0] == b'D'));
        }
    }

    #[test]
    fn epilogue_returns_to_origin_and_parks() {
        let job = polyline_epilogue();
        let commands: Vec<&[u8]> = job.commands().iter().map(Vec::as_slice).collect();
        assert!(commands.contains(&b"L0\x03".as_slice()));
        assert!(commands.contains(&b"M0,0\x03".as_slice()));
        assert!(commands.contains(&b"J0\x03".as_slice()));
        assert!(commands.contains(&b"FN0\x03".as_slice()));
    }

    #[test]
    fn split_job_matches_polyline_job() {
        let polylines = vec![(vec![(0.0, 0.0), (10.0, 0.0)], false)];
        let mapping = CutMapping::new(297.0);
        let settings = CutSettings::default();

        let full = polyline_job(&polylines, mapping, settings);
        let mut split = polyline_prelude(settings);
        for cmd in polyline_paths(&polylines, mapping).commands() {
            split.push(cmd.clone());
        }
        for cmd in polyline_epilogue().commands() {
            split.push(cmd.clone());
        }
        assert_eq!(full, split);
    }

    #[test]
    fn rectangle_fixture_maps_to_expected_su() {
        // Verified vertices from the A4 rectangle fixture.
        let points = vec![
            (69.1621, 89.2801),
            (69.1621, 188.9269),
            (141.712, 188.9269),
            (141.712, 89.2801),
        ];
        let job = polyline_job(
            &[(points, true)],
            CutMapping::new(297.0),
            CutSettings::default(),
        );

        let ascii: Vec<String> = job
            .commands()
            .iter()
            .filter(|c| matches!(c[0], b'M' | b'D') && c.as_slice() != b"M0,0\x03")
            .map(|c| String::from_utf8(c[..c.len() - 1].to_vec()).unwrap())
            .collect();

        assert_eq!(
            ascii,
            [
                "M1786,1383", // y = 89.2801 * 20 (y-down frame)
                "D3779,1383",
                "D3779,2834",
                "D1786,2834",
                "D1786,1383", // closes
            ]
        );
    }

    #[test]
    fn duplicate_points_are_dropped() {
        let points = vec![
            (10.0, 10.0),
            (10.0, 10.0),
            (20.0, 10.0),
            (20.0, 10.0),
            (20.0, 20.0),
        ];
        let job = polyline_paths(&[(points, true)], CutMapping::new(297.0));
        assert_eq!(job.len(), 4);
        assert_eq!(job.commands()[0], b"M200,200\x03");
        assert_eq!(job.commands()[3], b"D200,200\x03");
    }

    #[test]
    fn short_paths_are_skipped() {
        let job = polyline_job(
            &[(vec![(10.0, 10.0)], false), (vec![], true)],
            CutMapping::new(297.0),
            CutSettings::default(),
        );
        assert!(!job
            .commands()
            .iter()
            .any(|c| matches!(c[0], b'M' | b'D') && c.as_slice() != b"M0,0\x03"));
    }

    #[test]
    fn job_preamble_matches_the_golden_job() {
        let job = polyline_job(
            &[(vec![(10.0, 10.0), (20.0, 10.0)], false)],
            CutMapping::new(297.0),
            CutSettings::default(),
        );
        let golden = golden_square_cut();
        // The page prelude is the golden setup without `SO0`: the media origin
        // must not move to wherever the carriage happens to be parked.
        let mut expected = golden.commands()[..12].to_vec();
        expected.remove(2);
        expected.truncate(10);
        assert_eq!(&job.commands()[..10], &expected[..]);
        assert_eq!(job.commands()[10], b"FE0,1\x03");
        assert!(!job.commands().iter().any(|c| c == b"SO0\x03"));
        assert_eq!(
            &job.commands()[job.len() - 6..],
            &[
                b"L0\x03".as_slice(),
                b"\\0,0\x03".as_slice(),
                b"M0,0\x03".as_slice(),
                b"J0\x03".as_slice(),
                b"FN0\x03".as_slice(),
                b"TB50,0\x03".as_slice(),
            ]
        );
    }
}
