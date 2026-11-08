mod bt_dbus;
mod i18n;
mod import;
mod prefs;

use std::io::Write;
use std::path::{Path, PathBuf};

use eframe::egui;
use silhouette_core::{
    offset_polygon, regmark_shapes, trace_rgba, Path as CorePath, PathKind, Point, RegMarkSpec,
    RegMarkStyle, TraceMode, TraceOptions, TracedPath,
};
use silhouette_driver::{
    list_cameo5, order_for_cut, polyline_area, polyline_epilogue, polyline_paths, polyline_prelude,
    BtRfcommTransport, CutMapping, CutSettings, RegMarkScan, Session, Status, Transport,
    UsbTransport,
};
use silhouette_core::point_in_polygon;

use import::FontEntry;

type ArtOriginals = Vec<(usize, Vec<egui::Pos2>)>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Link {
    Usb,
    Bluetooth,
}

struct Machine {
    link: Link,
    // `Send` so a worker thread can open the transport, query firmware/status
    // and hand the ready machine back to the UI through a channel.
    session: Option<Session<Box<dyn Transport + Send>>>,
    firmware: String,
    status: String,
    detail: String,
    usb: Option<(u8, u8)>,
}

impl Machine {
    fn empty() -> Self {
        Self {
            link: Link::Usb,
            session: None,
            firmware: "—".to_string(),
            status: "desligada".to_string(),
            detail: "sem ligação".to_string(),
            usb: None,
        }
    }

    fn connected(&self) -> bool {
        self.session.is_some()
    }
}

/// Which trace engine the «Método» dropdown selects. `Studio` delegates to
/// vtracer (color clustering + curve fitting) for Studio-quality outlines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TraceMethod {
    /// vtracer + auto-tuned parameters (Otsu threshold, auto despeckle).
    Auto,
    Outline,
    Edges,
    Studio,
}

impl TraceMethod {
    fn key(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Outline => "outline",
            Self::Edges => "edges",
            Self::Studio => "studio",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        match key {
            "auto" => Some(Self::Auto),
            "outline" => Some(Self::Outline),
            "edges" => Some(Self::Edges),
            "studio" => Some(Self::Studio),
            _ => None,
        }
    }

    /// Both engines below delegate to vtracer's curve fitting.
    fn is_studio_family(self) -> bool {
        matches!(self, Self::Auto | Self::Studio)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    Select,
    Nodes,
    Text,
    Image,
    Pan,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Handle {
    Nw,
    N,
    Ne,
    E,
    Se,
    S,
    Sw,
    W,
    Rotate,
}

#[derive(Clone)]
enum Drag {
    Vertex {
        layer: usize,
        index: usize,
    },
    Move {
        start: egui::Pos2,
        bbox: egui::Rect,
        originals: Vec<(usize, Vec<egui::Pos2>)>,
    },
    Band {
        start: egui::Pos2,
        current: egui::Pos2,
        additive: bool,
    },
    Scale {
        handle: Handle,
        bbox: egui::Rect,
        originals: Vec<(usize, Vec<egui::Pos2>)>,
    },
    Rotate {
        center: egui::Pos2,
        start_angle: f32,
        originals: Vec<(usize, Vec<egui::Pos2>)>,
    },
    Art {
        start: egui::Pos2,
        art_start: egui::Vec2,
        originals: Vec<(usize, Vec<egui::Pos2>)>,
        offset: Option<OffsetPreview>,
        trace: Option<Vec<TracedPath>>,
    },
    ArtScale {
        handle: Handle,
        anchor: egui::Pos2,
        start: egui::Rect,
        originals: Vec<(usize, Vec<egui::Pos2>)>,
        offset: Option<OffsetPreview>,
        trace: Option<Vec<TracedPath>>,
    },
}

#[derive(Clone, Copy)]
enum Guide {
    V(f32),
    H(f32),
}

#[derive(Clone)]
struct BtDevice {
    mac: String,
    name: String,
}

#[derive(Clone, Copy)]
enum Align {
    Left,
    CenterH,
    Right,
    Top,
    CenterV,
    Bottom,
}

#[derive(Clone, Copy)]
struct Affine {
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
}

impl Affine {
    fn identity() -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 0.0,
            f: 0.0,
        }
    }

    fn translate(dx: f32, dy: f32) -> Self {
        Self {
            e: dx,
            f: dy,
            ..Self::identity()
        }
    }

    fn scale(sx: f32, sy: f32) -> Self {
        Self {
            a: sx,
            d: sy,
            ..Self::identity()
        }
    }

    fn rotate(angle: f32) -> Self {
        let (sin, cos) = angle.sin_cos();
        Self {
            a: cos,
            b: sin,
            c: -sin,
            d: cos,
            e: 0.0,
            f: 0.0,
        }
    }

    fn flip(horizontal: bool) -> Self {
        if horizontal {
            Self {
                a: -1.0,
                ..Self::identity()
            }
        } else {
            Self {
                d: -1.0,
                ..Self::identity()
            }
        }
    }

    fn apply(&self, p: egui::Pos2) -> egui::Pos2 {
        egui::pos2(
            self.a * p.x + self.c * p.y + self.e,
            self.b * p.x + self.d * p.y + self.f,
        )
    }

    fn linear(&self, v: egui::Vec2) -> egui::Vec2 {
        egui::vec2(self.a * v.x + self.c * v.y, self.b * v.x + self.d * v.y)
    }

    fn then(&self, next: &Affine) -> Affine {
        Affine {
            a: next.a * self.a + next.c * self.b,
            b: next.b * self.a + next.d * self.b,
            c: next.a * self.c + next.c * self.d,
            d: next.b * self.c + next.d * self.d,
            e: next.a * self.e + next.c * self.f + next.e,
            f: next.b * self.e + next.d * self.f + next.f,
        }
    }
}

#[derive(Clone)]
struct Layer {
    kind: PathKind,
    points: Vec<egui::Pos2>,
    original: Vec<egui::Pos2>,
    closed: bool,
    visible: bool,
    group: Option<u64>,
    owner: Option<u64>,
    traced: bool,
    /// Index into `Document.arts` for layers produced by tracing that image;
    /// they move/scale with their art and are replaced when it re-traces.
    art: Option<usize>,
    color: egui::Color32,
}

const LAYER_COLOR: egui::Color32 = egui::Color32::from_rgb(210, 40, 40);

/// udev rule shipped with the project, embedded so the GUI can install it
/// with one polkit prompt — an AppImage cannot grant USB permissions itself.
const UDEV_RULE: &str = include_str!("../../../packaging/udev/99-silhouette.rules");

#[derive(Clone)]
struct TextItem {
    id: u64,
    group: Option<u64>,
    font: String,
    text: String,
    size: f32,
    origin: egui::Pos2,
    affine: Affine,
}

enum TextAction {
    Apply {
        id: u64,
        text: String,
        font: String,
        size: f32,
    },
    Convert(u64),
    Delete(u64),
}

enum MachineAction {
    Usb(usize),
    Bluetooth(usize, String, String),
    Disconnect(usize),
    Refresh,
}

/// Query firmware/status and bundle a ready `Machine` (runs on worker threads).
fn machine_from_transport(
    link: Link,
    detail: String,
    usb: Option<(u8, u8)>,
    mut session: Session<Box<dyn Transport + Send>>,
) -> Machine {
    let firmware = session
        .firmware()
        .unwrap_or_else(|e| format!("firmware? {e}"));
    let status = session
        .status()
        .map(|s| App::status_text(s).to_string())
        .unwrap_or_else(|e| format!("estado? {e}"));
    Machine {
        link,
        session: Some(session),
        firmware,
        status,
        detail,
        usb,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PrintQuality {
    Normal,
    High,
}

impl PrintQuality {
    const ALL: [Self; 2] = [Self::Normal, Self::High];

    fn label(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::High => "Alta",
        }
    }

    fn cups(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::High => "High",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PrintColor {
    Rgb,
    Gray,
    AutoGray,
    ProcessGray,
}

impl PrintColor {
    const ALL: [Self; 4] = [Self::Rgb, Self::Gray, Self::AutoGray, Self::ProcessGray];

    fn label(self) -> &'static str {
        match self {
            Self::Rgb => "RGB",
            Self::Gray => "Preto e branco",
            Self::AutoGray => "Cinza automático",
            Self::ProcessGray => "Cinza (processo)",
        }
    }

    fn cups(self) -> &'static str {
        match self {
            Self::Rgb => "RGB",
            Self::Gray => "Gray",
            Self::AutoGray => "AutoGray",
            Self::ProcessGray => "ProcessGray",
        }
    }

    fn ipp(self) -> &'static str {
        match self {
            Self::Rgb => "color",
            Self::Gray | Self::ProcessGray => "monochrome",
            Self::AutoGray => "auto-monochrome",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PrintMedia {
    Stationery,
    Photographic,
    PhotographicMatte,
    PhotographicGlossy,
    PhotographicSemiGloss,
    PhotographicHighGloss,
    StationeryCoated,
    Envelope,
}

impl PrintMedia {
    const ALL: [Self; 8] = [
        Self::Stationery,
        Self::Photographic,
        Self::PhotographicMatte,
        Self::PhotographicGlossy,
        Self::PhotographicSemiGloss,
        Self::PhotographicHighGloss,
        Self::StationeryCoated,
        Self::Envelope,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Stationery => "Papel normal",
            Self::Photographic => "Fotográfico",
            Self::PhotographicMatte => "Mate",
            Self::PhotographicGlossy => "Brilhante",
            Self::PhotographicSemiGloss => "Semi-brilhante",
            Self::PhotographicHighGloss => "Alto brilho",
            Self::StationeryCoated => "Couché",
            Self::Envelope => "Envelope",
        }
    }

    fn cups(self) -> &'static str {
        match self {
            Self::Stationery => "Stationery",
            Self::Photographic => "Photographic",
            Self::PhotographicMatte => "PhotographicMatte",
            Self::PhotographicGlossy => "PhotographicGlossy",
            Self::PhotographicSemiGloss => "PhotographicSemiGloss",
            Self::PhotographicHighGloss => "PhotographicHighGloss",
            Self::StationeryCoated => "StationeryCoated",
            Self::Envelope => "Envelope",
        }
    }
}

fn print_option_args(quality: PrintQuality, color: PrintColor, media: PrintMedia) -> Vec<String> {
    vec![
        "-o".to_string(),
        format!("cupsPrintQuality={}", quality.cups()),
        "-o".to_string(),
        format!("ColorModel={}", color.cups()),
        "-o".to_string(),
        format!("print-color-mode={}", color.ipp()),
        "-o".to_string(),
        format!("MediaType={}", media.cups()),
    ]
}

fn cut_log(line: &str) {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let dir = base.join("sillinux");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("cut.log"))
    else {
        return;
    };
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = writeln!(file, "{secs} {line}");
}

fn command_text(cmd: &[u8]) -> String {
    let body = cmd.strip_suffix(&[0x03]).unwrap_or(cmd);
    String::from_utf8_lossy(body).to_string()
}

#[cfg(test)]
mod cut_tests {
    use super::*;

    #[test]
    fn cut_order_starts_at_the_top() {
        let low = vec![(10.0, 200.0), (20.0, 200.0)];
        let high = vec![(10.0, 20.0), (20.0, 20.0)];
        let ordered = order_for_cut(vec![(low, false), (high, false)]);
        assert_eq!(ordered[0].0[0].1, 20.0);
    }

    #[test]
    fn closed_contour_starts_at_its_top_point() {
        let square = vec![(10.0, 30.0), (10.0, 10.0), (30.0, 10.0), (30.0, 30.0)];
        let ordered = order_for_cut(vec![(square, true)]);
        assert_eq!(ordered[0].0[0], (10.0, 10.0));
    }
}

#[cfg(test)]
#[cfg(test)]
mod studio_tests {
    use super::*;

    #[test]
    fn studio_engine_manual_traces_white_content_on_black_frame() {
        // The cactus-template case: bright shapes cover most of a dark frame.
        // The border ring must decide the polarity (dark background), not the
        // histogram peak, or the engine traces the background instead.
        let (w, h) = (100u32, 100u32);
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..w * h {
            rgba.extend_from_slice(&[30, 30, 30, 255]); // dark background
        }
        for y in 10..90 {
            for x in 10..90 {
                let i = ((y * w + x) * 4) as usize;
                rgba[i] = 240;
                rgba[i + 1] = 240;
                rgba[i + 2] = 240;
            }
        }
        // A dark hole in the middle (like the slit in a template piece).
        for y in 45..55 {
            for x in 45..55 {
                let i = ((y * w + x) * 4) as usize;
                rgba[i] = 30;
                rgba[i + 1] = 30;
                rgba[i + 2] = 30;
            }
        }
        let job = TraceJob {
            rgba: std::sync::Arc::new(rgba),
            width: w,
            height: h,
            max_px: 200,
            studio_noise_pct: 0.0,
            outer_only: false,
            apply_offset: false,
            offset_mm: 0.0,
            method: TraceMethod::Studio, // manual, not Auto
            rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(100.0, 100.0)),
            offset: egui::Vec2::ZERO,
            hue: 0.0,
            saturation: 1.0,
            vibrance: 0.0,
            contrast: 1.0,
            brightness: 0.0,
            options: TraceOptions {
                threshold: 128,
                min_area_mm2: 0.0,
                ..TraceOptions::default()
            },
        };
        let paths = run_trace_job(&job);
        assert_eq!(paths.len(), 2, "outer contour + hole, nothing else");
    }

    #[test]
    fn studio_engine_traces_a_black_square_on_white() {
        // Non-square mapping on purpose: 0.5 mm/px in X, 2 mm/px in Y, plus a
        // page offset — the exact case that used to push the trace off-page.
        let (w, h) = (100u32, 100u32);
        let mut rgba = vec![255u8; (w * h * 4) as usize];
        for y in 30..70 {
            for x in 30..70 {
                let i = ((y * w + x) * 4) as usize;
                rgba[i] = 0;
                rgba[i + 1] = 0;
                rgba[i + 2] = 0;
            }
        }
        let job = TraceJob {
            rgba: std::sync::Arc::new(rgba),
            width: w,
            height: h,
            max_px: 200,
            studio_noise_pct: 0.0,
            outer_only: false,
            apply_offset: false,
            offset_mm: 0.0,
            method: TraceMethod::Studio,
            rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(50.0, 200.0)),
            offset: egui::vec2(10.0, 20.0),
            hue: 0.0,
            saturation: 1.0,
            vibrance: 0.0,
            contrast: 1.0,
            brightness: 0.0,
            options: TraceOptions {
                threshold: 128,
                min_area_mm2: 0.0,
                ..TraceOptions::default()
            },
        };
        let paths = run_trace_job(&job);
        assert_eq!(paths.len(), 1, "one cluster -> one closed contour");
        assert!(paths[0].closed);
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for (x, y) in &paths[0].points {
            x0 = x0.min(*x);
            y0 = y0.min(*y);
            x1 = x1.max(*x);
            y1 = y1.max(*y);
        }
        // x: 30..70 px * 0.5 mm/px + 10 | y: 30..70 px * 2 mm/px + 20
        assert!((x0 - 25.0).abs() < 3.0, "x0 {x0}");
        assert!((y0 - 80.0).abs() < 6.0, "y0 {y0}");
        assert!((x1 - 45.0).abs() < 3.0, "x1 {x1}");
        assert!((y1 - 160.0).abs() < 6.0, "y1 {y1}");
    }
}

mod print_tests {
    use super::*;

    fn args(color: PrintColor, quality: PrintQuality) -> Vec<String> {
        print_option_args(quality, color, PrintMedia::Stationery)
    }

    #[test]
    fn rgb_forces_colour_mode() {
        let args = args(PrintColor::Rgb, PrintQuality::High);
        assert!(args.contains(&"print-color-mode=color".to_string()));
        assert!(args.contains(&"ColorModel=RGB".to_string()));
        assert!(args.contains(&"cupsPrintQuality=High".to_string()));
        assert!(args.contains(&"MediaType=Stationery".to_string()));
    }

    #[test]
    fn gray_modes_do_not_ask_for_colour() {
        for color in [PrintColor::Gray, PrintColor::ProcessGray] {
            let args = args(color, PrintQuality::Normal);
            assert!(args.contains(&"print-color-mode=monochrome".to_string()));
        }
        let auto = args(PrintColor::AutoGray, PrintQuality::Normal);
        assert!(auto.contains(&"print-color-mode=auto-monochrome".to_string()));
    }

    #[test]
    fn every_option_has_its_dash_o() {
        let args = args(PrintColor::Rgb, PrintQuality::Normal);
        assert_eq!(args.len(), 8);
        for pair in args.chunks(2) {
            assert_eq!(pair[0], "-o");
        }
    }
}

#[derive(Clone)]
struct Snapshot {
    layers: Vec<Layer>,
    texts: Vec<TextItem>,
    next_id: u64,
    art_offsets: Vec<egui::Vec2>,
    art_rects: Vec<egui::Rect>,
    /// Full images (shared Arc buffers) so undo can resurrect a deleted one.
    arts: Vec<ArtSnapshot>,
}

/// Everything needed to rebuild an `Artwork` (texture recreated on restore).
#[derive(Clone)]
struct ArtSnapshot {
    name: String,
    rgba: std::sync::Arc<Vec<u8>>,
    width: u32,
    height: u32,
    rect: egui::Rect,
    visible: bool,
    hue: f32,
    saturation: f32,
    vibrance: f32,
    contrast: f32,
    brightness: f32,
}

#[derive(Clone)]
struct OffsetPreview {
    external: bool,
    distance: f32,
    contours: Vec<(usize, Vec<Vec<egui::Pos2>>)>,
    skipped: usize,
}

impl Layer {
    fn width(&self) -> f32 {
        bounds(&self.points).map(|r| r.width()).unwrap_or(0.0)
    }

    fn height(&self) -> f32 {
        bounds(&self.points).map(|r| r.height()).unwrap_or(0.0)
    }

    fn rect_placed(&self, scale: f32, offset: egui::Vec2) -> Option<egui::Rect> {
        let r = bounds(&self.points)?;
        Some(egui::Rect::from_min_max(
            egui::pos2(r.min.x * scale + offset.x, r.min.y * scale + offset.y),
            egui::pos2(r.max.x * scale + offset.x, r.max.y * scale + offset.y),
        ))
    }
}

fn bounds(points: &[egui::Pos2]) -> Option<egui::Rect> {
    if points.is_empty() {
        return None;
    }
    let mut min = points[0];
    let mut max = points[0];
    for p in points {
        min.x = min.x.min(p.x);
        min.y = min.y.min(p.y);
        max.x = max.x.max(p.x);
        max.y = max.y.max(p.y);
    }
    Some(egui::Rect::from_min_max(min, max))
}

fn scale_point(p: egui::Pos2, anchor: egui::Pos2, sx: f32, sy: f32) -> egui::Pos2 {
    egui::pos2(
        anchor.x + (p.x - anchor.x) * sx,
        anchor.y + (p.y - anchor.y) * sy,
    )
}

fn scale_rect(r: egui::Rect, anchor: egui::Pos2, sx: f32, sy: f32) -> egui::Rect {
    egui::Rect::from_min_max(
        scale_point(r.min, anchor, sx, sy),
        scale_point(r.max, anchor, sx, sy),
    )
}

fn union(a: Option<egui::Rect>, b: Option<egui::Rect>) -> Option<egui::Rect> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.union(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn kind_name(kind: PathKind) -> &'static str {
    match kind {
        PathKind::Line => "linha",
        PathKind::Rectangle => "retângulo",
        PathKind::Curve => "curva",
        PathKind::Other => "outro",
    }
}

fn printer_state(name: &str) -> String {
    let Ok(out) = std::process::Command::new("lpstat")
        .args(["-p", name])
        .output()
    else {
        return "lpstat indisponível".to_string();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.contains("now printing") {
            return "a imprimir".to_string();
        }
        if lower.contains("is idle") {
            return "pronta (idle)".to_string();
        }
        if lower.contains("disabled") {
            return "desativada".to_string();
        }
        if lower.contains("is stopped") {
            return "parada".to_string();
        }
    }
    let trimmed = text.trim();
    if trimmed.is_empty() {
        "estado desconhecido".to_string()
    } else {
        trimmed.to_string()
    }
}

fn job_state(job: &str) -> String {
    let active = std::process::Command::new("lpstat")
        .args(["-W", "not-completed", "-o"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    if active.lines().any(|l| l.starts_with(job)) {
        return format!("{job}: na fila (ainda não impresso)");
    }
    let completed = std::process::Command::new("lpstat")
        .args(["-W", "completed", "-l", "-o"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    let mut in_job = false;
    for line in completed.lines() {
        if line.starts_with(job) {
            in_job = true;
        } else if !line.starts_with(char::is_whitespace) && !line.is_empty() {
            in_job = false;
        }
        if in_job && line.trim_start().starts_with("Alerts:") {
            let alert = line.trim_start().trim_start_matches("Alerts:").trim();
            if !alert.is_empty() {
                return format!("{job}: {alert}");
            }
        }
    }
    format!("{job}: terminado ou cancelado (ver lpstat -W completed -o)")
}

fn cups_printers() -> Vec<String> {
    let mut printers = Vec::new();
    if let Ok(out) = std::process::Command::new("lpstat").arg("-p").output() {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("printer ") {
                if let Some(name) = rest.split_whitespace().next() {
                    if !printers.iter().any(|p| p == name) {
                        printers.push(name.to_string());
                    }
                }
            }
        }
    }
    printers.sort();
    printers
}

fn preferred_printer(printers: &[String]) -> usize {
    printers
        .iter()
        .position(|p| {
            let name = p.to_ascii_lowercase();
            !name.contains("cameo") && !name.contains("silhouette")
        })
        .unwrap_or(0)
}

fn is_cutter_queue(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("cameo") || lower.contains("silhouette")
}

fn queue_has_epson_options(name: &str) -> bool {
    std::process::Command::new("lpoptions")
        .args(["-p", name, "-l"])
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).contains("cupsPrintQuality"))
        .unwrap_or(false)
}

fn best_print_queue(printers: &[String]) -> Option<usize> {
    let usable = |p: &String| !is_cutter_queue(p);
    printers
        .iter()
        .enumerate()
        .find(|(_, p)| usable(p) && queue_has_epson_options(p))
        .map(|(i, _)| i)
}

fn parse_bt_devices(text: &str) -> Vec<BtDevice> {
    let mut devices: Vec<BtDevice> = Vec::new();
    for line in text.lines() {
        let Some(pos) = line.find("Device ") else {
            continue;
        };
        let rest = &line[pos + 7..];
        let mut parts = rest.splitn(2, char::is_whitespace);
        let mac = parts.next().unwrap_or("").trim();
        let name = parts.next().unwrap_or("").trim();
        if mac.len() != 17 || !mac.contains(':') {
            continue;
        }
        if let Some(existing) = devices.iter_mut().find(|d| d.mac == mac) {
            if existing.name.is_empty() && !name.is_empty() {
                existing.name = name.to_string();
            }
        } else {
            devices.push(BtDevice {
                mac: mac.to_string(),
                name: name.to_string(),
            });
        }
    }
    devices.sort_by(|a, b| a.name.cmp(&b.name).then(a.mac.cmp(&b.mac)));
    devices
}

fn scan_bt_devices() -> Vec<BtDevice> {
    // BlueZ por D-Bus primeiro (sem binário externo); se não houver BlueZ,
    // cai para o parse da saída do bluetoothctl, como antes.
    match bt_dbus::scan(8) {
        Ok(devices) => return devices,
        Err(_) => {}
    }
    let mut text = String::new();
    if let Ok(out) = std::process::Command::new("bluetoothctl")
        .args(["--timeout", "10", "scan", "on"])
        .output()
    {
        text.push_str(&String::from_utf8_lossy(&out.stdout));
        text.push_str(&String::from_utf8_lossy(&out.stderr));
    }
    if let Ok(out) = std::process::Command::new("bluetoothctl")
        .arg("devices")
        .output()
    {
        text.push('\n');
        text.push_str(&String::from_utf8_lossy(&out.stdout));
    }
    parse_bt_devices(&text)
}

fn color_test_rgba(width: u32, height: u32) -> Vec<u8> {
    let colors = [
        (255u8, 0u8, 0u8),
        (0, 190, 0),
        (0, 80, 255),
        (0, 210, 220),
        (225, 0, 190),
        (255, 215, 0),
        (0, 0, 0),
    ];
    let bars = ((height as f32) * 0.66) as u32;
    let mut out = vec![255u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let i = ((y * width + x) * 4) as usize;
            let (r, g, b) = if y < bars {
                let idx = (x as usize * colors.len() / width.max(1) as usize).min(colors.len() - 1);
                colors[idx]
            } else {
                let v = (x as f32 / width.max(1) as f32 * 255.0).round() as u8;
                (v, v, v)
            };
            out[i] = r;
            out[i + 1] = g;
            out[i + 2] = b;
            out[i + 3] = 255;
        }
    }
    out
}

fn queue_name_for(mac: &str) -> String {
    let tail: String = mac.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    let tail = tail.chars().rev().take(4).collect::<String>();
    format!("Sillinux-BT-{tail}")
}

#[derive(Clone)]
struct Artwork {
    /// Shared so snapshots (undo) and trace jobs can hold it without copying
    /// megabytes.
    rgba: std::sync::Arc<Vec<u8>>,
    width: u32,
    height: u32,
    preview: Vec<u8>,
    pw: u32,
    ph: u32,
    rect: egui::Rect,
    texture: egui::TextureHandle,
    name: String,
    /// Per-image layer state: hide/show and color adjustments are owned by
    /// each image, not by the document — every image edits independently.
    visible: bool,
    hue: f32,
    saturation: f32,
    vibrance: f32,
    contrast: f32,
    brightness: f32,
}

fn color_image(rgba: &[u8], w: u32, h: u32) -> egui::ColorImage {
    let pixels = rgba
        .chunks_exact(4)
        .map(|c| egui::Color32::from_rgb(c[0], c[1], c[2]))
        .collect();
    egui::ColorImage {
        size: [w as usize, h as usize],
        pixels,
    }
}

impl Artwork {
    fn new(
        ctx: &egui::Context,
        name: String,
        rgba: Vec<u8>,
        width: u32,
        height: u32,
        rect: egui::Rect,
    ) -> Self {
        let rgba = std::sync::Arc::new(rgba);
        let (preview, pw, ph) = import::downscale(&rgba, width, height, 1200);
        let texture = ctx.load_texture(
            format!("art-{name}"),
            color_image(&preview, pw, ph),
            egui::TextureOptions::LINEAR,
        );
        Self {
            rgba,
            width,
            height,
            preview,
            pw,
            ph,
            rect,
            texture,
            name,
            visible: true,
            hue: 0.0,
            saturation: 1.0,
            vibrance: 0.0,
            contrast: 1.0,
            brightness: 0.0,
        }
    }

    fn refresh(&mut self, ctx: &egui::Context) {
        let adjusted = import::adjust_rgba(
            &self.preview,
            self.hue,
            self.saturation,
            self.vibrance,
            self.contrast,
            self.brightness,
        );
        self.texture.set(
            color_image(&adjusted, self.pw, self.ph),
            egui::TextureOptions::LINEAR,
        );
        ctx.request_repaint();
    }

    /// Full-resolution PNG with this image's adjustments applied.
    fn png(&self) -> Result<Vec<u8>, String> {
        let adjusted = self.adjusted();
        import::encode_png(&adjusted, self.width, self.height)
    }

    /// Full-resolution RGBA with this image's adjustments applied — the same
    /// filter the preview shows, used for tracing so cut lines follow what the
    /// user sees.
    fn adjusted(&self) -> Vec<u8> {
        import::adjust_rgba(
            &self.rgba,
            self.hue,
            self.saturation,
            self.vibrance,
            self.contrast,
            self.brightness,
        )
    }
}

struct Document {
    path: PathBuf,
    page: (f32, f32),
    layers: Vec<Layer>,
    arts: Vec<Artwork>,
    notes: Vec<String>,
    texts: Vec<TextItem>,
    next_id: u64,
    art_offsets: Vec<egui::Vec2>,
}

impl Document {
    fn load(ctx: &egui::Context, path: PathBuf, fonts: &[FontEntry]) -> Result<Self, String> {
        let imported = import::load(&path, fonts)?;
        let layers = imported
            .paths
            .iter()
            .map(|p| {
                let points: Vec<egui::Pos2> = p
                    .points
                    .iter()
                    .map(|q| egui::pos2(q.x_mm, q.y_mm))
                    .collect();
                Layer {
                    kind: p.kind,
                    original: points.clone(),
                    points,
                    closed: p.closed,
                    visible: true,
                    group: None,
                    owner: None,
                    traced: false,
                    art: None,
                    color: LAYER_COLOR,
                }
            })
            .collect();
        let arts = imported
            .art
            .into_iter()
            .map(|a| {
                Artwork::new(
                    ctx,
                    a.name.clone(),
                    a.rgba,
                    a.width,
                    a.height,
                    egui::Rect::from_min_size(
                        egui::pos2(a.rect_mm.0, a.rect_mm.1),
                        egui::vec2(a.rect_mm.2, a.rect_mm.3),
                    ),
                )
            })
            .collect::<Vec<_>>();
        let art_offsets = vec![egui::Vec2::ZERO; arts.len()];
        Ok(Self {
            path,
            page: imported.page_mm,
            layers,
            arts,
            notes: imported.notes,
            texts: Vec::new(),
            next_id: 1,
            art_offsets,
        })
    }
}

fn dist_to_segment(p: egui::Pos2, a: egui::Pos2, b: egui::Pos2) -> f32 {
    let ab = b - a;
    let len2 = ab.length_sq();
    if len2 < 1e-9 {
        return (p - a).length();
    }
    let t = ((p - a).dot(ab) / len2).clamp(0.0, 1.0);
    (a + ab * t - p).length()
}

fn closest_on_segment(p: egui::Pos2, a: egui::Pos2, b: egui::Pos2) -> egui::Pos2 {
    let ab = b - a;
    let len2 = ab.length_sq();
    if len2 < 1e-9 {
        return a;
    }
    let t = ((p - a).dot(ab) / len2).clamp(0.0, 1.0);
    a + ab * t
}

fn radial_glow(
    painter: &egui::Painter,
    center: egui::Pos2,
    radius: egui::Vec2,
    color: egui::Color32,
) {
    let steps = 64usize;
    let mut mesh = egui::epaint::Mesh::default();
    mesh.colored_vertex(center, color);
    for i in 0..steps {
        let angle = i as f32 / steps as f32 * std::f32::consts::TAU;
        mesh.colored_vertex(
            center + egui::vec2(angle.cos() * radius.x, angle.sin() * radius.y),
            egui::Color32::TRANSPARENT,
        );
    }
    for i in 0..steps {
        mesh.add_triangle(0, 1 + i as u32, 1 + ((i + 1) % steps) as u32);
    }
    painter.add(egui::Shape::mesh(mesh));
}

fn paint_body_gradient(painter: &egui::Painter, rect: egui::Rect, dark: bool) {
    if dark {
        painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(16, 22, 30));
        radial_glow(
            painter,
            egui::pos2(rect.center().x, rect.top() - rect.height() * 0.1),
            egui::vec2(rect.width() * 0.7, rect.height() * 0.6),
            egui::Color32::from_rgba_unmultiplied(40, 90, 150, 90),
        );
        radial_glow(
            painter,
            egui::pos2(rect.center().x, rect.bottom() + rect.height() * 0.1),
            egui::vec2(rect.width() * 0.6, rect.height() * 0.45),
            egui::Color32::from_rgba_unmultiplied(40, 80, 130, 60),
        );
        return;
    }

    let top = egui::Color32::from_rgb(216, 236, 255);
    let mid = egui::Color32::from_rgb(176, 208, 238);
    let bottom = egui::Color32::from_rgb(150, 182, 214);
    let y_mid = rect.top() + rect.height() * 0.45;
    let mut mesh = egui::epaint::Mesh::default();
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(egui::pos2(rect.left(), y_mid), mid);
    mesh.colored_vertex(egui::pos2(rect.right(), y_mid), mid);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.indices = vec![0, 1, 2, 1, 3, 2, 2, 3, 4, 3, 5, 4];
    painter.add(egui::Shape::mesh(mesh));

    radial_glow(
        painter,
        egui::pos2(rect.center().x, rect.top() - rect.height() * 0.12),
        egui::vec2(rect.width() * 0.72, rect.height() * 0.62),
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 120),
    );
    radial_glow(
        painter,
        egui::pos2(rect.center().x, rect.bottom() + rect.height() * 0.18),
        egui::vec2(rect.width() * 0.68, rect.height() * 0.5),
        egui::Color32::from_rgba_unmultiplied(160, 195, 230, 80),
    );
    radial_glow(
        painter,
        egui::pos2(rect.left() + rect.width() * 0.22, rect.center().y),
        egui::vec2(rect.width() * 0.42, rect.height() * 0.55),
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 45),
    );
}

fn glass_frame(ctx: &egui::Context) -> egui::Frame {
    let dark = ctx.style().visuals.dark_mode;
    let fill = if dark {
        egui::Color32::from_rgba_unmultiplied(24, 34, 48, 236)
    } else {
        egui::Color32::from_rgba_unmultiplied(247, 252, 255, 228)
    };
    let line = if dark {
        egui::Color32::from_rgba_unmultiplied(120, 160, 200, 45)
    } else {
        egui::Color32::from_rgba_unmultiplied(120, 160, 200, 60)
    };
    egui::Frame::none()
        .fill(fill)
        .stroke(egui::Stroke::new(1.0_f32, line))
}

fn primary_button(text: egui::RichText) -> egui::Button<'static> {
    egui::Button::new(text.color(egui::Color32::WHITE).strong())
        .fill(egui::Color32::from_rgb(0, 136, 255))
        .stroke(egui::Stroke::new(
            1.0_f32,
            egui::Color32::from_rgb(24, 104, 200),
        ))
        .rounding(egui::Rounding::same(9.0))
}

fn apply_theme(ctx: &egui::Context, dark: bool) {
    let mut visuals = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    let accent = egui::Color32::from_rgb(0, 136, 255);
    let accent_light = egui::Color32::from_rgb(102, 186, 255);
    let accent_dark = egui::Color32::from_rgb(0, 102, 204);
    let text = if dark {
        egui::Color32::from_rgb(232, 240, 248)
    } else {
        egui::Color32::from_rgb(26, 42, 58)
    };
    let line = if dark {
        egui::Color32::from_rgba_unmultiplied(120, 160, 200, 55)
    } else {
        egui::Color32::from_rgba_unmultiplied(120, 160, 200, 89)
    };
    let glass = if dark {
        egui::Color32::from_rgba_unmultiplied(38, 50, 66, 230)
    } else {
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 210)
    };
    let glass_mid = if dark {
        egui::Color32::from_rgba_unmultiplied(30, 42, 58, 210)
    } else {
        egui::Color32::from_rgba_unmultiplied(245, 250, 255, 186)
    };

    visuals.selection.bg_fill = accent;
    visuals.selection.stroke = egui::Stroke::new(1.0_f32, egui::Color32::WHITE);
    visuals.hyperlink_color = accent_dark;
    visuals.warn_fg_color = egui::Color32::from_rgb(255, 154, 74);
    visuals.error_fg_color = egui::Color32::from_rgb(224, 74, 90);
    visuals.panel_fill = glass;
    visuals.window_fill = if dark {
        egui::Color32::from_rgba_unmultiplied(30, 42, 58, 248)
    } else {
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 246)
    };
    visuals.faint_bg_color = glass_mid;
    visuals.extreme_bg_color = if dark {
        egui::Color32::from_rgb(20, 28, 38)
    } else {
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 225)
    };
    visuals.code_bg_color = glass_mid;
    visuals.window_rounding = egui::Rounding::same(16.0);
    visuals.menu_rounding = egui::Rounding::same(10.0);
    visuals.window_stroke = egui::Stroke::new(
        1.0_f32,
        if dark {
            egui::Color32::from_rgba_unmultiplied(120, 160, 200, 60)
        } else {
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 220)
        },
    );
    visuals.window_shadow = egui::epaint::Shadow {
        offset: egui::vec2(0.0, 10.0),
        blur: 30.0,
        spread: 0.0,
        color: egui::Color32::from_rgba_unmultiplied(30, 70, 120, 60),
    };
    visuals.popup_shadow = egui::epaint::Shadow {
        offset: egui::vec2(0.0, 8.0),
        blur: 30.0,
        spread: 0.0,
        color: egui::Color32::from_rgba_unmultiplied(30, 70, 120, 55),
    };
    visuals.button_frame = true;
    visuals.striped = false;
    visuals.indent_has_left_vline = false;
    visuals.slider_trailing_fill = true;

    visuals.widgets.noninteractive.bg_fill = glass;
    visuals.widgets.noninteractive.weak_bg_fill = glass_mid;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, line);
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, text);
    visuals.widgets.noninteractive.rounding = egui::Rounding::same(8.0);

    visuals.widgets.inactive.bg_fill = glass;
    visuals.widgets.inactive.weak_bg_fill = glass;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, line);
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0_f32, text);
    visuals.widgets.inactive.rounding = egui::Rounding::same(9.0);

    visuals.widgets.hovered.bg_fill = if dark {
        egui::Color32::from_rgba_unmultiplied(60, 80, 104, 235)
    } else {
        egui::Color32::from_rgba_unmultiplied(224, 240, 255, 240)
    };
    visuals.widgets.hovered.weak_bg_fill = visuals.widgets.hovered.bg_fill;
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, accent_light);
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.5_f32, accent_dark);
    visuals.widgets.hovered.rounding = egui::Rounding::same(9.0);

    visuals.widgets.active.bg_fill = accent;
    visuals.widgets.active.weak_bg_fill = accent;
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0_f32, accent_dark);
    visuals.widgets.active.fg_stroke = egui::Stroke::new(1.5_f32, egui::Color32::WHITE);
    visuals.widgets.active.rounding = egui::Rounding::same(9.0);

    visuals.widgets.open.bg_fill = visuals.widgets.hovered.bg_fill;
    visuals.widgets.open.weak_bg_fill = visuals.widgets.hovered.weak_bg_fill;
    visuals.widgets.open.bg_stroke = egui::Stroke::new(1.0_f32, accent);
    visuals.widgets.open.fg_stroke = egui::Stroke::new(1.0_f32, accent_dark);
    visuals.widgets.open.rounding = egui::Rounding::same(9.0);

    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.text_styles = [
        (egui::TextStyle::Heading, egui::FontId::proportional(15.0)),
        (egui::TextStyle::Body, egui::FontId::proportional(12.5)),
        (egui::TextStyle::Monospace, egui::FontId::monospace(12.0)),
        (egui::TextStyle::Button, egui::FontId::proportional(12.5)),
        (egui::TextStyle::Small, egui::FontId::proportional(11.0)),
    ]
    .into();
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.spacing.interact_size.y = 24.0;
    style.spacing.slider_width = 150.0;
    ctx.set_style(style);
}

struct App {
    /// Kept so undo can rebuild image textures after restoring a snapshot.
    ctx: egui::Context,
    doc: Option<Document>,
    zoom: f32,
    pan: egui::Vec2,
    canvas_rect: Option<egui::Rect>,
    settings: CutSettings,
    show_marks: bool,
    show_art: bool,
    show_paths: bool,
    show_grid: bool,
    selected: Vec<usize>,
    tool: Tool,
    drag: Option<Drag>,
    undo: Vec<Snapshot>,
    theme_dark: bool,
    mark_size: f32,
    mark_square: f32,
    mark_stroke: f32,
    mark_inset: f32,
    mark_search_offset_top: f32,
    mark_search_offset_left: f32,
    mark_step_size: f32,
    mark_step_direction: i32,
    mark_style: RegMarkStyle,
    place_offset: egui::Vec2,
    place_scale: f32,
    status: String,
    confirm: bool,
    dry_run: bool,
    preview: bool,
    preview_progress: f32,
    hover_vertex: Option<(usize, usize)>,
    snap_grid: bool,
    snap_guides: bool,
    grid_mm: f32,
    guides: Vec<Guide>,
    space_down: bool,
    fonts: Vec<FontEntry>,
    font_index: usize,
    text: String,
    text_size: f32,
    text_origin: Option<egui::Pos2>,
    printers: Vec<String>,
    printer_index: usize,
    printer_poll: f64,
    prefs_poll: f64,
    printer_options: bool,
    bt_devices: Vec<BtDevice>,
    bt_status: String,
    bt_rx: Option<std::sync::mpsc::Receiver<Vec<BtDevice>>>,
    print_status: String,
    printer_status: String,
    print_job: Option<String>,
    print_marks: bool,
    print_paths: bool,
    print_art: bool,
    print_quality: PrintQuality,
    print_color: PrintColor,
    print_media: PrintMedia,
    trace_method: TraceMethod,
    trace_threshold: f32,
    trace_edge: f32,
    trace_simplify: f32,
    trace_min_length: f32,
    trace_min_area: f32,
    /// Speckle removal for the Studio engine, in % of the image area.
    trace_studio_noise: f32,
    /// Studio engine: keep only the outer contour of each piece.
    trace_outer_only: bool,
    /// Studio engine: bake an external offset into the traced lines.
    trace_apply_offset: bool,
    /// Last rubber-band rectangle, used by the clip in/out actions.
    last_band: Option<egui::Rect>,
    trace_smooth: u8,
    trace_separate: bool,
    /// Vectorization preview runs on a worker thread; these carry the result.
    trace_dirty: bool,
    trace_rx: Option<std::sync::mpsc::Receiver<(usize, Vec<TracedPath>)>>,
    /// Which image the auto-preview was already shown for, so it does not
    /// come back on its own after «Vetorizar» clears it.
    preview_shown_for: Option<usize>,
    offset_mm: f32,
    offset_preview: Option<OffsetPreview>,
    selected_art: bool,
    selected_art_idx: usize,
    trace_preview: Option<Vec<TracedPath>>,
    /// Ghost outline of the text being typed (tool Texto), updated live.
    text_preview: Option<Vec<Vec<egui::Pos2>>>,
    text_preview_key: (usize, String, f32, [f32; 2]),
    redo: Vec<Snapshot>,
    machines: [Machine; 2],
    machine_bt_choice: [usize; 2],
    machine_cut: [bool; 2],
    machine_rx: Option<std::sync::mpsc::Receiver<(usize, Result<Machine, String>)>>,
    /// Shown when a USB open failed with "acesso negado": one-click udev fix.
    udev_hint: bool,
    usb_poll: Option<std::time::Instant>,
    /// Whether the "Cameo detetada" hint is currently on (avoid spamming).
    usb_seen: bool,
    scan_marks: bool,
    last_scan: String,
    help_open: bool,
    lang: i18n::Lang,
    /// Beginner welcome window suppressed at the user's request.
    welcome_hidden: bool,
    clipboard_image: Option<Artwork>,
    clipboard_layer: Option<Layer>,
    clipboard_layers: Vec<Layer>,
    show_art_context_menu: bool,
    art_context_pos: Option<egui::Pos2>,
    show_layer_context_menu: bool,
    layer_context_idx: Option<usize>,
    show_selection_context_menu: bool,
    selection_context_pos: Option<egui::Pos2>,
}

/// Preview and final trace share one resolution, so the result always
/// matches what the preview showed.
const TRACE_PX: u32 = 1200;

/// Everything needed to trace one image into page coordinates. Plain data so
/// it can move to a worker thread.
struct TraceJob {
    rgba: std::sync::Arc<Vec<u8>>,
    width: u32,
    height: u32,
    max_px: u32,
    studio_noise_pct: f32,
    outer_only: bool,
    apply_offset: bool,
    offset_mm: f32,
    method: TraceMethod,
    /// Unplaced rect of the image (page mm).
    rect: egui::Rect,
    offset: egui::Vec2,
    hue: f32,
    saturation: f32,
    vibrance: f32,
    contrast: f32,
    brightness: f32,
    options: TraceOptions,
}


/// «Qualidade Studio»: vtracer (color clustering + curve fitting). The same
/// auto-invert rule as the contour engine applies, because vtracer always
/// traces dark content on a light background.
/// Otsu's threshold over the luminance histogram — separates content from
/// background without any user input.
/// Most common luminance in the image (the histogram peak) — used as a
/// fallback when the border ring is ambiguous.
fn background_luminance(rgba: &[u8]) -> f32 {
    let mut hist = [0u64; 256];
    for px in rgba.chunks_exact(4) {
        let a = f32::from(px[3]) / 255.0;
        let lum = (0.299 * f32::from(px[0]) + 0.587 * f32::from(px[1]) + 0.114 * f32::from(px[2]))
            * a
            + 255.0 * (1.0 - a);
        hist[lum.round().clamp(0.0, 255.0) as usize] += 1;
    }
    let (best, _) = hist
        .iter()
        .enumerate()
        .max_by_key(|(_, n)| **n)
        .unwrap_or((255, &0));
    best as f32
}

/// Luminance of one pixel, alpha composited over white.
fn pixel_lum(px: &[u8]) -> f32 {
    let a = f32::from(px[3]) / 255.0;
    (0.299 * f32::from(px[0]) + 0.587 * f32::from(px[1]) + 0.114 * f32::from(px[2])) * a
        + 255.0 * (1.0 - a)
}

/// Is the background brighter than `threshold`? The 2-px border ring decides:
/// it is background unless content covers most of it. Unlike corner sampling
/// this survives content touching the edge, and unlike the histogram peak it
/// survives content covering most of a dark frame (the white-on-black
/// template case, where content and background areas are similar).
fn background_is_bright(rgba: &[u8], w: u32, h: u32, threshold: u8) -> bool {
    let (w, h) = (w as usize, h as usize);
    if w == 0 || h == 0 {
        return true;
    }
    let band = 2.min(w.min(h));
    let (mut above, mut below) = (0u64, 0u64);
    let mut ring = |x: usize, y: usize| {
        let o = (y * w + x) * 4;
        if pixel_lum(&rgba[o..o + 4]) >= f32::from(threshold) {
            above += 1;
        } else {
            below += 1;
        }
    };
    for y in 0..h {
        for x in 0..w {
            if x < band || y < band || x >= w - band || y >= h - band {
                ring(x, y);
            }
        }
    }
    let total = above + below;
    if total == 0 {
        return true;
    }
    if above as f64 / total as f64 >= 0.6 {
        return true;
    }
    if below as f64 / total as f64 >= 0.6 {
        return false;
    }
    // Ambiguous border: fall back to the histogram peak.
    background_luminance(rgba) >= f32::from(threshold)
}

/// Dominant background colour. The 2-px border ring decides (it is background
/// unless content covers it) — the whole-image histogram peak would pick the
/// content colour when the content out-areas the background. Works for white,
/// black and coloured backgrounds alike, unlike any luminance rule.
fn dominant_bg_color(rgba: &[u8], w: u32, h: u32) -> [u8; 3] {
    let (w, h) = (w as usize, h as usize);
    if w == 0 || h == 0 {
        return [255, 255, 255];
    }
    let band = 2.min(w.min(h));
    let mut count = [0u64; 512];
    let mut sums = [[0u64; 3]; 512];
    let mut sample = |x: usize, y: usize, count: &mut [u64; 512], sums: &mut [[u64; 3]; 512]| {
        let o = (y * w + x) * 4;
        let px = &rgba[o..o + 4];
        let a = f32::from(px[3]) / 255.0;
        let r = (f32::from(px[0]) * a + 255.0 * (1.0 - a)) as u32;
        let g = (f32::from(px[1]) * a + 255.0 * (1.0 - a)) as u32;
        let b = (f32::from(px[2]) * a + 255.0 * (1.0 - a)) as u32;
        let key = ((r >> 5) << 6) | ((g >> 5) << 3) | (b >> 5);
        count[key as usize] += 1;
        sums[key as usize][0] += r as u64;
        sums[key as usize][1] += g as u64;
        sums[key as usize][2] += b as u64;
    };
    for y in 0..h {
        for x in 0..w {
            if x < band || y < band || x >= w - band || y >= h - band {
                sample(x, y, &mut count, &mut sums);
            }
        }
    }
    let best = (0..512)
        .max_by_key(|&k| count[k])
        .filter(|&k| count[k] > 0)
        .unwrap_or(511);
    let n = count[best].max(1);
    [
        (sums[best][0] / n) as u8,
        (sums[best][1] / n) as u8,
        (sums[best][2] / n) as u8,
    ]
}

fn otsu_threshold(rgba: &[u8]) -> u8 {
    let mut hist = [0u64; 256];
    for px in rgba.chunks_exact(4) {
        let a = f32::from(px[3]) / 255.0;
        let lum = (0.299 * f32::from(px[0]) + 0.587 * f32::from(px[1]) + 0.114 * f32::from(px[2]))
            * a
            + 255.0 * (1.0 - a);
        hist[lum.round().clamp(0.0, 255.0) as usize] += 1;
    }
    let total: u64 = hist.iter().sum();
    if total == 0 {
        return 128;
    }
    let (mut sum_all, mut sum_b, mut w_b) = (0u64, 0u64, 0u64);
    for (i, &n) in hist.iter().enumerate() {
        sum_all += i as u64 * n;
    }
    let (mut best, mut best_var) = (128u8, -1.0f64);
    for t in 0..256 {
        w_b += hist[t];
        if w_b == 0 {
            continue;
        }
        let w_f = total - w_b;
        if w_f == 0 {
            break;
        }
        sum_b += t as u64 * hist[t];
        let mu_b = sum_b as f64 / w_b as f64;
        let mu_f = (sum_all - sum_b) as f64 / w_f as f64;
        let var = w_b as f64 * w_f as f64 * (mu_b - mu_f).powi(2);
        if var > best_var {
            best_var = var;
            best = t as u8;
        }
    }
    best
}

fn run_trace_job_studio(job: &TraceJob, auto: bool) -> Vec<TracedPath> {
    let adjusted = import::adjust_rgba(
        &job.rgba,
        job.hue,
        job.saturation,
        job.vibrance,
        job.contrast,
        job.brightness,
    );
    let (rgba, tw, th) = import::downscale(&adjusted, job.width, job.height, job.max_px);
    let sx = job.rect.width() / tw.max(1) as f32;
    let sy = job.rect.height() / th.max(1) as f32;
    if !(sx.is_finite() && sx > 0.0 && sy.is_finite() && sy > 0.0) {
        return Vec::new();
    }
    let mut pixels = rgba;
    // vtracer thresholds the raw red channel and ignores alpha — flatten any
    // transparency over white first, then binarize by distance from the
    // dominant background colour. This is what lets coloured artwork (orange,
    // cyan, lavender…) trace: "dark" no longer decides, "different from the
    // background" does. Auto picks a tighter tolerance than manual.
    let bg = dominant_bg_color(&pixels, tw, th);
    let tol = if auto { 20.0 } else { 30.0 };
    for px in pixels.chunks_exact_mut(4) {
        let a = f32::from(px[3]) / 255.0;
        let r = (f32::from(px[0]) * a + 255.0 * (1.0 - a)).round() as u8;
        let g = (f32::from(px[1]) * a + 255.0 * (1.0 - a)).round() as u8;
        let b = (f32::from(px[2]) * a + 255.0 * (1.0 - a)).round() as u8;
        let d = ((f32::from(r) - f32::from(bg[0])).powi(2)
            + (f32::from(g) - f32::from(bg[1])).powi(2)
            + (f32::from(b) - f32::from(bg[2])).powi(2))
        .sqrt();
        let v = if d > tol { 0 } else { 255 };
        px[0] = v;
        px[1] = v;
        px[2] = v;
        px[3] = 255;
    }
    let img = vtracer::ColorImage {
        width: tw as usize,
        height: th as usize,
        pixels,
    };
    // Speckle threshold: vtracer SQUARS the value internally (the config
    // number is a length whose square is the patch area), so the slider
    // percentage is applied to the LENGTH scale: pct 10% removes at most
    // ~1% of the image area — small loose pieces survive.
    let total_px = tw as usize * th as usize;
    let speckle = if auto {
        // ~0.1 % of the image area.
        ((total_px as f64) / 1000.0).sqrt().round() as usize
    } else {
        ((total_px as f64).sqrt() * f64::from(job.studio_noise_pct) / 100.0).round()
            as usize
    };
    let config = vtracer::Config {
        color_mode: vtracer::ColorMode::Binary,
        hierarchical: vtracer::Hierarchical::Stacked,
        mode: visioncortex::PathSimplifyMode::Spline,
        filter_speckle: speckle.min(10_000),
        // «Suavizar»: higher keeps fewer sharp corners (more smoothing).
        corner_threshold: 45 + 15 * i32::from(job.options.smooth_iterations.min(4)),
        // Subdivision granularity for the smoothing pass (vtracer default).
        length_threshold: 4.0,
        splice_threshold: 45,
        max_iterations: 10,
        path_precision: None,
        color_precision: 6,
        layer_difference: 16,
    };
    let Ok(svg) = vtracer::convert(img, config) else {
        return Vec::new();
    };
    let origin = job.rect.min + job.offset;
    let y_ratio = sy / sx;
    let mut out = Vec::new();
    for svg_path in &svg.paths {
        for element in svg_path.path.iter() {
            // Cluster boundaries are closed loops in page-pixel space; map
            // them to millimetres exactly like the contour engine does.
            let points: Vec<(f32, f32)> = match element {
                visioncortex::CompoundPathElement::PathI32(p) => p
                    .path
                    .iter()
                    .map(|q| (q.x as f32 * sx + origin.x, q.y as f32 * sy + origin.y))
                    .collect(),
                visioncortex::CompoundPathElement::PathF64(p) => p
                    .path
                    .iter()
                    .map(|q| (q.x as f32 * sx + origin.x, q.y as f32 * sy + origin.y))
                    .collect(),
                visioncortex::CompoundPathElement::Spline(s) => flatten_cubics(&s.points, 12)
                    .iter()
                    .map(|(x, y)| (*x * sx + origin.x, *y * sy + origin.y))
                    .collect(),
            };
            if points.len() >= 3 {
                out.push(TracedPath {
                    points,
                    closed: true,
                });
            }
        }
    }
    // «Apenas contorno externo»: drop contours nested inside another one
    // (holes) — same result as Studio's outer-edge trace.
    if job.outer_only {
        let kept: Vec<bool> = out
            .iter()
            .map(|p| {
                !out.iter().any(|other| {
                    !std::ptr::eq(other, p)
                        && other.points.len() >= 3
                        && p.points.len() >= 3
                        && point_in_polygon(p.points[0], &other.points)
                })
            })
            .collect();
        let kept_iter: Vec<TracedPath> = out
            .iter()
            .zip(kept)
            .filter(|(_, k)| *k)
            .map(|(p, _)| p.clone())
            .collect();
        out = kept_iter;
    }
    // «Aplicar offset externo»: bake the offset into the traced lines.
    if job.apply_offset && job.offset_mm.abs() > 0.01 {
        let mut offset_out = Vec::new();
        for p in &out {
            for contour in offset_polygon(&p.points, job.offset_mm, 0.25) {
                if contour.len() >= 3 {
                    offset_out.push(TracedPath {
                        points: contour,
                        closed: true,
                    });
                }
            }
        }
        out = offset_out;
    }
    // «Simplif.» — full polish on the placed paths: uniform resampling,
    // corner-preserving smoothing (35° guard keeps real corners) with
    // auto-correction for jagged contours, then RDP.
    for path in &mut out {
        path.points = silhouette_core::polish(
            &path.points,
            path.closed,
            job.options.simplify_mm.max(0.05),
            35.0,
            job.options.smooth_iterations.max(1),
        );
    }
    // «Mín.» — drop short paths, same semantics as the contour engine.
    if job.options.min_length_mm > 0.0 {
        out.retain(|p| polyline_len(&p.points, p.closed) >= job.options.min_length_mm);
    }
    out
}

/// Sutherland–Hodgman: clip a closed polygon to a rectangle.
fn clip_polygon_rect(
    points: &[(f32, f32)],
    rect: egui::Rect,
) -> Vec<(f32, f32)> {
    if points.len() < 3 {
        return Vec::new();
    }
    let inside = |p: (f32, f32), edge: u8| match edge {
        0 => p.0 >= rect.min.x,
        1 => p.0 <= rect.max.x,
        2 => p.1 >= rect.min.y,
        _ => p.1 <= rect.max.y,
    };
    let intersect = |a: (f32, f32), b: (f32, f32), edge: u8| -> (f32, f32) {
        let t = match edge {
            0 => (rect.min.x - a.0) / (b.0 - a.0),
            1 => (rect.max.x - a.0) / (b.0 - a.0),
            2 => (rect.min.y - a.1) / (b.1 - a.1),
            _ => (rect.max.y - a.1) / (b.1 - a.1),
        };
        (a.0 + t * (b.0 - a.0), a.1 + t * (b.1 - a.1))
    };
    let mut poly = points.to_vec();
    for edge in 0..4 {
        let src = std::mem::take(&mut poly);
        if src.len() < 3 {
            return Vec::new();
        }
        for i in 0..src.len() {
            let cur = src[i];
            let prev = src[(i + src.len() - 1) % src.len()];
            let cur_in = inside(cur, edge);
            let prev_in = inside(prev, edge);
            if cur_in {
                if !prev_in {
                    poly.push(intersect(prev, cur, edge));
                }
                poly.push(cur);
            } else if prev_in {
                poly.push(intersect(prev, cur, edge));
            }
        }
    }
    poly
}

/// Split an open polyline into the runs inside and outside `rect`.
fn split_polyline_rect(
    points: &[(f32, f32)],
    rect: egui::Rect,
) -> (Vec<Vec<(f32, f32)>>, Vec<Vec<(f32, f32)>>) {
    let inside = |p: (f32, f32)| rect.contains(egui::pos2(p.0, p.1));
    let mut inside_runs: Vec<Vec<(f32, f32)>> = Vec::new();
    let mut outside_runs: Vec<Vec<(f32, f32)>> = Vec::new();
    let mut cur_in: Option<Vec<(f32, f32)>> = None;
    let mut cur_out: Option<Vec<(f32, f32)>> = None;
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        let (a_in, b_in) = (inside(a), inside(b));
        if a_in != b_in {
            // Crossing point on the rect boundary (axis-aligned segments and
            // the generic case both resolve via component-wise clamp search).
            let t = if (b.0 - a.0).abs() > (b.1 - a.1).abs() {
                if (b.0 - a.0).abs() > 1e-9 {
                    ((if b.0 > a.0 { rect.min.x } else { rect.max.x } - a.0) / (b.0 - a.0))
                        .clamp(0.0, 1.0)
                } else {
                    0.5
                }
            } else if (b.1 - a.1).abs() > 1e-9 {
                ((if b.1 > a.1 { rect.min.y } else { rect.max.y } - a.1) / (b.1 - a.1)).clamp(0.0, 1.0)
            } else {
                0.5
            };
            let cross = (a.0 + t * (b.0 - a.0), a.1 + t * (b.1 - a.1));
            if let Some(run) = cur_in.as_mut() {
                run.push(cross);
            }
            if let Some(run) = cur_out.as_mut() {
                run.push(cross);
            }
            if let Some(run) = cur_in.take() {
                if run.len() >= 2 {
                    inside_runs.push(run);
                }
            }
            if let Some(run) = cur_out.take() {
                if run.len() >= 2 {
                    outside_runs.push(run);
                }
            }
            let target = if b_in { &mut cur_in } else { &mut cur_out };
            *target = Some(vec![cross, b]);
        } else {
            let target = if a_in { &mut cur_in } else { &mut cur_out };
            let run = target.get_or_insert_with(Vec::new);
            if run.is_empty() {
                run.push(a);
            }
            run.push(b);
        }
    }
    if let Some(run) = cur_in.take() {
        if run.len() >= 2 {
            inside_runs.push(run);
        }
    }
    if let Some(run) = cur_out.take() {
        if run.len() >= 2 {
            outside_runs.push(run);
        }
    }
    (inside_runs, outside_runs)
}

/// Area-weighted centroid of a closed polygon (fallback: first vertex).
fn polygon_centroid(points: &[(f32, f32)]) -> (f32, f32) {
    let n = points.len();
    if n == 0 {
        return (0.0, 0.0);
    }
    let (mut area, mut cx, mut cy) = (0.0f32, 0.0f32, 0.0f32);
    for i in 0..n {
        let (x0, y0) = points[i];
        let (x1, y1) = points[(i + 1) % n];
        let cross = x0 * y1 - x1 * y0;
        area += cross;
        cx += (x0 + x1) * cross;
        cy += (y0 + y1) * cross;
    }
    if area.abs() < 1e-9 {
        return points[0];
    }
    (cx / (3.0 * area), cy / (3.0 * area))
}

/// Total polyline length in mm (closing segment included when closed).
fn polyline_len(points: &[(f32, f32)], closed: bool) -> f32 {
    let mut len = 0.0;
    for w in points.windows(2) {
        len += (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1);
    }
    if closed {
        if let (Some(first), Some(last)) = (points.first(), points.last()) {
            len += (first.0 - last.0).hypot(first.1 - last.1);
        }
    }
    len
}

/// Flatten a vtracer `Spline` (a chain of cubic Béziers, 1+3n points) into a
/// polyline — the cutter follows segments, and 12 samples per curve are far
/// below the machine's resolution.
fn flatten_cubics(points: &[visioncortex::PointF64], samples: usize) -> Vec<(f32, f32)> {
    let curves = points.len().saturating_sub(1) / 3;
    let mut out = Vec::with_capacity(curves * samples + 1);
    if let Some(first) = points.first() {
        out.push((first.x as f32, first.y as f32));
    }
    for c in 0..curves {
        let (p0, p1, p2, p3) = (
            &points[c * 3],
            &points[c * 3 + 1],
            &points[c * 3 + 2],
            &points[c * 3 + 3],
        );
        for s in 1..=samples {
            let t = f64::from(s as u32) / f64::from(samples as u32);
            let mt = 1.0 - t;
            let x = mt * mt * mt * p0.x + 3.0 * mt * mt * t * p1.x + 3.0 * mt * t * t * p2.x
                + t * t * t * p3.x;
            let y = mt * mt * mt * p0.y + 3.0 * mt * mt * t * p1.y + 3.0 * mt * t * t * p2.y
                + t * t * t * p3.y;
            out.push((x as f32, y as f32));
        }
    }
    out
}

/// Adjust, downscale, trace and place. Horizontal and vertical scales are
/// computed separately, so a stretched image no longer skews the trace.
fn run_trace_job(job: &TraceJob) -> Vec<TracedPath> {
    if job.method.is_studio_family() {
        return run_trace_job_studio(job, job.method == TraceMethod::Auto);
    }
    let adjusted = import::adjust_rgba(
        &job.rgba,
        job.hue,
        job.saturation,
        job.vibrance,
        job.contrast,
        job.brightness,
    );
    let (rgba, tw, th) = import::downscale(&adjusted, job.width, job.height, job.max_px);
    let sx = job.rect.width() / tw.max(1) as f32;
    let sy = job.rect.height() / th.max(1) as f32;
    if !(sx.is_finite() && sx > 0.0 && sy.is_finite() && sy > 0.0) {
        return Vec::new();
    }
    let mut options = job.options;
    // Coloured-art mode: the dominant colour is the background and content is
    // anything far from it — pale pieces the luminance threshold misses get
    // traced. «Limiar» maps inversely to the colour tolerance.
    options.bg_color = Some(dominant_bg_color(&rgba, tw, th));
    options.color_tolerance = (255 - options.threshold).clamp(6, 120);
    options.invert = false;
    // Refinement parameters follow the horizontal scale; the vertical axis is
    // rescaled below (single mm_per_px would skew Y on non-square pixels).
    options.mm_per_px = sx;
    let mut paths = trace_rgba(&rgba, tw, th, &options);
    let origin = job.rect.min + job.offset;
    let y_ratio = sy / sx;
    for path in &mut paths {
        for p in &mut path.points {
            p.0 += origin.x;
            p.1 = p.1 * y_ratio + origin.y;
        }
    }
    paths
}

/// UI string in the active language (PT is the source, EN translated).
/// A free function so call sites can mix `&mut self.field` with `self.lang`.
fn t(lang: i18n::Lang, pt: &'static str) -> &'static str {
    i18n::tr(lang, pt)
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let theme_dark = false;
        let ui_ctx = cc.egui_ctx.clone();
        apply_theme(&cc.egui_ctx, theme_dark);
        let fonts = import::load_fonts();
        import::register_ui_font(&cc.egui_ctx, &fonts);
        let printers = cups_printers();
        let printer_index = preferred_printer(&printers);
        let printer_status = printers
            .get(printer_index)
            .map(|n| printer_state(n))
            .unwrap_or_else(|| "sem impressora CUPS".to_string());
        let printer_options = printers
            .get(printer_index)
            .is_some_and(|n| queue_has_epson_options(n));
        // Cópia para o load de preferências depois de `printers` mover-se
        // para dentro do struct literal.
        let printer_list = printers.clone();
        let mut app = Self {
            ctx: ui_ctx,
            doc: None,
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
            canvas_rect: None,
            settings: CutSettings::default(),
            show_marks: true,
            show_art: true,
            show_paths: true,
            show_grid: true,
            selected: Vec::new(),
            tool: Tool::Select,
            drag: None,
            undo: Vec::new(),
            theme_dark,
            mark_size: 20.0,
            mark_square: 10.0,
            mark_stroke: 0.5,
            mark_inset: 15.875,
            mark_search_offset_top: 10.0,
            mark_search_offset_left: 10.0,
            mark_step_size: 0.0,
            mark_step_direction: 0,
            mark_style: RegMarkStyle::FourCorner,
            place_offset: egui::Vec2::ZERO,
            place_scale: 1.0,
            status: "Abra um ficheiro .studio3, .svg, .psd, .tiff, .pdf ou .png/.jpg/.webp"
                .to_string(),
            confirm: false,
            dry_run: false,
            preview: false,
            preview_progress: 0.0,
            hover_vertex: None,
            snap_grid: true,
            snap_guides: true,
            grid_mm: 5.0,
            guides: Vec::new(),
            space_down: false,
            fonts,
            font_index: 0,
            text: "Texto".to_string(),
            text_size: 20.0,
            text_origin: None,
            printers,
            printer_index,
            printer_poll: 0.0,
            prefs_poll: 0.0,
            printer_options,
            bt_devices: Vec::new(),
            bt_status: "Bluetooth pronto".to_string(),
            bt_rx: None,
            print_status: "sem impressão".to_string(),
            printer_status,
            print_job: None,
            print_marks: true,
            print_paths: false,
            print_art: true,
            print_quality: PrintQuality::Normal,
            print_color: PrintColor::Rgb,
            print_media: PrintMedia::Stationery,
            trace_method: TraceMethod::Auto,
            trace_threshold: 230.0,
            trace_edge: 60.0,
            trace_simplify: 0.2,
            trace_min_length: 1.0,
            trace_min_area: 2.0,
            trace_studio_noise: 0.0,
            trace_outer_only: false,
            trace_apply_offset: false,
            last_band: None,
            trace_smooth: 2,
            trace_separate: true,
            trace_dirty: false,
            trace_rx: None,
            preview_shown_for: None,
            offset_mm: 2.0,
            offset_preview: None,
            selected_art: false,
            selected_art_idx: 0,
            trace_preview: None,
            text_preview: None,
            text_preview_key: (0, String::new(), 0.0, [0.0; 2]),
            redo: Vec::new(),
            machines: [Machine::empty(), Machine::empty()],
            machine_bt_choice: [0, 0],
            machine_cut: [true, false],
            machine_rx: None,
            udev_hint: false,
            usb_poll: None,
            usb_seen: false,
            scan_marks: true,
            last_scan: "—".to_string(),
            help_open: false,
            lang: i18n::Lang::Pt,
            welcome_hidden: false,
            clipboard_image: None,
            clipboard_layer: None,
            clipboard_layers: Vec::new(),
            show_art_context_menu: false,
            art_context_pos: None,
            show_layer_context_menu: false,
            layer_context_idx: None,
            show_selection_context_menu: false,
            selection_context_pos: None,
        };
        if let Some(path) = std::env::args().nth(1) {
            app.add_files(&cc.egui_ctx, vec![PathBuf::from(path)]);
        }
        // Preferências da última sessão (traço, impressão, corte, impressora).
        prefs::load_into(&mut app, &printer_list);
        app
    }

    /// Write the current settings to `~/.config/sillinux/prefs.json`.
    fn save_prefs(&mut self) {
        if let Err(e) = prefs::save_from(self) {
            self.status = format!("preferências: {e}");
        }
    }

    fn page(&self) -> (f32, f32) {
        self.doc.as_ref().map(|d| d.page).unwrap_or((210.0, 297.0))
    }

    fn open_dialog(&mut self, ctx: &egui::Context) {
        if let Some(paths) = rfd::FileDialog::new()
            .add_filter(
                "Design",
                &[
                    "studio3", "svg", "pdf", "psd", "tif", "tiff", "png", "jpg", "jpeg", "webp",
                ],
            )
            .pick_files()
        {
            self.add_files(ctx, paths);
        }
    }

    fn add_files(&mut self, ctx: &egui::Context, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let skip = if self.doc.is_none() {
            let first = paths[0].clone();
            match Document::load(ctx, first, &self.fonts) {
                Ok(doc) => {
                    self.status = format!(
                        "{}: {} traços, {} imagens",
                        doc.path.display(),
                        doc.layers.len(),
                        doc.arts.len()
                    );
                    self.selected.clear();
                    self.offset_preview = None;
                    self.trace_preview = None;
                    self.selected_art = false;
                    self.selected_art_idx = 0;
                    self.zoom = 1.0;
                    self.pan = egui::Vec2::ZERO;
                    self.place_offset = egui::Vec2::ZERO;
                    self.place_scale = 1.0;
                    self.preview = false;
                    self.text_origin = None;
                    self.show_art = true;
                    self.show_paths = true;
                    self.show_marks = true;
                    self.undo.clear();
                    self.doc = Some(doc);
                    1
                }
                Err(e) => {
                    self.status = e;
                    return;
                }
            }
        } else {
            0
        };
        let mut arts = 0usize;
        let mut layers = 0usize;
        let mut errors = Vec::new();
        for path in paths.iter().skip(skip) {
            match self.append_import(ctx, path) {
                Ok((a, l)) => {
                    arts += a;
                    layers += l;
                }
                Err(e) => errors.push(e),
            }
        }
        if arts > 0 {
            if let Some(doc) = &self.doc {
                self.selected_art = true;
                self.selected_art_idx = doc.arts.len() - 1;
            }
            self.selected.clear();
        }
        if arts > 0 || layers > 0 {
            let extra = if errors.is_empty() {
                String::new()
            } else {
                format!(" · {} ficheiro(s) com erro", errors.len())
            };
            self.status =
                format!("adicionado ao canvas: {arts} imagem(ns), {layers} traço(s){extra}");
        } else if let Some(e) = errors.first() {
            self.status = e.clone();
        } else if skip == 0 {
            self.status = "nada a adicionar".to_string();
        }
    }

    fn append_import(
        &mut self,
        ctx: &egui::Context,
        path: &Path,
    ) -> Result<(usize, usize), String> {
        let imported = import::load(path, &self.fonts)?;
        let mut arts = 0usize;
        let mut layers = 0usize;
        let Some(doc) = self.doc.as_mut() else {
            return Ok((0, 0));
        };
        if let Some(a) = imported.art {
            let mut rect = egui::Rect::from_min_size(
                egui::pos2(a.rect_mm.0, a.rect_mm.1),
                egui::vec2(a.rect_mm.2, a.rect_mm.3),
            );
            let occupied = doc.arts.iter().fold(None::<egui::Rect>, |acc, art| {
                Some(match acc {
                    Some(r) => r.union(art.rect),
                    None => art.rect,
                })
            });
            if let Some(occ) = occupied {
                if rect.intersects(occ) {
                    let right = occ.max.x + 5.0;
                    if right + rect.width() <= doc.page.0 {
                        rect = rect.translate(egui::vec2(right - rect.min.x, 0.0));
                    } else {
                        rect = rect.translate(egui::vec2(0.0, occ.max.y + 5.0 - rect.min.y));
                    }
                }
            }
            doc.arts.push(Artwork::new(
                ctx,
                a.name.clone(),
                a.rgba,
                a.width,
                a.height,
                rect,
            ));
            doc.art_offsets.push(egui::Vec2::ZERO);
            arts += 1;
        }
        for p in &imported.paths {
            let points: Vec<egui::Pos2> = p
                .points
                .iter()
                .map(|q| egui::pos2(q.x_mm, q.y_mm))
                .collect();
            doc.layers.push(Layer {
                kind: p.kind,
                original: points.clone(),
                points,
                closed: p.closed,
                visible: true,
                group: None,
                owner: None,
                traced: false,
                art: None,
                color: LAYER_COLOR,
            });
            layers += 1;
        }
        doc.notes.extend(imported.notes);
        Ok((arts, layers))
    }

    fn update_art(&mut self, ctx: &egui::Context) {
        if let Some(doc) = &mut self.doc {
            if let Some(art) = doc.arts.get_mut(self.selected_art_idx) {
                art.refresh(ctx);
            }
        }
    }

    fn art_png(&self) -> Result<Option<Vec<u8>>, String> {
        if let Some(doc) = &self.doc {
            if let Some(art) = doc.arts.get(self.selected_art_idx) {
                return Ok(Some(art.png()?));
            }
        }
        Ok(None)
    }

    fn mark_spec(&self, page: (f32, f32)) -> RegMarkSpec {
        let mut spec = RegMarkSpec::for_page(page.0, page.1);
        spec.inset_mm = self.mark_inset;
        spec.arm_mm = self.mark_size;
        spec.square_mm = self.mark_square;
        spec.stroke_mm = self.mark_stroke;
        spec.search_offset_top_mm = self.mark_search_offset_top;
        spec.search_offset_left_mm = self.mark_search_offset_left;
        spec.step_size_mm = self.mark_step_size;
        spec.step_direction = self.mark_step_direction;
        spec.style = self.mark_style;
        spec
    }

    fn snapshot(&self) -> Snapshot {
        self.doc
            .as_ref()
            .map(|d| Snapshot {
                layers: d.layers.clone(),
                texts: d.texts.clone(),
                next_id: d.next_id,
                art_offsets: d.art_offsets.clone(),
                art_rects: d.arts.iter().map(|a| a.rect).collect(),
                arts: d
                    .arts
                    .iter()
                    .map(|a| ArtSnapshot {
                        name: a.name.clone(),
                        rgba: a.rgba.clone(),
                        width: a.width,
                        height: a.height,
                        rect: a.rect,
                        visible: a.visible,
                        hue: a.hue,
                        saturation: a.saturation,
                        vibrance: a.vibrance,
                        contrast: a.contrast,
                        brightness: a.brightness,
                    })
                    .collect(),
            })
            .unwrap_or(Snapshot {
                layers: Vec::new(),
                texts: Vec::new(),
                next_id: 1,
                art_offsets: Vec::new(),
                art_rects: Vec::new(),
                arts: Vec::new(),
            })
    }

    fn push_undo(&mut self) {
        let snap = self.snapshot();
        self.undo.push(snap);
        if self.undo.len() > 60 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn restore_snapshot(&mut self, snap: Snapshot) {
        if let Some(doc) = &mut self.doc {
            doc.layers = snap.layers;
            doc.texts = snap.texts;
            doc.next_id = snap.next_id;
            doc.art_offsets = snap.art_offsets;
            // Resurrect images when the snapshot has different ones (undo of a
            // delete); skip the texture rebuild while they are untouched.
            let unchanged = snap.arts.len() == doc.arts.len()
                && snap.arts.iter().zip(&doc.arts).all(|(s, a)| {
                    std::sync::Arc::ptr_eq(&s.rgba, &a.rgba) && s.rect == a.rect
                });
            if !unchanged {
                doc.arts = snap
                    .arts
                    .iter()
                    .map(|s| {
                        let mut art = Artwork::new(
                            &self.ctx,
                            s.name.clone(),
                            (*s.rgba).clone(),
                            s.width,
                            s.height,
                            s.rect,
                        );
                        art.visible = s.visible;
                        art.hue = s.hue;
                        art.saturation = s.saturation;
                        art.vibrance = s.vibrance;
                        art.contrast = s.contrast;
                        art.brightness = s.brightness;
                        art
                    })
                    .collect();
                doc.art_offsets = vec![egui::Vec2::ZERO; doc.arts.len()];
            } else {
                for (i, rect) in snap.art_rects.into_iter().enumerate() {
                    if i < doc.arts.len() {
                        doc.arts[i].rect = rect;
                    }
                }
            }
            self.selected.retain(|i| *i < doc.layers.len());
            self.offset_preview = None;
            self.trace_preview = None;
        }
    }

    fn undo(&mut self) {
        if let Some(snap) = self.undo.pop() {
            self.redo.push(self.snapshot());
            self.restore_snapshot(snap);
            self.status = "desfeito".to_string();
        }
    }

    fn redo(&mut self) {
        if let Some(snap) = self.redo.pop() {
            self.undo.push(self.snapshot());
            self.restore_snapshot(snap);
            self.status = "refeito".to_string();
        }
    }

    /// Ctrl+D: copy the selected image (or the selected layers) 5 mm away.
    fn duplicate_selected(&mut self) {
        if self.doc.is_none() {
            return;
        }
        self.push_undo();
        let d = egui::vec2(5.0, 5.0);
        if self.selected_art {
            if let Some(doc) = &mut self.doc {
                if let Some(mut copy) = doc.arts.get(self.selected_art_idx).cloned() {
                    let old_idx = self.selected_art_idx;
                    copy.name = if copy.name.is_empty() {
                        "imagem (cópia)".to_string()
                    } else {
                        format!("{} (cópia)", copy.name)
                    };
                    copy.rect = copy.rect.translate(d);
                    copy.visible = true;
                    let new_idx = doc.arts.len();
                    doc.arts.push(copy);
                    doc.art_offsets.push(
                        doc.art_offsets
                            .get(old_idx)
                            .copied()
                            .unwrap_or_default()
                            + d,
                    );
                    let clones: Vec<Layer> = doc
                        .layers
                        .iter()
                        .filter(|l| l.traced && l.art == Some(old_idx))
                        .cloned()
                        .collect();
                    for mut layer in clones {
                        for p in &mut layer.points {
                            *p += d;
                        }
                        layer.original = layer.points.clone();
                        layer.art = Some(new_idx);
                        doc.layers.push(layer);
                    }
                    self.selected_art_idx = new_idx;
                    self.status = "imagem duplicada (com os traços dela)".to_string();
                }
            }
        } else if !self.selected.is_empty() {
            let count = self.selected.len();
            if let Some(doc) = &mut self.doc {
                let clones: Vec<Layer> = self
                    .selected
                    .iter()
                    .filter_map(|i| doc.layers.get(*i).cloned())
                    .collect();
                let mut new_sel = Vec::new();
                for mut layer in clones {
                    for p in &mut layer.points {
                        *p += d;
                    }
                    layer.original = layer.points.clone();
                    // A cópia deixa de pertencer ao texto original.
                    layer.owner = None;
                    new_sel.push(doc.layers.len());
                    doc.layers.push(layer);
                }
                self.selected = new_sel;
            }
            self.status = format!("{count} traço(s) duplicado(s)");
        }
    }

    fn place(&self, p: egui::Pos2) -> egui::Pos2 {
        egui::pos2(
            p.x * self.place_scale + self.place_offset.x,
            p.y * self.place_scale + self.place_offset.y,
        )
    }

    fn unplace(&self, p: egui::Pos2) -> egui::Pos2 {
        egui::pos2(
            (p.x - self.place_offset.x) / self.place_scale,
            (p.y - self.place_offset.y) / self.place_scale,
        )
    }

    fn placed_paths(&self) -> Vec<CorePath> {
        let Some(doc) = &self.doc else {
            return Vec::new();
        };
        doc.layers
            .iter()
            .filter(|l| l.visible)
            .map(|l| CorePath {
                kind: l.kind,
                points: l
                    .points
                    .iter()
                    .map(|p| {
                        let q = self.place(*p);
                        Point::new(q.x, q.y)
                    })
                    .collect(),
                closed: l.closed,
                transform: None,
            })
            .collect()
    }

    fn polylines(&self) -> Vec<(Vec<(f32, f32)>, bool)> {
        let Some(doc) = &self.doc else {
            return Vec::new();
        };
        doc.layers
            .iter()
            .filter(|l| l.visible)
            .map(|l| {
                (
                    l.points
                        .iter()
                        .map(|p| {
                            let q = self.place(*p);
                            (q.x, q.y)
                        })
                        .collect::<Vec<_>>(),
                    l.closed,
                )
            })
            .collect()
    }

    fn export_svg(&mut self) {
        let Some(doc) = &self.doc else { return };
        let Some(path) = rfd::FileDialog::new()
            .set_file_name("corte.svg")
            .add_filter("SVG", &["svg"])
            .save_file()
        else {
            return;
        };
        let paths = self.placed_paths();
        let svg = silhouette_core::to_svg(&paths, doc.page.0, doc.page.1);
        self.status = match std::fs::write(&path, svg) {
            Ok(()) => format!("SVG de corte em {}", path.display()),
            Err(e) => format!("erro ao escrever SVG: {e}"),
        };
    }

    fn save_project(&mut self) {
        let Some(doc) = &self.doc else { return };
        let Some(path) = rfd::FileDialog::new()
            .set_file_name("projeto.svg")
            .add_filter("SVG", &["svg"])
            .save_file()
        else {
            return;
        };
        let png = match self.art_png() {
            Ok(png) => png,
            Err(e) => {
                self.status = e;
                return;
            }
        };
        let rect = doc.arts.get(self.selected_art_idx).map(|a| {
            let source = a.rect.translate(
                doc.art_offsets
                    .get(self.selected_art_idx)
                    .copied()
                    .unwrap_or_default(),
            );
            let r = egui::Rect::from_min_max(self.place(source.min), self.place(source.max));
            (r.min.x, r.min.y, r.width(), r.height())
        });
        let art = png.as_deref().zip(rect);
        let svg = import::project_svg(doc.page, &self.placed_paths(), art);
        self.status = match std::fs::write(&path, svg) {
            Ok(()) => format!("projeto SVG em {}", path.display()),
            Err(e) => format!("erro ao guardar: {e}"),
        };
    }

    fn sheet_pdf(&self) -> Result<Vec<u8>, String> {
        let doc = self.doc.as_ref().ok_or("sem documento")?;
        let png = self.art_png()?.unwrap_or_default();
        let images: Vec<silhouette_app::pdf::SheetImage<'_>> = if self.print_art {
            if let Some(art) = doc.arts.get(self.selected_art_idx) {
                vec![silhouette_app::pdf::SheetImage {
                    label: art.name.clone(),
                    png: &png,
                    x_mm: (art.rect.min.x
                        + doc
                            .art_offsets
                            .get(self.selected_art_idx)
                            .copied()
                            .unwrap_or_default()
                            .x)
                        * self.place_scale
                        + self.place_offset.x,
                    y_mm: (art.rect.min.y
                        + doc
                            .art_offsets
                            .get(self.selected_art_idx)
                            .copied()
                            .unwrap_or_default()
                            .y)
                        * self.place_scale
                        + self.place_offset.y,
                    width_mm: art.rect.width() * self.place_scale,
                    height_mm: art.rect.height() * self.place_scale,
                }]
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        let marks = self.mark_spec(doc.page);
        let placed = if self.print_paths {
            self.placed_paths()
        } else {
            Vec::new()
        };
        let sheet = silhouette_app::pdf::Sheet {
            page_width_mm: doc.page.0,
            page_height_mm: doc.page.1,
            marks,
            draw_marks: self.print_marks,
            paths: &placed,
            images: &images,
        };
        silhouette_app::pdf::render(&sheet)
    }

    fn print_sheet(&mut self) {
        let Some(doc) = &self.doc else { return };
        let Some(path) = rfd::FileDialog::new()
            .set_file_name("folha.pdf")
            .add_filter("PDF", &["pdf"])
            .save_file()
        else {
            return;
        };
        let page = doc.page;
        let result = self
            .sheet_pdf()
            .and_then(|pdf| std::fs::write(&path, pdf).map_err(|e| e.to_string()));
        self.status = match result {
            Ok(()) => format!(
                "folha {:.0}×{:.0} mm escrita em {}",
                page.0,
                page.1,
                path.display()
            ),
            Err(e) => format!("erro ao gerar a folha: {e}"),
        };
    }

    fn refresh_printers(&mut self) {
        let current = self.printers.get(self.printer_index).cloned();
        self.printers = cups_printers();
        let mut index = current
            .and_then(|name| self.printers.iter().position(|p| *p == name))
            .unwrap_or_else(|| preferred_printer(&self.printers))
            .min(self.printers.len().saturating_sub(1));
        if let Some(better) = best_print_queue(&self.printers) {
            if self
                .printers
                .get(index)
                .is_some_and(|p| !is_cutter_queue(p) && !queue_has_epson_options(p))
            {
                index = better;
            }
        }
        self.printer_index = index;
        self.printer_status = self
            .printers
            .get(self.printer_index)
            .map(|n| printer_state(n))
            .unwrap_or_else(|| "sem impressora CUPS".to_string());
        self.printer_options = self
            .printers
            .get(self.printer_index)
            .is_some_and(|n| queue_has_epson_options(n));
    }

    fn ensure_print_queue(&mut self) -> Option<String> {
        let mut queue = self.printers.get(self.printer_index).cloned()?;
        if is_cutter_queue(&queue) {
            self.status = format!("fila {queue} é a cortadora; escolha uma impressora");
            return None;
        }
        if !queue_has_epson_options(&queue) {
            if let Some(i) = best_print_queue(&self.printers) {
                let better = self.printers[i].clone();
                if better != queue {
                    self.print_status =
                        format!("fila {queue} sem opções do driver Epson; a usar {better}");
                    queue = better;
                    self.printer_index = i;
                    self.printer_status = printer_state(&queue);
                    self.printer_options = true;
                }
            }
        }
        Some(queue)
    }

    fn scan_bluetooth(&mut self) {
        if self.bt_rx.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(scan_bt_devices());
        });
        self.bt_rx = Some(rx);
        self.bt_status = "a procurar dispositivos Bluetooth (10 s)...".to_string();
    }

    fn pair_bluetooth(&mut self, mac: String, name: String) {
        self.bt_status = format!("a emparelhar {name}...");
        if bt_dbus::pair_connect(&mac).is_err() {
            // BlueZ indisponível: fallback ao bluetoothctl antigo.
            let _ = std::process::Command::new("bluetoothctl")
                .args(["--timeout", "12", "pair", &mac])
                .output();
            for action in ["trust", "connect"] {
                let _ = std::process::Command::new("bluetoothctl")
                    .args(["--timeout", "8", action, &mac])
                    .output();
            }
        }
        let queue = queue_name_for(&mac);
        let uri = format!("bluetooth://{mac}");
        let args = [
            "lpadmin",
            "-p",
            queue.as_str(),
            "-E",
            "-v",
            uri.as_str(),
            "-m",
            "everywhere",
        ];
        let mut attempt = std::process::Command::new("pkexec").args(args).output();
        let mut ok = attempt.as_ref().is_ok_and(|o| o.status.success());
        let mut detail = attempt
            .as_ref()
            .map(|o| String::from_utf8_lossy(&o.stderr).trim().to_string())
            .unwrap_or_default();
        if !ok {
            attempt = std::process::Command::new("lpadmin").args(args).output();
            ok = attempt.as_ref().is_ok_and(|o| o.status.success());
            detail = attempt
                .as_ref()
                .map(|o| String::from_utf8_lossy(&o.stderr).trim().to_string())
                .unwrap_or_default();
        }
        if ok {
            self.refresh_printers();
            if let Some(i) = self.printers.iter().position(|p| *p == queue) {
                self.printer_index = i;
            }
            self.bt_status = format!("{name} ligado · fila CUPS {queue}");
            self.status = self.bt_status.clone();
        } else {
            self.bt_status = if detail.is_empty() {
                format!("{name} emparelhado, mas a fila CUPS não foi criada")
            } else {
                format!("{name}: fila CUPS não criada · {detail}")
            };
            self.status = self.bt_status.clone();
        }
    }

    fn status_text(status: Status) -> &'static str {
        match status {
            Status::Ready => "pronta",
            Status::Moving => "a mover",
            Status::Unloaded => "sem mídia",
            Status::Paused => "pausada",
            Status::Cancelled => "cancelada",
            Status::Unknown(_) => "desconhecida",
        }
    }

    fn connect_usb(&mut self, slot: usize) {
        let devices = match list_cameo5() {
            Ok(devices) => devices,
            Err(e) => {
                self.status = format!("USB: {e}");
                return;
            }
        };
        let used: Vec<(u8, u8)> = self
            .machines
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != slot)
            .filter_map(|(_, m)| m.usb)
            .collect();
        let Some(pick) = devices.iter().find(|d| !used.contains(&(d.bus, d.address))) else {
            self.status = "só há uma Silhouette USB; ligue a M2 por Bluetooth".to_string();
            return;
        };
        let bus = pick.bus;
        let address = pick.address;
        self.status = format!("a ligar por USB {bus:03}:{address:03}...");
        // A abertura e as consultas vão para uma thread: a UI não congela.
        let (tx, rx) = std::sync::mpsc::channel();
        self.machine_rx = Some(rx);
        std::thread::spawn(move || {
            let result = match UsbTransport::open_at(bus, address) {
                Ok(transport) => {
                    let session =
                        Session::new(Box::new(transport) as Box<dyn Transport + Send>);
                    Ok(machine_from_transport(
                        Link::Usb,
                        format!("USB {bus:03}:{address:03}"),
                        Some((bus, address)),
                        session,
                    ))
                }
                Err(e) => Err(format!("USB {bus}:{address}: {e}")),
            };
            let _ = tx.send((slot, result));
        });
    }

    fn connect_bt(&mut self, slot: usize, mac: String, name: String) {
        self.status = format!("a ligar {name} por Bluetooth...");
        let (tx, rx) = std::sync::mpsc::channel();
        self.machine_rx = Some(rx);
        std::thread::spawn(move || {
            // Emparelhar/confiar/ligar via D-Bus (BlueZ); se o BlueZ não
            // responder, usa o bluetoothctl antigo como fallback.
            let dbus_note = match bt_dbus::pair_connect(&mac) {
                Ok(()) => None,
                Err(e) => {
                    let _ = std::process::Command::new("bluetoothctl")
                        .args(["--timeout", "12", "pair", &mac])
                        .output();
                    for action in ["trust", "connect"] {
                        let _ = std::process::Command::new("bluetoothctl")
                            .args(["--timeout", "8", action, &mac])
                            .output();
                    }
                    Some(format!("D-Bus falhou ({e}) · usado bluetoothctl"))
                }
            };
            // Socket RFCOMM direto: sem root, sem `rfcomm`, sem polkit.
            let mut opened = None;
            let mut last_err = String::from("sem resposta");
            for _ in 0..3 {
                match BtRfcommTransport::connect_m2(&mac) {
                    Ok(transport) => {
                        opened = Some(transport);
                        break;
                    }
                    Err(e) => {
                        last_err = e.to_string();
                        // A ligação ACL pode ainda estar a estabelecer-se.
                        std::thread::sleep(std::time::Duration::from_millis(500));
                    }
                }
            }
            let result = match opened {
                Some(transport) => {
                    let session =
                        Session::new(Box::new(transport) as Box<dyn Transport + Send>);
                    let mut machine = machine_from_transport(
                        Link::Bluetooth,
                        format!("{name} · {mac} · RFCOMM 1"),
                        None,
                        session,
                    );
                    if let Some(note) = dbus_note {
                        machine.detail.push_str(" · ");
                        machine.detail.push_str(&note);
                    }
                    Ok(machine)
                }
                None => Err(format!("{name}: RFCOMM falhou · {last_err}")),
            };
            let _ = tx.send((slot, result));
        });
    }

    /// One-click host setup: write the udev rule and reload udev via polkit.
    fn install_udev_rule(&mut self) {
        let tmp = "/tmp/sillinux-99-silhouette.rules";
        if let Err(e) = std::fs::write(tmp, UDEV_RULE) {
            self.status = format!("a escrever {tmp}: {e}");
            return;
        }
        let script = format!(
            "install -m644 {tmp} /etc/udev/rules.d/99-silhouette.rules \
             && udevadm control --reload-rules && udevadm trigger"
        );
        self.status = "a instalar a regra udev (confirme a senha no polkit)...".to_string();
        match std::process::Command::new("pkexec")
            .args(["sh", "-c", &script])
            .output()
        {
            Ok(out) if out.status.success() => {
                self.udev_hint = false;
                self.status =
                    "regra udev instalada — desligue e ligue o cabo USB da máquina".to_string();
            }
            Ok(out) => {
                let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
                self.status = if detail.is_empty() {
                    "instalação da regra udev cancelada".to_string()
                } else {
                    format!("regra udev falhou · {detail}")
                };
            }
            Err(e) => self.status = format!("pkexec: {e}"),
        }
    }

    fn disconnect_machine(&mut self, slot: usize) {
        self.machines[slot] = Machine::empty();
        self.status = format!("Máquina {} desligada", slot + 1);
    }

    fn refresh_machines(&mut self) {
        for slot in 0..2 {
            if self.machines[slot].session.is_none() {
                continue;
            }
            let (firmware, state) = {
                let session = self.machines[slot].session.as_mut().unwrap();
                let firmware = session
                    .firmware()
                    .unwrap_or_else(|e| format!("firmware? {e}"));
                let state = session
                    .status()
                    .map(|s| Self::status_text(s).to_string())
                    .unwrap_or_else(|e| format!("estado? {e}"));
                (firmware, state)
            };
            self.machines[slot].firmware = firmware;
            self.machines[slot].status = state;
        }
        self.status = "ligações atualizadas".to_string();
    }

    fn machine_targets(&self) -> Vec<usize> {
        (0..2)
            .filter(|i| self.machine_cut[*i] && self.machines[*i].connected())
            .collect()
    }

    fn apply_print_options(&self, cmd: &mut std::process::Command) {
        for arg in print_option_args(self.print_quality, self.print_color, self.print_media) {
            cmd.arg(arg);
        }
    }

    fn print_direct(&mut self) {
        let Some(queue) = self.ensure_print_queue() else {
            if self.printers.is_empty() {
                self.status = "sem fila de impressão (lpstat)".to_string();
            }
            return;
        };
        let pdf = match self.sheet_pdf() {
            Ok(pdf) => pdf,
            Err(e) => {
                self.status = e;
                return;
            }
        };
        let path = std::env::temp_dir().join(format!("sillinux-{}.pdf", std::process::id()));
        if let Err(e) = std::fs::write(&path, pdf) {
            self.status = format!("erro ao escrever PDF temporário: {e}");
            return;
        }
        let page = self.page();
        let mut cmd = std::process::Command::new("lp");
        cmd.args([
            "-d",
            &queue,
            "-o",
            "print-scaling=none",
            "-o",
            "fit-to-page=false",
        ]);
        if (page.0 - 210.0).abs() < 0.5 && (page.1 - 297.0).abs() < 0.5 {
            cmd.args(["-o", "media=A4"]);
        }
        self.apply_print_options(&mut cmd);
        cmd.arg(&path);
        match cmd.output() {
            Ok(out) if out.status.success() => {
                let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
                let job = raw
                    .split_whitespace()
                    .last()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                self.print_job = if job.is_empty() {
                    None
                } else {
                    Some(job.clone())
                };
                self.print_status = if raw.is_empty() {
                    format!("enviado para {queue}")
                } else {
                    format!("{queue}: {raw}")
                };
                self.status = self.print_status.clone();
                self.printer_status = printer_state(&queue);
            }
            Ok(out) => {
                self.status = format!(
                    "lp falhou em {queue}: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            Err(e) => self.status = format!("não foi possível executar lp: {e}"),
        }
    }

    fn print_calibration(&mut self) {
        let Some(queue) = self.ensure_print_queue() else {
            if self.printers.is_empty() {
                self.status = "sem fila de impressão".to_string();
            }
            return;
        };
        let page_w = 210.0;
        let page_h = 297.0;
        let mark_spec = self.mark_spec((page_w, page_h));
        let mut paths = Vec::new();

        // Outer frame at page edges
        paths.push(silhouette_core::Path {
            kind: PathKind::Rectangle,
            points: vec![
                Point {
                    x_mm: 0.0,
                    y_mm: 0.0,
                },
                Point {
                    x_mm: page_w,
                    y_mm: 0.0,
                },
                Point {
                    x_mm: page_w,
                    y_mm: page_h,
                },
                Point {
                    x_mm: 0.0,
                    y_mm: page_h,
                },
            ],
            closed: true,
            transform: None,
        });

        // Horizontal rulers: top and bottom edges with 10mm ticks
        for y_mm in [10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0, 80.0, 90.0, 100.0] {
            paths.push(silhouette_core::Path {
                kind: PathKind::Line,
                points: vec![Point { x_mm: 0.0, y_mm }, Point { x_mm: page_w, y_mm }],
                closed: false,
                transform: None,
            });
        }

        // Vertical rulers: left and right edges with 10mm ticks
        for x_mm in [
            10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0, 80.0, 90.0, 100.0, 110.0, 120.0, 130.0,
            140.0, 150.0, 160.0, 170.0, 180.0, 190.0, 200.0,
        ] {
            paths.push(silhouette_core::Path {
                kind: PathKind::Line,
                points: vec![Point { x_mm, y_mm: 0.0 }, Point { x_mm, y_mm: page_h }],
                closed: false,
                transform: None,
            });
        }

        // Known-size squares at corners for physical measurement
        let square_10 = 10.0;
        let square_20 = 20.0;
        // Top-left 10mm square
        paths.push(silhouette_core::Path {
            kind: PathKind::Rectangle,
            points: vec![
                Point {
                    x_mm: 5.0,
                    y_mm: 5.0,
                },
                Point {
                    x_mm: 5.0 + square_10,
                    y_mm: 5.0,
                },
                Point {
                    x_mm: 5.0 + square_10,
                    y_mm: 5.0 + square_10,
                },
                Point {
                    x_mm: 5.0,
                    y_mm: 5.0 + square_10,
                },
            ],
            closed: true,
            transform: None,
        });
        // Top-right 20mm square
        paths.push(silhouette_core::Path {
            kind: PathKind::Rectangle,
            points: vec![
                Point {
                    x_mm: page_w - 5.0 - square_20,
                    y_mm: 5.0,
                },
                Point {
                    x_mm: page_w - 5.0,
                    y_mm: 5.0,
                },
                Point {
                    x_mm: page_w - 5.0,
                    y_mm: 5.0 + square_20,
                },
                Point {
                    x_mm: page_w - 5.0 - square_20,
                    y_mm: 5.0 + square_20,
                },
            ],
            closed: true,
            transform: None,
        });
        // Bottom-left 20mm square
        paths.push(silhouette_core::Path {
            kind: PathKind::Rectangle,
            points: vec![
                Point {
                    x_mm: 5.0,
                    y_mm: page_h - 5.0 - square_20,
                },
                Point {
                    x_mm: 5.0 + square_20,
                    y_mm: page_h - 5.0 - square_20,
                },
                Point {
                    x_mm: 5.0 + square_20,
                    y_mm: page_h - 5.0,
                },
                Point {
                    x_mm: 5.0,
                    y_mm: page_h - 5.0,
                },
            ],
            closed: true,
            transform: None,
        });
        // Bottom-right 10mm square
        paths.push(silhouette_core::Path {
            kind: PathKind::Rectangle,
            points: vec![
                Point {
                    x_mm: page_w - 5.0 - square_10,
                    y_mm: page_h - 5.0 - square_10,
                },
                Point {
                    x_mm: page_w - 5.0,
                    y_mm: page_h - 5.0 - square_10,
                },
                Point {
                    x_mm: page_w - 5.0,
                    y_mm: page_h - 5.0,
                },
                Point {
                    x_mm: page_w - 5.0 - square_10,
                    y_mm: page_h - 5.0,
                },
            ],
            closed: true,
            transform: None,
        });

        // Center crosshairs
        paths.push(silhouette_core::Path {
            kind: PathKind::Line,
            points: vec![
                Point {
                    x_mm: page_w / 2.0 - 20.0,
                    y_mm: page_h / 2.0,
                },
                Point {
                    x_mm: page_w / 2.0 + 20.0,
                    y_mm: page_h / 2.0,
                },
            ],
            closed: false,
            transform: None,
        });
        paths.push(silhouette_core::Path {
            kind: PathKind::Line,
            points: vec![
                Point {
                    x_mm: page_w / 2.0,
                    y_mm: page_h / 2.0 - 20.0,
                },
                Point {
                    x_mm: page_w / 2.0,
                    y_mm: page_h / 2.0 + 20.0,
                },
            ],
            closed: false,
            transform: None,
        });

        let sheet = silhouette_app::pdf::Sheet {
            page_width_mm: page_w,
            page_height_mm: page_h,
            marks: mark_spec,
            draw_marks: true,
            paths: &paths,
            images: &[],
        };
        let pdf = match silhouette_app::pdf::render(&sheet) {
            Ok(pdf) => pdf,
            Err(e) => {
                self.status = format!("calibração: {e}");
                return;
            }
        };
        let file = std::env::temp_dir().join(format!("sillinux-calib-{}.pdf", std::process::id()));
        if let Err(e) = std::fs::write(&file, pdf) {
            self.status = format!("calibração: {e}");
            return;
        }
        let mut cmd = std::process::Command::new("lp");
        cmd.args([
            "-d",
            &queue,
            "-o",
            "print-scaling=none",
            "-o",
            "fit-to-page=false",
            "-o",
            "media=A4",
        ]);
        self.apply_print_options(&mut cmd);
        let out = cmd.arg(&file).output();
        match out {
            Ok(out) if out.status.success() => {
                let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
                self.print_status = format!("calibração {queue}: {raw}");
                self.status =
                    "página de calibração enviada — meça os quadrados com régua física".to_string();
                self.print_job = raw.split_whitespace().last().map(|s| s.to_string());
                self.printer_status = printer_state(&queue);
            }
            Ok(out) => {
                self.status = format!(
                    "calibração falhou em {queue}: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            Err(e) => self.status = format!("não foi possível executar lp: {e}"),
        }
    }

    fn test_print(&mut self) {
        let Some(queue) = self.ensure_print_queue() else {
            if self.printers.is_empty() {
                self.status = "sem fila de impressão".to_string();
            }
            return;
        };
        let color = color_test_rgba(1984, 567);
        let png = match import::encode_png(&color, 1984, 567) {
            Ok(png) => png,
            Err(e) => {
                self.status = format!("teste: {e}");
                return;
            }
        };
        let images = [silhouette_app::pdf::SheetImage {
            label: "teste de cor".to_string(),
            png: &png,
            x_mm: 35.0,
            y_mm: 40.0,
            width_mm: 140.0,
            height_mm: 40.0,
        }];
        let path = silhouette_core::Path {
            kind: PathKind::Rectangle,
            points: vec![
                Point {
                    x_mm: 30.0,
                    y_mm: 35.0,
                },
                Point {
                    x_mm: 180.0,
                    y_mm: 35.0,
                },
                Point {
                    x_mm: 180.0,
                    y_mm: 85.0,
                },
                Point {
                    x_mm: 30.0,
                    y_mm: 85.0,
                },
            ],
            closed: true,
            transform: None,
        };
        let mark_spec = self.mark_spec((210.0, 297.0));
        let sheet = silhouette_app::pdf::Sheet {
            page_width_mm: 210.0,
            page_height_mm: 297.0,
            marks: mark_spec,
            draw_marks: self.print_marks,
            paths: std::slice::from_ref(&path),
            images: &images,
        };
        let pdf = match silhouette_app::pdf::render(&sheet) {
            Ok(pdf) => pdf,
            Err(e) => {
                self.status = format!("teste: {e}");
                return;
            }
        };
        let file = std::env::temp_dir().join(format!("sillinux-teste-{}.pdf", std::process::id()));
        if let Err(e) = std::fs::write(&file, pdf) {
            self.status = format!("teste: {e}");
            return;
        }
        let mut cmd = std::process::Command::new("lp");
        cmd.args([
            "-d",
            &queue,
            "-o",
            "print-scaling=none",
            "-o",
            "fit-to-page=false",
            "-o",
            "media=A4",
        ]);
        self.apply_print_options(&mut cmd);
        let out = cmd.arg(&file).output();
        match out {
            Ok(out) if out.status.success() => {
                let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
                self.print_status = format!("teste {queue}: {raw}");
                self.status = self.print_status.clone();
                self.print_job = raw.split_whitespace().last().map(|s| s.to_string());
            }
            Ok(out) => {
                self.status = format!(
                    "teste falhou em {queue}: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            Err(e) => self.status = format!("teste: lp: {e}"),
        }
    }

    fn vectorize(&mut self) {
        let Some(traced) = self.trace_now(TRACE_PX) else {
            self.status = if self.doc.as_ref().and_then(|d| d.arts.first()).is_some() {
                "imagem com dimensões inválidas".to_string()
            } else if self.doc.is_some() {
                "sem imagem raster para vetorizar".to_string()
            } else {
                "abra uma imagem primeiro".to_string()
            };
            return;
        };
        let group = if self.trace_separate {
            None
        } else {
            Some(self.next_id())
        };
        self.push_undo();
        let start;
        let art_idx = self.selected_art_idx;
        {
            let Some(doc) = self.doc.as_mut() else { return };
            // Only this image's trace is replaced; other images keep theirs.
            doc.layers
                .retain(|layer| !layer.traced || layer.art != Some(art_idx));
            start = doc.layers.len();
            if traced.is_empty() {
                doc.notes
                    .push("vetorização: nenhum contorno; anterior removida".to_string());
            }
            for path in traced.iter() {
                let points: Vec<egui::Pos2> = path
                    .points
                    .iter()
                    .map(|(x, y)| egui::pos2(*x, *y))
                    .collect();
                doc.layers.push(Layer {
                    kind: PathKind::Curve,
                    original: points.clone(),
                    points,
                    closed: path.closed,
                    visible: true,
                    group,
                    owner: None,
                    traced: true,
                    // The tag marks which image produced this trace (so
                    // re-vectorizing replaces it) but the layer is born
                    // INDEPENDENT: moving the image no longer drags the vector.
                    art: Some(art_idx),
                    color: LAYER_COLOR,
                });
            }
            doc.notes.push(format!(
                "vetorização: {} caminho(s), modo {}, {}",
                traced.len(),
                match self.trace_method {
                    TraceMethod::Auto => "auto",
                    TraceMethod::Outline => "contorno",
                    TraceMethod::Edges => "bordas",
                    TraceMethod::Studio => "qualidade Studio",
                },
                if self.trace_separate {
                    "linhas separadas"
                } else {
                    "linhas agrupadas"
                }
            ));
        }
        let end = self.doc.as_ref().map_or(start, |d| d.layers.len());
        self.selected = (start..end).collect();
        self.selected_art = false;
        self.tool = Tool::Select;
        self.offset_preview = None;
        self.trace_preview = None;
        self.preview_shown_for = Some(self.selected_art_idx);
        self.status = if traced.is_empty() {
            "vetorização: nenhum contorno (ajuste o limiar); anterior removida".to_string()
        } else {
            format!(
                "vetorizado: {} caminho(s) — clique direito numa camada para separar",
                traced.len()
            )
        };
    }

    /// «Vetorizar área»: crop the selected image to the last rubber-band
    /// rectangle and trace only that region at double resolution — the way to
    /// pick up small loose details the full-image trace missed. The resulting
    /// layers are fully independent (never auto-replaced).
    fn trace_area(&mut self) {
        let Some(band) = self.last_band else {
            self.status =
                "arraste primeiro uma área no canvas (seleção retangular)".to_string();
            return;
        };
        let Some(doc) = self.doc.as_ref() else {
            self.status = "abra um ficheiro primeiro".to_string();
            return;
        };
        let Some(art) = doc.arts.get(self.selected_art_idx) else {
            self.status = "sem imagem para vetorizar".to_string();
            return;
        };
        let idx = self.selected_art_idx;
        let off = doc.art_offsets.get(idx).copied().unwrap_or_default();
        // Band (page mm) → art-local mm → pixel crop on the full-res image.
        let shift = (art.rect.min + off).to_vec2();
        let local = band.translate(-shift).intersect(art.rect);
        if !local.is_positive() {
            self.status = "a área não está sobre a imagem".to_string();
            return;
        }
        let fw = art.width as f32;
        let fh = art.height as f32;
        let rw = art.rect.width().max(1e-6);
        let rh = art.rect.height().max(1e-6);
        let x0 = ((local.min.x / rw) * fw).floor().clamp(0.0, (fw - 1.0).max(0.0)) as u32;
        let y0 = ((local.min.y / rh) * fh).floor().clamp(0.0, (fh - 1.0).max(0.0)) as u32;
        let x1 = ((local.max.x / rw) * fw).ceil().clamp(x0 as f32 + 2.0, fw) as u32;
        let y1 = ((local.max.y / rh) * fh).ceil().clamp(y0 as f32 + 2.0, fh) as u32;
        let (cw, ch) = (x1 - x0, y1 - y0);
        let mut crop = Vec::with_capacity((cw * ch * 4) as usize);
        for y in y0..y1 {
            let row = ((y * art.width + x0) * 4) as usize;
            let end = row + (cw * 4) as usize;
            crop.extend_from_slice(&art.rgba[row..end]);
        }
        // Same position mapping as a normal trace: rect = the crop in
        // art-local mm, offset = the art's placed origin.
        let job = TraceJob {
            rgba: std::sync::Arc::new(crop),
            width: cw,
            height: ch,
            max_px: 2400,
            studio_noise_pct: self.trace_studio_noise,
            outer_only: self.trace_outer_only,
            apply_offset: self.trace_apply_offset,
            offset_mm: self.offset_mm,
            method: self.trace_method,
            rect: local,
            offset: art.rect.min.to_vec2() + off,
            hue: art.hue,
            saturation: art.saturation,
            vibrance: art.vibrance,
            contrast: art.contrast,
            brightness: art.brightness,
            options: TraceOptions {
                mode: match self.trace_method {
                    TraceMethod::Outline => TraceMode::Outline,
                    TraceMethod::Edges | TraceMethod::Auto | TraceMethod::Studio => {
                        TraceMode::Edges
                    }
                },
                threshold: self.trace_threshold.clamp(0.0, 255.0) as u8,
                edge_threshold: self.trace_edge.clamp(0.0, 255.0) as u8,
                simplify_mm: if self.trace_method == TraceMethod::Auto {
                    0.1
                } else {
                    self.trace_simplify.max(0.0)
                },
                min_length_mm: if self.trace_method == TraceMethod::Auto {
                    0.5
                } else {
                    self.trace_min_length.max(0.0)
                },
                min_area_mm2: if self.trace_method == TraceMethod::Auto {
                    0.5
                } else {
                    self.trace_min_area.max(0.0)
                },
                invert: false,
                bg_color: None,
                color_tolerance: 25,
                smooth_iterations: if self.trace_method == TraceMethod::Auto {
                    2
                } else {
                    self.trace_smooth.min(4)
                },
                mm_per_px: 1.0,
            },
        };
        let traced = run_trace_job(&job);
        if traced.is_empty() {
            self.status =
                "vetorização da área: nenhum contorno (ajuste o limiar)".to_string();
            return;
        }
        self.push_undo();
        let start_idx;
        if let Some(doc) = &mut self.doc {
            start_idx = doc.layers.len();
            for path in traced.iter() {
                let points: Vec<egui::Pos2> = path
                    .points
                    .iter()
                    .map(|(x, y)| egui::pos2(*x, *y))
                    .collect();
                doc.layers.push(Layer {
                    kind: PathKind::Curve,
                    original: points.clone(),
                    points,
                    closed: path.closed,
                    visible: true,
                    group: None,
                    owner: None,
                    traced: true,
                    art: None, // area traces are never auto-replaced
                    color: LAYER_COLOR,
                });
            }
            doc.notes.push(format!(
                "vetorização de área: {} caminho(s) em {:.1}×{:.1} mm",
                traced.len(),
                local.width(),
                local.height()
            ));
        } else {
            return;
        }
        let end_idx = self.doc.as_ref().map_or(start_idx, |d| d.layers.len());
        self.selected = (start_idx..end_idx).collect();
        self.selected_art = false;
        self.tool = Tool::Select;
        self.trace_preview = None;
        self.last_band = None; // area consumed
        self.preview_shown_for = Some(self.selected_art_idx);
        self.status = format!(
            "vetorizada a área: {} caminho(s) com detalhe extra",
            end_idx - start_idx
        );
    }

    fn offset_targets(&self) -> Vec<usize> {
        let Some(doc) = self.doc.as_ref() else {
            return Vec::new();
        };
        let mut targets: Vec<usize> = if self.selected.is_empty() {
            (0..doc.layers.len())
                .filter(|i| {
                    let layer = &doc.layers[*i];
                    layer.traced && layer.visible && layer.closed && layer.owner.is_none()
                })
                .collect()
        } else {
            self.selected
                .iter()
                .copied()
                .filter(|i| *i < doc.layers.len())
                .collect()
        };
        if targets.is_empty() {
            targets = (0..doc.layers.len())
                .filter(|i| {
                    let layer = &doc.layers[*i];
                    layer.visible && layer.closed && layer.owner.is_none()
                })
                .collect();
        }
        targets
    }

    fn compute_offset(&self, external: bool) -> Option<OffsetPreview> {
        let distance = if external {
            self.offset_mm.abs()
        } else {
            -self.offset_mm.abs()
        };
        let doc = self.doc.as_ref()?;
        let targets = self.offset_targets();
        if targets.is_empty() {
            return None;
        }
        let mut contours: Vec<(usize, Vec<Vec<egui::Pos2>>)> = Vec::new();
        let mut skipped = 0usize;
        for &index in &targets {
            let layer = &doc.layers[index];
            if !layer.closed || layer.points.len() < 3 || layer.owner.is_some() {
                skipped += 1;
                continue;
            }
            let raw: Vec<(f32, f32)> = layer.points.iter().map(|p| (p.x, p.y)).collect();
            let result = offset_polygon(&raw, distance, 0.25);
            if result.is_empty() {
                skipped += 1;
                continue;
            }
            contours.push((
                index,
                result
                    .into_iter()
                    .map(|contour| contour.into_iter().map(|(x, y)| egui::pos2(x, y)).collect())
                    .collect(),
            ));
        }
        if contours.is_empty() {
            return None;
        }
        Some(OffsetPreview {
            external,
            distance,
            contours,
            skipped,
        })
    }

    fn preview_offset(&mut self, external: bool) {
        match self.compute_offset(external) {
            Some(preview) => {
                self.status = format!(
                    "pré-visualização do offset {} ({:.1} mm): {} contorno(s) — Aplicar ou Cancelar",
                    if external { "externo" } else { "interno" },
                    preview.distance.abs(),
                    preview.contours.len()
                );
                self.offset_preview = Some(preview);
            }
            None => {
                self.offset_preview = None;
                self.status = "offset: nenhum contorno fechado para alterar".to_string();
            }
        }
    }

    fn offset_from_image(&mut self, external: bool) {
        let has_traced = self
            .doc
            .as_ref()
            .map(|d| d.layers.iter().any(|l| l.traced && l.visible && l.closed))
            .unwrap_or(false);
        self.selected.clear();
        if !has_traced {
            self.trace_with_mode(if external {
                TraceMethod::Outline
            } else {
                TraceMethod::Edges
            });
        }
        self.preview_offset(external);
    }

    fn apply_offset_preview(&mut self) {
        let Some(preview) = self.offset_preview.take() else {
            self.status = "offset: nada para aplicar".to_string();
            return;
        };
        self.push_undo();
        let mut added = 0usize;
        {
            let Some(doc) = self.doc.as_mut() else { return };
            for (index, contours) in &preview.contours {
                if *index >= doc.layers.len() {
                    continue;
                }
                let base = doc.layers[*index].clone();
                let first = contours[0].clone();
                doc.layers[*index].points = first.clone();
                doc.layers[*index].original = first;
                doc.layers[*index].closed = true;
                for contour in contours.iter().skip(1) {
                    doc.layers.push(Layer {
                        kind: base.kind,
                        original: contour.clone(),
                        points: contour.clone(),
                        closed: true,
                        visible: base.visible,
                        group: base.group,
                        owner: None,
                        traced: base.traced,
                        art: base.art,
                        color: base.color,
                    });
                    added += 1;
                }
            }
        }
        self.status = format!(
            "offset {} aplicado: {} elemento(s){}",
            if preview.external {
                "externo"
            } else {
                "interno"
            },
            preview.contours.len(),
            if preview.skipped > 0 {
                format!(" · {} ignorado(s)", preview.skipped)
            } else if added > 0 {
                format!(" · {added} novo(s)")
            } else {
                String::new()
            }
        );
    }

    fn cancel_offset(&mut self) {
        if self.offset_preview.take().is_some() {
            self.status = "offset cancelado".to_string();
        }
    }

    fn delete_traced(&mut self) {
        let Some(doc) = self.doc.as_ref() else {
            self.status = "abra um ficheiro primeiro".to_string();
            return;
        };
        let count = doc.layers.iter().filter(|l| l.traced).count();
        if count == 0 {
            self.status = "não há vetorização para apagar".to_string();
            return;
        }
        self.push_undo();
        if let Some(doc) = &mut self.doc {
            doc.layers.retain(|layer| !layer.traced);
            doc.notes
                .push(format!("vetorização apagada: {count} caminho(s)"));
        }
        self.selected.clear();
        self.offset_preview = None;
        self.status = format!("vetorização apagada: {count} caminho(s)");
    }

    fn run_cut(&mut self) {
        self.save_prefs();
        let Some(doc) = self.doc.as_ref() else { return };
        let page = doc.page;
        let polylines = order_for_cut(self.polylines());
        if polylines.is_empty() {
            self.status = "nada para cortar".to_string();
            return;
        }
        let spec = self.mark_spec(page);
        let mapping = if self.scan_marks {
            CutMapping::new(page.1).with_origin(spec.inset_mm, spec.inset_mm)
        } else {
            CutMapping::new(page.1)
        };
        let prelude = polyline_prelude(self.settings);
        let path_jobs: Vec<_> = polylines
            .iter()
            .filter(|(points, _)| points.len() >= 2)
            .map(|path| polyline_paths(std::slice::from_ref(path), mapping))
            .filter(|job| !job.is_empty())
            .collect();
        let epilogue = polyline_epilogue();
        let scan = self.scan_marks.then(|| RegMarkScan {
            arm_su: (spec.arm_mm * 20.0).round() as i32,
            stroke_su: (spec.stroke_mm * 20.0).round() as i32,
            height_su: (spec.mark_height_mm() * 20.0).round() as i32,
            width_su: (spec.mark_width_mm() * 20.0).round() as i32,
            top_su: (spec.scan_margin_top_mm() * 20.0).round() as i32,
            left_su: (spec.scan_margin_left_mm() * 20.0).round() as i32,
            quad: false,
        });
        cut_log(&format!(
            "cut page {}x{} marks arm {} stroke {} inset {} scan {}",
            page.0,
            page.1,
            spec.arm_mm,
            spec.stroke_mm,
            spec.inset_mm,
            if scan.is_some() { "on" } else { "off" }
        ));
        if let Some(scan) = &scan {
            cut_log(&format!(
                "scan TB123,{},{},{},{} arm {} stroke {}",
                scan.height_su,
                scan.width_su,
                scan.top_su,
                scan.left_su,
                scan.arm_su,
                scan.stroke_su
            ));
        }
        if self.dry_run {
            let points: usize = polylines.iter().map(|(p, _)| p.len()).sum();
            let path_commands: usize = path_jobs.iter().map(|job| job.len()).sum();
            let commands = prelude.len()
                + scan.as_ref().map_or(0, |scan| scan.commands().len())
                + path_commands
                + epilogue.len();
            for job in &path_jobs {
                if let (Some(first), Some(last)) = (job.commands().first(), job.commands().last()) {
                    cut_log(&format!(
                        "dry path {} cmds from {} to {}",
                        job.len(),
                        command_text(first),
                        command_text(last)
                    ));
                }
            }
            let first = path_jobs
                .first()
                .and_then(|job| job.commands().first())
                .map(|cmd| command_text(cmd))
                .unwrap_or_default();
            self.status = format!(
                "dry-run: {} comandos, {} traços, {} pontos, marcas {} · primeiro {}",
                commands,
                polylines.len(),
                points,
                if scan.is_some() { "sim" } else { "não" },
                first
            );
            return;
        }
        if path_jobs.is_empty() {
            self.status = "nada para cortar: os traços não têm pontos suficientes".to_string();
            return;
        }
        let chosen: Vec<usize> = (0..2).filter(|i| self.machine_cut[*i]).collect();
        if chosen.is_empty() {
            self.status = "escolha M1, M2 ou ambas para cortar".to_string();
            return;
        }
        let targets = self.machine_targets();
        if targets.len() != chosen.len() {
            let missing: Vec<String> = chosen
                .iter()
                .filter(|i| !self.machines[**i].connected())
                .map(|i| format!("M{}", i + 1))
                .collect();
            self.status = if missing.is_empty() {
                "ligue a Silhouette 1 ou 2 (USB ou Bluetooth)".to_string()
            } else {
                format!(
                    "ligue {} antes de cortar (ou desmarque a máquina)",
                    missing.join(" e ")
                )
            };
            return;
        }
        let mut done = 0usize;
        let mut errors = Vec::new();
        for slot in targets {
            let Some(session) = self.machines[slot].session.as_mut() else {
                continue;
            };
            if let Err(e) = session.initialize() {
                self.machines[slot].status = format!("erro: {e}");
                errors.push(format!("M{}: {e}", slot + 1));
                continue;
            }
            if let Err(e) = session.run(&prelude) {
                self.machines[slot].status = format!("erro: {e}");
                errors.push(format!("M{}: {e}", slot + 1));
                continue;
            }
            if let Err(e) = session.wait_idle(15_000) {
                self.machines[slot].status = format!("prelúdio: {e}");
                errors.push(format!("M{} prelúdio: {e}", slot + 1));
                continue;
            }
            if let Some(scan) = &scan {
                self.machines[slot].status = "a ler marcas de registo".to_string();
                match session.scan_regmarks_reply(scan, 45_000) {
                    Ok(reply) => {
                        self.last_scan = reply.clone();
                        cut_log(&format!("M{} scan ok: {reply}", slot + 1));
                        if let Err(e) = session.wait_idle(45_000) {
                            self.machines[slot].status = format!("marcas: {e}");
                            errors.push(format!("M{} marcas: {e}", slot + 1));
                            continue;
                        }
                    }
                    Err(e) => {
                        self.last_scan = e.to_string();
                        cut_log(&format!("M{} scan error: {e}", slot + 1));
                        self.machines[slot].status = format!("marcas: {e}");
                        errors.push(format!("M{} marcas: {e}", slot + 1));
                        continue;
                    }
                }
                self.machines[slot].status = "marcas OK · a cortar".to_string();
            } else {
                self.machines[slot].status = "a cortar".to_string();
            }
            let (area_h, area_w) = if scan.is_some() {
                (
                    (spec.mark_height_mm() * 20.0).round() as i32,
                    (spec.mark_width_mm() * 20.0).round() as i32,
                )
            } else {
                (
                    (page.1 * 20.0).round() as i32,
                    (page.0 * 20.0).round() as i32,
                )
            };
            let area = polyline_area(area_h, area_w, self.settings.tool);
            cut_log(&format!("area \\0,0 Z{area_h},{area_w}"));
            if let Err(e) = session.run(&area) {
                self.machines[slot].status = format!("área: {e}");
                errors.push(format!("M{} área: {e}", slot + 1));
                continue;
            }
            let _ = session.wait_idle(10_000);
            let mut result = Ok(());
            for (index, path_job) in path_jobs.iter().enumerate() {
                self.machines[slot].status =
                    format!("a cortar · elemento {}/{}", index + 1, path_jobs.len());
                if let (Some(first), Some(last)) =
                    (path_job.commands().first(), path_job.commands().last())
                {
                    cut_log(&format!(
                        "M{} element {}/{} {} cmds from {} to {}",
                        slot + 1,
                        index + 1,
                        path_jobs.len(),
                        path_job.len(),
                        command_text(first),
                        command_text(last)
                    ));
                }
                if let Err(e) = session.run_paced(path_job) {
                    result = Err(e);
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
                let _ = session.wait_idle(10_000);
            }
            if result.is_ok() {
                if let Err(e) = session.run(&epilogue) {
                    result = Err(e);
                }
            } else {
                let _ = session.run(&epilogue);
            }
            let _ = session.wait_idle(10_000);
            match result {
                Ok(()) => {
                    self.machines[slot].status = format!(
                        "corte concluído · {} elemento(s){}",
                        path_jobs.len(),
                        if scan.is_some() { " · marcas OK" } else { "" }
                    );
                    done += 1;
                }
                Err(e) => {
                    self.machines[slot].status = format!("falha: {e}");
                    errors.push(format!("M{}: {e}", slot + 1));
                }
            }
        }
        self.status = if errors.is_empty() {
            format!(
                "corte concluído em {done} máquina(s): {} traços",
                path_jobs.len()
            )
        } else {
            format!("corte: {done} ok · {}", errors.join(" · "))
        };
    }

    fn test_marks(&mut self) {
        let page = self.page();
        let spec = self.mark_spec(page);
        let scan = RegMarkScan {
            arm_su: (spec.arm_mm * 20.0).round() as i32,
            stroke_su: (spec.stroke_mm * 20.0).round() as i32,
            height_su: (spec.mark_height_mm() * 20.0).round() as i32,
            width_su: (spec.mark_width_mm() * 20.0).round() as i32,
            top_su: (spec.scan_margin_top_mm() * 20.0).round() as i32,
            left_su: (spec.scan_margin_left_mm() * 20.0).round() as i32,
            quad: false,
        };
        let Some(slot) = self.machine_targets().first().copied() else {
            self.status = "ligue a Silhouette 1 ou 2 antes de testar as marcas".to_string();
            return;
        };
        cut_log(&format!(
            "scan test TB123,{},{},{},{} arm {} stroke {}",
            scan.height_su, scan.width_su, scan.top_su, scan.left_su, scan.arm_su, scan.stroke_su
        ));
        let prelude = polyline_prelude(self.settings);
        let Some(session) = self.machines[slot].session.as_mut() else {
            return;
        };
        if let Err(e) = session.initialize() {
            self.status = format!("marcas: {e}");
            return;
        }
        if let Err(e) = session.run(&prelude) {
            self.status = format!("marcas: {e}");
            return;
        }
        let _ = session.wait_idle(15_000);
        self.machines[slot].status = "a ler marcas de registo".to_string();
        let reply = session.scan_regmarks_reply(&scan, 45_000);
        let _ = session.wait_idle(45_000);
        let _ = session.run(&polyline_epilogue());
        let _ = session.wait_idle(10_000);
        match reply {
            Ok(text) => {
                self.last_scan = text.clone();
                self.machines[slot].status = format!("marcas OK · {text}");
                self.status = format!("marcas lidas: {text}");
                cut_log(&format!("scan test ok: {text}"));
            }
            Err(e) => {
                self.last_scan = e.to_string();
                self.machines[slot].status = format!("marcas: {e}");
                self.status = format!("marcas: {e}");
                cut_log(&format!("scan test error: {e}"));
            }
        }
    }

    fn hit_layer(&self, mm: egui::Pos2, tol_mm: f32) -> Option<usize> {
        let doc = self.doc.as_ref()?;
        for (i, layer) in doc.layers.iter().enumerate().rev() {
            if !layer.visible {
                continue;
            }
            let pts: Vec<egui::Pos2> = layer.points.iter().map(|p| self.place(*p)).collect();
            if pts.len() == 1 {
                if (pts[0] - mm).length() <= tol_mm {
                    return Some(i);
                }
                continue;
            }
            let segments = pts.len() - 1 + usize::from(layer.closed && pts.len() > 2);
            for s in 0..segments {
                let a = pts[s];
                let b = pts[(s + 1) % pts.len()];
                if dist_to_segment(mm, a, b) <= tol_mm {
                    return Some(i);
                }
            }
        }
        None
    }

    fn hit_vertex(&self, layer_index: usize, mm: egui::Pos2, tol_mm: f32) -> Option<usize> {
        let doc = self.doc.as_ref()?;
        let layer = doc.layers.get(layer_index)?;
        let mut best = None;
        let mut best_d = tol_mm;
        for (i, p) in layer.points.iter().enumerate() {
            let d = (self.place(*p) - mm).length();
            if d <= best_d {
                best_d = d;
                best = Some(i);
            }
        }
        best
    }

    fn add_vertex(&mut self, layer_index: usize, mm: egui::Pos2, tol_mm: f32) -> bool {
        let Some(doc) = &self.doc else {
            return false;
        };
        let Some(layer) = doc.layers.get(layer_index) else {
            return false;
        };
        let pts: Vec<egui::Pos2> = layer.points.iter().map(|p| self.place(*p)).collect();
        let segments = pts.len().saturating_sub(1) + usize::from(layer.closed && pts.len() > 2);
        let mut best = (f32::INFINITY, 0usize, egui::Pos2::ZERO);
        for s in 0..segments {
            let a = pts[s];
            let b = pts[(s + 1) % pts.len()];
            let q = closest_on_segment(mm, a, b);
            let d = (q - mm).length();
            if d < best.0 {
                best = (d, s, q);
            }
        }
        if best.0 > tol_mm {
            return false;
        }
        self.push_undo();
        let p = self.unplace(best.2);
        if let Some(doc) = &mut self.doc {
            if let Some(layer) = doc.layers.get_mut(layer_index) {
                layer.points.insert(best.1 + 1, p);
            }
        }
        true
    }

    fn delete_vertex(&mut self, layer_index: usize, vertex: usize) {
        self.push_undo();
        if let Some(doc) = &mut self.doc {
            if let Some(layer) = doc.layers.get_mut(layer_index) {
                if layer.points.len() > 2 {
                    layer.points.remove(vertex);
                }
            }
        }
    }

    fn only_selected(&self) -> Option<usize> {
        if self.selected.len() == 1 {
            self.selected.first().copied()
        } else {
            None
        }
    }

    fn next_id(&mut self) -> u64 {
        if let Some(doc) = &mut self.doc {
            let id = doc.next_id;
            doc.next_id += 1;
            id
        } else {
            1
        }
    }

    fn group_members(&self, index: usize) -> Vec<usize> {
        let Some(doc) = &self.doc else {
            return Vec::new();
        };
        let Some(layer) = doc.layers.get(index) else {
            return Vec::new();
        };
        match layer.group {
            Some(group) => doc
                .layers
                .iter()
                .enumerate()
                .filter(|(_, l)| l.group == Some(group))
                .map(|(i, _)| i)
                .collect(),
            None => vec![index],
        }
    }

    fn expand_selection(&mut self) {
        let Some(doc) = &self.doc else {
            return;
        };
        let mut groups = Vec::new();
        for i in &self.selected {
            if let Some(group) = doc.layers.get(*i).and_then(|l| l.group) {
                if !groups.contains(&group) {
                    groups.push(group);
                }
            }
        }
        if groups.is_empty() {
            return;
        }
        for (i, layer) in doc.layers.iter().enumerate() {
            if layer.group.is_some_and(|g| groups.contains(&g)) && !self.selected.contains(&i) {
                self.selected.push(i);
            }
        }
        self.selected.sort_unstable();
    }

    fn select_indices(&mut self, indices: Vec<usize>, additive: bool) {
        self.selected_art = false;
        if additive {
            for i in indices {
                if !self.selected.contains(&i) {
                    self.selected.push(i);
                }
            }
        } else {
            self.selected = indices;
        }
        self.expand_selection();
        self.selected.sort_unstable();
    }

    fn selection_units(&self) -> Vec<Vec<usize>> {
        let Some(doc) = &self.doc else {
            return Vec::new();
        };
        let mut units: Vec<Vec<usize>> = Vec::new();
        let mut seen_groups: Vec<u64> = Vec::new();
        for index in &self.selected {
            let group = doc.layers.get(*index).and_then(|l| l.group);
            match group {
                Some(g) => {
                    if seen_groups.contains(&g) {
                        continue;
                    }
                    seen_groups.push(g);
                    let members: Vec<usize> = doc
                        .layers
                        .iter()
                        .enumerate()
                        .filter(|(_, l)| l.group == Some(g))
                        .map(|(i, _)| i)
                        .collect();
                    units.push(members);
                }
                None => units.push(vec![*index]),
            }
        }
        units
    }

    fn text_font(&self, name: &str) -> Option<&FontEntry> {
        self.fonts
            .iter()
            .find(|f| f.name == name)
            .or_else(|| self.fonts.first())
    }

    fn rebuild_text(&mut self, id: u64) {
        let Some(doc) = &self.doc else {
            return;
        };
        let Some(item) = doc.texts.iter().find(|t| t.id == id).cloned() else {
            return;
        };
        let Some(font) = self.text_font(&item.font) else {
            self.status = "fonte indisponível".to_string();
            return;
        };
        let paths =
            match import::text_paths(font, &item.text, item.size, (item.origin.x, item.origin.y)) {
                Ok(paths) => paths,
                Err(e) => {
                    self.status = e;
                    return;
                }
            };
        let selected_before = doc
            .layers
            .iter()
            .enumerate()
            .any(|(i, l)| l.owner == Some(id) && self.selected.contains(&i));
        let old: Vec<usize> = doc
            .layers
            .iter()
            .enumerate()
            .filter(|(_, l)| l.owner == Some(id))
            .map(|(i, _)| i)
            .collect();
        self.push_undo();
        if let Some(doc) = &mut self.doc {
            for i in old.iter().rev() {
                if *i < doc.layers.len() {
                    doc.layers.remove(*i);
                }
            }
            let mut fresh = Vec::new();
            for path in paths {
                let raw: Vec<egui::Pos2> = path
                    .points
                    .iter()
                    .map(|q| egui::pos2(q.x_mm, q.y_mm))
                    .collect();
                let points = raw.iter().map(|p| item.affine.apply(*p)).collect();
                doc.layers.push(Layer {
                    kind: path.kind,
                    original: raw,
                    points,
                    closed: path.closed,
                    visible: true,
                    group: item.group,
                    owner: Some(id),
                    traced: false,
                    art: None,
                    color: LAYER_COLOR,
                });
                fresh.push(doc.layers.len() - 1);
            }
            if selected_before {
                self.selected = fresh;
            } else {
                self.selected.retain(|i| *i < doc.layers.len());
            }
        }
        self.status = "texto atualizado".to_string();
    }

    fn apply_text(&mut self, id: u64, text: String, font: String, size: f32) {
        let Some(doc) = &mut self.doc else {
            return;
        };
        let Some(item) = doc.texts.iter_mut().find(|t| t.id == id) else {
            return;
        };
        if item.text == text && item.font == font && (item.size - size).abs() < 1e-4 {
            return;
        }
        item.text = text;
        item.font = font;
        item.size = size.max(1.0);
        self.rebuild_text(id);
    }

    fn convert_text(&mut self, id: u64) {
        let Some(doc) = &mut self.doc else {
            return;
        };
        let Some(pos) = doc.texts.iter().position(|t| t.id == id) else {
            return;
        };
        self.push_undo();
        if let Some(doc) = &mut self.doc {
            doc.texts.remove(pos);
            for layer in &mut doc.layers {
                if layer.owner == Some(id) {
                    layer.owner = None;
                }
            }
        }
        self.status = "texto convertido em caminho".to_string();
    }

    fn delete_text(&mut self, id: u64) {
        let Some(doc) = &self.doc else {
            return;
        };
        let old: Vec<usize> = doc
            .layers
            .iter()
            .enumerate()
            .filter(|(_, l)| l.owner == Some(id))
            .map(|(i, _)| i)
            .collect();
        self.push_undo();
        if let Some(doc) = &mut self.doc {
            for i in old.iter().rev() {
                if *i < doc.layers.len() {
                    doc.layers.remove(*i);
                }
            }
            doc.texts.retain(|t| t.id != id);
        }
        self.selected.clear();
        self.status = "texto apagado".to_string();
    }

    fn group_selected(&mut self) {
        if self.selected.len() < 2 {
            self.status = "selecione dois ou mais elementos".to_string();
            return;
        }
        self.push_undo();
        let id = self.next_id();
        let selected = self.selected.clone();
        if let Some(doc) = &mut self.doc {
            for (i, layer) in doc.layers.iter_mut().enumerate() {
                if selected.contains(&i) {
                    layer.group = Some(id);
                }
            }
            for text in &mut doc.texts {
                if doc
                    .layers
                    .iter()
                    .enumerate()
                    .any(|(i, l)| l.owner == Some(text.id) && selected.contains(&i))
                {
                    text.group = Some(id);
                }
            }
        }
        self.status = "elementos agrupados".to_string();
    }

    /// «Arredondar»: simplify the selected lines the rounded way — uniform
    /// resampling + corner-preserving smoothing + RDP, so curves stay smooth
    /// instead of angular. Tolerance comes from the «Simplif.» slider.
    fn round_selection(&mut self) {
        if self.selected.is_empty() {
            self.status = "selecione as linhas a arredondar".to_string();
            return;
        }
        self.push_undo();
        let tolerance = self.trace_simplify.max(0.15);
        let selected = self.selected.clone();
        let mut count = 0usize;
        if let Some(doc) = &mut self.doc {
            for i in &selected {
                if let Some(layer) = doc.layers.get_mut(*i) {
                    let pts: Vec<(f32, f32)> =
                        layer.points.iter().map(|p| (p.x, p.y)).collect();
                    let rounded = silhouette_core::polish(&pts, layer.closed, tolerance, 35.0, 2);
                    if rounded.len() >= 2 {
                        layer.points =
                            rounded.iter().map(|(x, y)| egui::pos2(*x, *y)).collect();
                        layer.original = layer.points.clone();
                        count += 1;
                    }
                }
            }
        }
        self.offset_preview = None;
        self.status = format!("arredondada(s) {count} linha(s) a {tolerance:.2} mm");
    }

    /// «Separar objetos»: remove the group id from every layer sharing the
    /// group of the given layer, so each contour becomes individually
    /// selectable.
    fn split_group_of(&mut self, idx: usize) {
        let Some(group) = self
            .doc
            .as_ref()
            .and_then(|d| d.layers.get(idx))
            .and_then(|l| l.group)
        else {
            return;
        };
        self.push_undo();
        if let Some(doc) = &mut self.doc {
            for l in doc.layers.iter_mut() {
                if l.group == Some(group) {
                    l.group = None;
                }
            }
        }
        self.status = "vetor separado em objetos individuais".to_string();
    }

    /// Keep only the parts of the selected lines inside (`keep_inside`) or
    /// outside the last rubber-band rectangle — per-line crop for traces.
    fn clip_layers_to_selection(&mut self, keep_inside: bool) {
        let Some(rect) = self.last_band else {
            self.status = "faça primeiro uma seleção retangular (arraste no vazio)".to_string();
            return;
        };
        if self.selected.is_empty() {
            self.status = "selecione as linhas a recortar".to_string();
            return;
        }
        self.push_undo();
        let selected = self.selected.clone();
        let mut removed = 0usize;
        let mut clipped = 0usize;
        // (layer index, replacement pieces; empty = remove the layer)
        let mut plan: Vec<(usize, Vec<Layer>)> = Vec::new();
        if let Some(doc) = &mut self.doc {
            for i in &selected {
                let Some(layer) = doc.layers.get(*i).cloned() else {
                    continue;
                };
                if layer.closed {
                    if keep_inside {
                        let src_pts: Vec<(f32, f32)> =
                            layer.points.iter().map(|p| (p.x, p.y)).collect();
                        let clipped_pts = clip_polygon_rect(&src_pts, rect);
                        if clipped_pts.len() >= 3 {
                            let mut l = layer.clone();
                            l.points =
                                clipped_pts.iter().map(|(x, y)| egui::pos2(*x, *y)).collect();
                            l.original = l.points.clone();
                            plan.push((*i, vec![l]));
                            clipped += 1;
                        } else {
                            plan.push((*i, Vec::new()));
                            removed += 1;
                        }
                    } else {
                        let c = polygon_centroid(
                            &layer.points.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>(),
                        );
                        if rect.contains(egui::pos2(c.0, c.1)) {
                            plan.push((*i, Vec::new()));
                            removed += 1;
                        }
                        // Partly outside: there is nothing sensible to remove
                        // from a closed contour without splitting it, so it
                        // stays.
                    }
                } else {
                    let src_pts: Vec<(f32, f32)> =
                        layer.points.iter().map(|p| (p.x, p.y)).collect();
                    let (inside, outside) = split_polyline_rect(&src_pts, rect);
                    let keep = if keep_inside { inside } else { outside };
                    if keep.is_empty() {
                        plan.push((*i, Vec::new()));
                        removed += 1;
                        continue;
                    }
                    let mut pieces = Vec::new();
                    for pts in keep {
                        let mut l = layer.clone();
                        l.points = pts.iter().map(|(x, y)| egui::pos2(*x, *y)).collect();
                        l.original = l.points.clone();
                        l.closed = false;
                        l.group = None;
                        l.owner = None;
                        pieces.push(l);
                    }
                    clipped += 1;
                    plan.push((*i, pieces));
                }
            }
            // Apply the plan: first piece replaces in place, extras are
            // appended, empties are dropped.
            let mut extra: Vec<Layer> = Vec::new();
            let mut to_remove: Vec<usize> = Vec::new();
            for (i, pieces) in &plan {
                if pieces.is_empty() {
                    to_remove.push(*i);
                    continue;
                }
                if let Some(slot) = doc.layers.get_mut(*i) {
                    *slot = pieces[0].clone();
                }
                for p in pieces.iter().skip(1) {
                    extra.push(p.clone());
                }
            }
            for p in extra {
                doc.layers.push(p);
            }
            if !to_remove.is_empty() {
                let rm: std::collections::HashSet<usize> = to_remove.into_iter().collect();
                doc.layers = doc
                    .layers
                    .drain(..)
                    .enumerate()
                    .filter_map(|(i, l)| if rm.contains(&i) { None } else { Some(l) })
                    .collect();
            }
        }
        self.selected.clear();
        self.offset_preview = None;
        self.status = format!("recorte: {clipped} linha(s) ajustada(s), {removed} removida(s)");
    }

    fn ungroup_selected(&mut self) {
        if self.selected.is_empty() {
            return;
        }
        self.push_undo();
        let selected = self.selected.clone();
        if let Some(doc) = &mut self.doc {
            for (i, layer) in doc.layers.iter_mut().enumerate() {
                if selected.contains(&i) {
                    layer.group = None;
                }
            }
            for text in &mut doc.texts {
                if doc
                    .layers
                    .iter()
                    .enumerate()
                    .any(|(i, l)| l.owner == Some(text.id) && selected.contains(&i))
                {
                    text.group = None;
                }
            }
        }
        self.status = "elementos desagrupados".to_string();
    }

    fn touch_texts(&mut self, subset: &[usize], g: Affine) {
        let Some(doc) = &mut self.doc else {
            return;
        };
        let mut owners: Vec<(u64, Vec<usize>)> = Vec::new();
        for (i, layer) in doc.layers.iter().enumerate() {
            let Some(owner) = layer.owner else {
                continue;
            };
            if let Some(entry) = owners.iter_mut().find(|(o, _)| *o == owner) {
                entry.1.push(i);
            } else {
                owners.push((owner, vec![i]));
            }
        }
        for text in &mut doc.texts {
            let Some((_, layers)) = owners.iter().find(|(o, _)| *o == text.id) else {
                continue;
            };
            if layers.iter().all(|i| subset.contains(i)) {
                text.affine = text.affine.then(&g);
            }
        }
    }

    fn placement_affine(&self, linear: Affine, anchor: egui::Pos2) -> Affine {
        let unplaced = self.unplace(anchor);
        let rotated = linear.linear(self.place_offset - anchor.to_vec2()) / self.place_scale;
        let offset = unplaced.to_vec2() + rotated;
        Affine {
            a: linear.a,
            b: linear.b,
            c: linear.c,
            d: linear.d,
            e: offset.x,
            f: offset.y,
        }
    }

    fn translate_selected(&mut self, dx: f32, dy: f32) {
        if self.selected.is_empty() {
            return;
        }
        let d = egui::vec2(dx, dy) / self.place_scale;
        let selected = self.selected.clone();
        if let Some(doc) = &mut self.doc {
            for (i, layer) in doc.layers.iter_mut().enumerate() {
                if selected.contains(&i) {
                    for p in &mut layer.points {
                        *p += d;
                    }
                }
            }
        }
        self.touch_texts(&selected, Affine::translate(d.x, d.y));
        self.expand_selection();
    }

    fn align_page(&mut self, align: Align) {
        if self.selected_art {
            let Some(bbox) = self.art_bbox() else {
                return;
            };
            let page = self.page();
            let (dx, dy) = match align {
                Align::Left => (-bbox.min.x, 0.0),
                Align::CenterH => (page.0 * 0.5 - bbox.center().x, 0.0),
                Align::Right => (page.0 - bbox.max.x, 0.0),
                Align::Top => (0.0, -bbox.min.y),
                Align::CenterV => (0.0, page.1 * 0.5 - bbox.center().y),
                Align::Bottom => (0.0, page.1 - bbox.max.y),
            };
            if dx.abs() < 1e-4 && dy.abs() < 1e-4 {
                return;
            }
            self.push_undo();
            self.translate_art(dx, dy);
            self.status = "imagem alinhada à página".to_string();
            return;
        }
        let Some(bbox) = self.selection_bbox() else {
            return;
        };
        let page = self.page();
        let (dx, dy) = match align {
            Align::Left => (-bbox.min.x, 0.0),
            Align::CenterH => (page.0 * 0.5 - bbox.center().x, 0.0),
            Align::Right => (page.0 - bbox.max.x, 0.0),
            Align::Top => (0.0, -bbox.min.y),
            Align::CenterV => (0.0, page.1 * 0.5 - bbox.center().y),
            Align::Bottom => (0.0, page.1 - bbox.max.y),
        };
        self.push_undo();
        self.translate_selected(dx, dy);
        self.status = "alinhado à página".to_string();
    }

    fn center_on_canvas(&mut self) {
        if self.selected_art {
            let Some(bbox) = self.art_bbox() else {
                return;
            };
            let page = self.page();
            self.push_undo();
            self.translate_art(
                page.0 * 0.5 - bbox.center().x,
                page.1 * 0.5 - bbox.center().y,
            );
            self.status = "imagem centrada no canvas".to_string();
            return;
        }
        let Some(bbox) = self.selection_bbox() else {
            return;
        };
        let page = self.page();
        let dx = page.0 * 0.5 - bbox.center().x;
        let dy = page.1 * 0.5 - bbox.center().y;
        self.push_undo();
        self.translate_selected(dx, dy);
        self.status = "centrado no canvas".to_string();
    }

    fn align_relative(&mut self, align: Align) {
        let Some(whole) = self.selection_bbox() else {
            return;
        };
        let units = self.selection_units();
        if units.len() < 2 {
            self.status = "selecione dois ou mais elementos".to_string();
            return;
        }
        self.push_undo();
        for unit in units {
            let Some(unit_bbox) = unit
                .iter()
                .filter_map(|i| self.layer_bbox(*i))
                .reduce(|a, b| a.union(b))
            else {
                continue;
            };
            let (dx, dy) = match align {
                Align::Left => (whole.min.x - unit_bbox.min.x, 0.0),
                Align::CenterH => (whole.center().x - unit_bbox.center().x, 0.0),
                Align::Right => (whole.max.x - unit_bbox.max.x, 0.0),
                Align::Top => (0.0, whole.min.y - unit_bbox.min.y),
                Align::CenterV => (0.0, whole.center().y - unit_bbox.center().y),
                Align::Bottom => (0.0, whole.max.y - unit_bbox.max.y),
            };
            if dx.abs() < 1e-4 && dy.abs() < 1e-4 {
                continue;
            }
            let d = egui::vec2(dx, dy) / self.place_scale;
            if let Some(doc) = &mut self.doc {
                for (i, layer) in doc.layers.iter_mut().enumerate() {
                    if unit.contains(&i) {
                        for p in &mut layer.points {
                            *p += d;
                        }
                    }
                }
            }
            self.touch_texts(&unit, Affine::translate(d.x, d.y));
        }
        self.status = "alinhado entre si".to_string();
    }

    fn selected_originals(&self) -> Vec<(usize, Vec<egui::Pos2>)> {
        let Some(doc) = &self.doc else {
            return Vec::new();
        };
        self.selected
            .iter()
            .filter_map(|i| doc.layers.get(*i).map(|l| (*i, l.points.clone())))
            .collect()
    }

    fn layer_bbox(&self, i: usize) -> Option<egui::Rect> {
        let doc = self.doc.as_ref()?;
        let layer = doc.layers.get(i)?;
        if !layer.visible {
            return None;
        }
        layer.rect_placed(self.place_scale, self.place_offset)
    }

    fn selection_bbox(&self) -> Option<egui::Rect> {
        self.selected
            .iter()
            .filter_map(|i| self.layer_bbox(*i))
            .fold(None, |acc, rect| union(acc, Some(rect)))
    }

    fn art_bbox(&self) -> Option<egui::Rect> {
        let doc = self.doc.as_ref()?;
        let art = doc.arts.get(self.selected_art_idx)?;
        Some(
            art.rect.translate(
                doc.art_offsets
                    .get(self.selected_art_idx)
                    .copied()
                    .unwrap_or_default(),
            ),
        )
    }

    fn art_hit_at(&self, mm: egui::Pos2) -> Option<usize> {
        let doc = self.doc.as_ref()?;
        doc.arts
            .iter()
            .enumerate()
            .filter(|(_, art)| art.visible)
            .find(|(i, art)| {
                let off = doc.art_offsets.get(*i).copied().unwrap_or_default();
                art.rect.translate(off).contains(mm)
            })
            .map(|(i, _)| i)
    }

    fn art_hit(&self, mm: egui::Pos2) -> Option<(egui::Vec2, ArtOriginals)> {
        let doc = self.doc.as_ref()?;
        let art = doc.arts.get(self.selected_art_idx)?;
        if !art
            .rect
            .translate(
                doc.art_offsets
                    .get(self.selected_art_idx)
                    .copied()
                    .unwrap_or_default(),
            )
            .contains(mm)
        {
            return None;
        }
        // Traced layers are independent — dragging the image moves the image.
        let originals = Vec::new();
        Some((
            doc.art_offsets
                .get(self.selected_art_idx)
                .copied()
                .unwrap_or_default(),
            originals,
        ))
    }

    fn art_handles(bbox: egui::Rect) -> Vec<(Handle, egui::Pos2)> {
        let c = bbox.center();
        vec![
            (Handle::Nw, bbox.min),
            (Handle::N, egui::pos2(c.x, bbox.min.y)),
            (Handle::Ne, egui::pos2(bbox.max.x, bbox.min.y)),
            (Handle::E, egui::pos2(bbox.max.x, c.y)),
            (Handle::Se, bbox.max),
            (Handle::S, egui::pos2(c.x, bbox.max.y)),
            (Handle::Sw, egui::pos2(bbox.min.x, bbox.max.y)),
            (Handle::W, egui::pos2(bbox.min.x, c.y)),
        ]
    }

    fn hit_art_handle(&self, bbox: egui::Rect, mm: egui::Pos2, scale: f32) -> Option<Handle> {
        // Generous grab radius: tiny handles were hard to catch on zoomed-out
        // canvases and drags kept missing.
        let tol = 13.0 / scale.max(0.01);
        for (handle, pos) in Self::art_handles(bbox) {
            if (pos - mm).length() <= tol {
                return Some(handle);
            }
        }
        None
    }

    fn art_anchor(bbox: egui::Rect, handle: Handle, from_center: bool) -> egui::Pos2 {
        let c = bbox.center();
        if from_center {
            return c;
        }
        match handle {
            Handle::Nw => bbox.max,
            Handle::Ne => egui::pos2(bbox.min.x, bbox.max.y),
            Handle::Se => bbox.min,
            Handle::Sw => egui::pos2(bbox.max.x, bbox.min.y),
            Handle::N => egui::pos2(c.x, bbox.max.y),
            Handle::S => egui::pos2(c.x, bbox.min.y),
            Handle::E => egui::pos2(bbox.min.x, c.y),
            Handle::W => egui::pos2(bbox.max.x, c.y),
            Handle::Rotate => c,
        }
    }

    fn translate_art(&mut self, dx: f32, dy: f32) {
        if !(dx.is_finite() && dy.is_finite()) || (dx == 0.0 && dy == 0.0) {
            return;
        }
        let d = egui::vec2(dx, dy);
        if let Some(doc) = &mut self.doc {
            doc.art_offsets[self.selected_art_idx] += d;
            // Traced layers are independent: the image moves alone.
        }
        if let Some(preview) = &mut self.offset_preview {
            for (_, contours) in &mut preview.contours {
                for contour in contours {
                    for p in contour {
                        *p += d;
                    }
                }
            }
        }
        if let Some(preview) = &mut self.trace_preview {
            for path in preview {
                for p in &mut path.points {
                    p.0 += d.x;
                    p.1 += d.y;
                }
            }
        }
    }

    /// Scale the selected image about `anchor` (in placed/page coordinates).
    /// The stored rect is unplaced, so the offset is removed before storing —
    /// same care as the drag path above.
    fn scale_art_about(&mut self, anchor: egui::Pos2, sx: f32, sy: f32) {
        let sx = sx.clamp(0.01, 100.0);
        let sy = sy.clamp(0.01, 100.0);
        let idx = self.selected_art_idx;
        if let Some(doc) = &mut self.doc {
            let off = doc.art_offsets.get(idx).copied().unwrap_or_default();
            if let Some(art) = doc.arts.get_mut(idx) {
                let placed = art.rect.translate(off);
                art.rect = scale_rect(placed, anchor, sx, sy).translate(-off);
            }
            // Traced layers are independent — the image scales alone.
        }
        if let Some(preview) = &mut self.offset_preview {
            for (_, contours) in &mut preview.contours {
                for contour in contours {
                    for p in contour {
                        *p = scale_point(*p, anchor, sx, sy);
                    }
                }
            }
        }
        if let Some(preview) = &mut self.trace_preview {
            for path in preview {
                for p in &mut path.points {
                    let q = scale_point(egui::pos2(p.0, p.1), anchor, sx, sy);
                    p.0 = q.x;
                    p.1 = q.y;
                }
            }
        }
    }

    fn fit_art_to_page(&mut self) {
        let Some(doc) = self.doc.as_ref() else {
            self.status = "abra um ficheiro primeiro".to_string();
            return;
        };
        let Some(art) = doc.arts.get(self.selected_art_idx) else {
            self.status = "sem imagem para ajustar".to_string();
            return;
        };
        let page = doc.page;
        let margin = 10.0;
        let max_w = (page.0 - margin * 2.0).max(1.0);
        let max_h = (page.1 - margin * 2.0).max(1.0);
        let w = art.rect.width().max(0.01);
        let h = art.rect.height().max(0.01);
        let scale = (max_w / w).min(max_h / h).clamp(0.01, 100.0);
        // Anchor in placed/page coordinates (scale_art_about expects those).
        let center = self
            .art_bbox()
            .unwrap_or_else(|| art.rect)
            .center();
        self.push_undo();
        self.scale_art_about(center, scale, scale);
        let new_center = self.art_bbox().map(|r| r.center()).unwrap_or(center);
        self.translate_art(page.0 * 0.5 - new_center.x, page.1 * 0.5 - new_center.y);
        self.selected_art = true;
        self.selected.clear();
        self.tool = Tool::Image;
        self.status = format!("imagem ajustada à página ({:.0} %)", scale * 100.0);
    }

    fn art_size_percent(&self) -> Option<f32> {
        let doc = self.doc.as_ref()?;
        let art = doc.arts.get(self.selected_art_idx)?;
        let natural = art.width as f32 / 300.0 * 25.4;
        if natural <= 0.0 {
            return None;
        }
        Some(art.rect.width() / natural * 100.0)
    }

    fn reset_art_size(&mut self) {
        let Some(percent) = self.art_size_percent() else {
            self.status = "sem imagem".to_string();
            return;
        };
        if !(percent.is_finite() && percent > 0.0) {
            return;
        }
        let scale = 100.0 / percent;
        let Some(center) = self.art_bbox().map(|r| r.center()) else {
            return;
        };
        self.push_undo();
        self.scale_art_about(center, scale, scale);
        self.selected_art = true;
        self.selected.clear();
        self.status = "imagem no tamanho natural (100 %)".to_string();
    }

    /// Parameters for one trace pass (runs on worker threads; plain data).
    fn trace_job(&self, max_px: u32) -> Option<TraceJob> {
        let doc = self.doc.as_ref()?;
        let art = doc.arts.get(self.selected_art_idx)?;
        Some(TraceJob {
            rgba: art.rgba.clone(),
            width: art.width,
            height: art.height,
            max_px,
            rect: art.rect,
            offset: doc
                .art_offsets
                .get(self.selected_art_idx)
                .copied()
                .unwrap_or_default(),
            hue: art.hue,
            saturation: art.saturation,
            vibrance: art.vibrance,
            contrast: art.contrast,
            brightness: art.brightness,
            studio_noise_pct: self.trace_studio_noise,
            outer_only: self.trace_outer_only,
            apply_offset: self.trace_apply_offset,
            offset_mm: self.offset_mm,
            method: self.trace_method,
            options: {
                let auto = self.trace_method == TraceMethod::Auto;
                TraceOptions {
                mode: match self.trace_method {
                    TraceMethod::Outline => TraceMode::Outline,
                    // The vtracer engines have their own pipeline; Edges keeps
                    // the options struct valid without influencing them.
                        TraceMethod::Edges | TraceMethod::Auto | TraceMethod::Studio => {
                            TraceMode::Edges
                        }
                    },
                    threshold: self.trace_threshold.clamp(0.0, 255.0) as u8,
                    edge_threshold: self.trace_edge.clamp(0.0, 255.0) as u8,
                    simplify_mm: if auto {
                        0.2
                    } else {
                        self.trace_simplify.max(0.0)
                    },
                    min_length_mm: if auto {
                        1.0
                    } else {
                        self.trace_min_length.max(0.0)
                    },
                    min_area_mm2: self.trace_min_area.max(0.0),
                    // invert is decided per image inside run_trace_job.
                    invert: false,
                    bg_color: None,
                    color_tolerance: 25,
                    smooth_iterations: if auto {
                        2
                    } else {
                        self.trace_smooth.min(4)
                    },
                    mm_per_px: 1.0,
                }
            },
        })
    }

    /// Trace synchronously (used by «Vetorizar», at the same resolution as the
    /// live preview so what you see is exactly what you get).
    fn trace_now(&self, max_px: u32) -> Option<Vec<TracedPath>> {
        self.trace_job(max_px).map(|job| run_trace_job(&job))
    }

    /// Queue a background re-trace of the live preview (never blocks the UI).
    fn request_trace_preview(&mut self) {
        self.trace_dirty = true;
    }

    fn handle_positions(bbox: egui::Rect, scale: f32) -> Vec<(Handle, egui::Pos2)> {
        let c = bbox.center();
        let rot = egui::pos2(c.x, bbox.min.y - 22.0 / scale.max(0.01));
        vec![
            (Handle::Nw, bbox.min),
            (Handle::N, egui::pos2(c.x, bbox.min.y)),
            (Handle::Ne, egui::pos2(bbox.max.x, bbox.min.y)),
            (Handle::E, egui::pos2(bbox.max.x, c.y)),
            (Handle::Se, bbox.max),
            (Handle::S, egui::pos2(c.x, bbox.max.y)),
            (Handle::Sw, egui::pos2(bbox.min.x, bbox.max.y)),
            (Handle::W, egui::pos2(bbox.min.x, c.y)),
            (Handle::Rotate, rot),
        ]
    }

    fn hit_handle(&self, bbox: egui::Rect, mm: egui::Pos2, scale: f32) -> Option<Handle> {
        let tol = 8.0 / scale.max(0.01);
        for (handle, pos) in Self::handle_positions(bbox, scale) {
            if (pos - mm).length() <= tol {
                return Some(handle);
            }
        }
        None
    }

    fn apply_scale(
        &mut self,
        originals: &[(usize, Vec<egui::Pos2>)],
        bbox: egui::Rect,
        handle: Handle,
        pointer: egui::Pos2,
        from_center: bool,
        uniform: bool,
    ) {
        let center = bbox.center();
        let left = matches!(handle, Handle::Nw | Handle::W | Handle::Sw);
        let right = matches!(handle, Handle::Ne | Handle::E | Handle::Se);
        let top = matches!(handle, Handle::Nw | Handle::N | Handle::Ne);
        let bottom = matches!(handle, Handle::Sw | Handle::S | Handle::Se);
        if !left && !right && !top && !bottom {
            return;
        }
        let anchor_x = if from_center {
            center.x
        } else if left {
            bbox.max.x
        } else if right {
            bbox.min.x
        } else {
            center.x
        };
        let anchor_y = if from_center {
            center.y
        } else if top {
            bbox.max.y
        } else if bottom {
            bbox.min.y
        } else {
            center.y
        };
        let mut sx = 1.0f32;
        let mut sy = 1.0f32;
        if left || right {
            let denom = if from_center {
                (bbox.width() * 0.5).max(1e-4)
            } else {
                bbox.width().max(1e-4)
            };
            let dist = if right {
                pointer.x - anchor_x
            } else {
                anchor_x - pointer.x
            };
            sx = fixed_scale(dist / denom);
        }
        if top || bottom {
            let denom = if from_center {
                (bbox.height() * 0.5).max(1e-4)
            } else {
                bbox.height().max(1e-4)
            };
            let dist = if bottom {
                pointer.y - anchor_y
            } else {
                anchor_y - pointer.y
            };
            sy = fixed_scale(dist / denom);
        }
        if uniform && (left || right) && (top || bottom) {
            let s = if sx.abs() > sy.abs() { sx } else { sy };
            sx = s;
            sy = s;
        }

        let mut updated = Vec::with_capacity(originals.len());
        for (idx, points) in originals {
            let mapped: Vec<egui::Pos2> = points
                .iter()
                .map(|p| {
                    let q = self.place(*p);
                    let transformed = egui::pos2(
                        anchor_x + (q.x - anchor_x) * sx,
                        anchor_y + (q.y - anchor_y) * sy,
                    );
                    self.unplace(transformed)
                })
                .collect();
            updated.push((*idx, mapped));
        }
        if let Some(doc) = &mut self.doc {
            for (idx, points) in updated {
                if let Some(layer) = doc.layers.get_mut(idx) {
                    layer.points = points;
                }
            }
        }
        let selected: Vec<usize> = originals.iter().map(|(i, _)| *i).collect();
        let g = self.placement_affine(Affine::scale(sx, sy), egui::pos2(anchor_x, anchor_y));
        self.touch_texts(&selected, g);
        self.status = format!("escala {:.0}% × {:.0}%", sx * 100.0, sy * 100.0);
    }

    fn apply_rotation(
        &mut self,
        originals: &[(usize, Vec<egui::Pos2>)],
        center: egui::Pos2,
        angle: f32,
    ) {
        let (sin, cos) = angle.sin_cos();
        let mut updated = Vec::with_capacity(originals.len());
        for (idx, points) in originals {
            let mapped: Vec<egui::Pos2> = points
                .iter()
                .map(|p| {
                    let q = self.place(*p);
                    let dx = q.x - center.x;
                    let dy = q.y - center.y;
                    self.unplace(egui::pos2(
                        center.x + dx * cos - dy * sin,
                        center.y + dx * sin + dy * cos,
                    ))
                })
                .collect();
            updated.push((*idx, mapped));
        }
        if let Some(doc) = &mut self.doc {
            for (idx, points) in updated {
                if let Some(layer) = doc.layers.get_mut(idx) {
                    layer.points = points;
                }
            }
        }
        let selected: Vec<usize> = originals.iter().map(|(i, _)| *i).collect();
        let g = self.placement_affine(Affine::rotate(angle), center);
        self.touch_texts(&selected, g);
        self.status = format!("rotação {:.1}°", angle.to_degrees());
    }

    fn transform_selection(&mut self, rotate: Option<f32>, flip: Option<bool>) {
        let Some(bbox) = self.selection_bbox() else {
            return;
        };
        let center = bbox.center();
        let originals = self.selected_originals();
        self.push_undo();
        if let Some(angle) = rotate {
            self.apply_rotation(&originals, center, angle);
        }
        if let Some(horizontal) = flip {
            let mut updated = Vec::new();
            for (idx, points) in &originals {
                let mapped = points
                    .iter()
                    .map(|p| {
                        let q = self.place(*p);
                        let q = if horizontal {
                            egui::pos2(2.0 * center.x - q.x, q.y)
                        } else {
                            egui::pos2(q.x, 2.0 * center.y - q.y)
                        };
                        self.unplace(q)
                    })
                    .collect();
                updated.push((*idx, mapped));
            }
            if let Some(doc) = &mut self.doc {
                for (idx, points) in updated {
                    if let Some(layer) = doc.layers.get_mut(idx) {
                        layer.points = points;
                    }
                }
            }
            let selected: Vec<usize> = originals.iter().map(|(i, _)| *i).collect();
            let g = self.placement_affine(Affine::flip(horizontal), center);
            self.touch_texts(&selected, g);
            self.status = if horizontal {
                "invertido na horizontal".to_string()
            } else {
                "invertido na vertical".to_string()
            };
        }
    }

    fn reset_selected(&mut self) {
        self.push_undo();
        let indices = self.selected.clone();
        if let Some(doc) = &mut self.doc {
            for i in &indices {
                if let Some(layer) = doc.layers.get_mut(*i) {
                    layer.points = layer.original.clone();
                }
            }
            for text in &mut doc.texts {
                if doc
                    .layers
                    .iter()
                    .enumerate()
                    .any(|(i, l)| l.owner == Some(text.id) && indices.contains(&i))
                {
                    text.affine = Affine::identity();
                }
            }
        }
        self.status = "original reposto".to_string();
    }

    fn delete_selected(&mut self) {
        if self.selected.is_empty() {
            return;
        }
        let count = self.selected.len();
        self.push_undo();
        let mut indices = self.selected.clone();
        indices.sort_unstable();
        indices.dedup();
        if let Some(doc) = &mut self.doc {
            for i in indices.into_iter().rev() {
                if i < doc.layers.len() {
                    doc.layers.remove(i);
                }
            }
            doc.texts
                .retain(|text| doc.layers.iter().any(|layer| layer.owner == Some(text.id)));
        }
        self.selected.clear();
        self.offset_preview = None;
        self.status = format!("{count} traço(s) apagado(s)");
    }

    fn mirror_selection(&mut self, horizontal: bool) {
        if self.selected.is_empty() && !self.selected_art {
            return;
        }
        self.push_undo();
        if self.selected_art {
            if let Some(doc) = &mut self.doc {
                if let Some(art) = doc.arts.get_mut(self.selected_art_idx) {
                    let cx = art.rect.center().x;
                    let cy = art.rect.center().y;
                    if horizontal {
                        art.rect = egui::Rect::from_min_max(
                            egui::pos2(2.0 * cx - art.rect.max.x, art.rect.min.y),
                            egui::pos2(2.0 * cx - art.rect.min.x, art.rect.max.y),
                        );
                    } else {
                        art.rect = egui::Rect::from_min_max(
                            egui::pos2(art.rect.min.x, 2.0 * cy - art.rect.max.y),
                            egui::pos2(art.rect.max.x, 2.0 * cy - art.rect.min.y),
                        );
                    }
                }
            }
        } else {
            if let Some(doc) = &mut self.doc {
                for i in &self.selected {
                    if let Some(layer) = doc.layers.get_mut(*i) {
                        let cx = layer.points.iter().map(|p| p.x).sum::<f32>()
                            / layer.points.len().max(1) as f32;
                        let cy = layer.points.iter().map(|p| p.y).sum::<f32>()
                            / layer.points.len().max(1) as f32;
                        if horizontal {
                            for p in &mut layer.points {
                                p.x = 2.0 * cx - p.x;
                            }
                        } else {
                            for p in &mut layer.points {
                                p.y = 2.0 * cy - p.y;
                            }
                        }
                    }
                }
            }
        }
        self.status = if horizontal {
            "espelhado horizontal"
        } else {
            "espelhado vertical"
        }
        .to_string();
    }

    fn rotate_selection(&mut self, degrees: f32) {
        if self.selected.is_empty() && !self.selected_art {
            return;
        }
        self.push_undo();
        let rad = degrees.to_radians();
        let (sin, cos) = (rad.sin(), rad.cos());
        if self.selected_art {
            if let Some(doc) = &mut self.doc {
                if let Some(art) = doc.arts.get_mut(self.selected_art_idx) {
                    let cx = art.rect.center().x;
                    let cy = art.rect.center().y;
                    // For art, just swap width/height for 90°
                    if (degrees - 90.0).abs() < 1.0
                        || (degrees + 90.0).abs() < 1.0
                        || (degrees - 270.0).abs() < 1.0
                    {
                        let w = art.rect.width();
                        let h = art.rect.height();
                        art.rect = egui::Rect::from_min_size(
                            egui::pos2(cx - h / 2.0, cy - w / 2.0),
                            egui::vec2(h, w),
                        );
                    }
                }
            }
        } else {
            if let Some(doc) = &mut self.doc {
                for i in &self.selected {
                    if let Some(layer) = doc.layers.get_mut(*i) {
                        let cx = layer.points.iter().map(|p| p.x).sum::<f32>()
                            / layer.points.len().max(1) as f32;
                        let cy = layer.points.iter().map(|p| p.y).sum::<f32>()
                            / layer.points.len().max(1) as f32;
                        for p in &mut layer.points {
                            let dx = p.x - cx;
                            let dy = p.y - cy;
                            p.x = cx + dx * cos - dy * sin;
                            p.y = cy + dx * sin + dy * cos;
                        }
                    }
                }
            }
        }
        self.status = format!("rotado {degrees:.0}°");
    }

    fn trace_with_mode(&mut self, method: TraceMethod) {
        if let Some(doc) = &self.doc {
            if !doc.arts.is_empty() {
                let old_mode = self.trace_method;
                self.trace_method = method;
                self.vectorize();
                self.trace_method = old_mode;
            } else {
                self.status = "sem imagem raster para vetorizar".to_string();
            }
        } else {
            self.status = "abra uma imagem primeiro".to_string();
        }
    }

    fn snap_targets(&self, exclude: &[usize], include_art: bool) -> (Vec<f32>, Vec<f32>) {
        let page = self.page();
        let mut xs = vec![0.0, page.0 * 0.5, page.0];
        let mut ys = vec![0.0, page.1 * 0.5, page.1];
        if include_art {
            if let Some(r) = self.art_bbox() {
                xs.extend([r.min.x, r.center().x, r.max.x]);
                ys.extend([r.min.y, r.center().y, r.max.y]);
            }
        }
        if let Some(doc) = &self.doc {
            for (i, layer) in doc.layers.iter().enumerate() {
                if exclude.contains(&i) || !layer.visible {
                    continue;
                }
                if let Some(r) = layer.rect_placed(self.place_scale, self.place_offset) {
                    xs.extend([r.min.x, r.center().x, r.max.x]);
                    ys.extend([r.min.y, r.center().y, r.max.y]);
                }
                for p in &layer.points {
                    let q = self.place(*p);
                    xs.push(q.x);
                    ys.push(q.y);
                }
                let placed: Vec<egui::Pos2> = layer.points.iter().map(|p| self.place(*p)).collect();
                let segments =
                    placed.len().saturating_sub(1) + usize::from(layer.closed && placed.len() > 2);
                for s in 0..segments {
                    let a = placed[s];
                    let b = placed[(s + 1) % placed.len()];
                    let mid = a + (b - a) * 0.5;
                    xs.push(mid.x);
                    ys.push(mid.y);
                }
            }
        }
        (xs, ys)
    }

    fn snap_point(
        &self,
        p: egui::Pos2,
        exclude: &[usize],
        threshold: f32,
    ) -> (egui::Pos2, Vec<Guide>) {
        let mut guides = Vec::new();
        if !self.snap_grid && !self.snap_guides {
            return (p, guides);
        }
        let mut out = p;
        if self.snap_guides {
            let (xs, ys) = self.snap_targets(exclude, true);
            let mut best_x = threshold;
            for t in &xs {
                let d = (t - p.x).abs();
                if d < best_x {
                    best_x = d;
                    out.x = *t;
                }
            }
            let mut best_y = threshold;
            for t in &ys {
                let d = (t - p.y).abs();
                if d < best_y {
                    best_y = d;
                    out.y = *t;
                }
            }
            for t in xs {
                if (t - out.x).abs() <= 0.05
                    && !guides
                        .iter()
                        .any(|g| matches!(g, Guide::V(x) if (*x - t).abs() < 1e-3))
                {
                    guides.push(Guide::V(t));
                }
            }
            for t in ys {
                if (t - out.y).abs() <= 0.05
                    && !guides
                        .iter()
                        .any(|g| matches!(g, Guide::H(y) if (*y - t).abs() < 1e-3))
                {
                    guides.push(Guide::H(t));
                }
            }
        }
        if self.snap_grid {
            let gx = (p.x / self.grid_mm).round() * self.grid_mm;
            if (gx - p.x).abs() < threshold {
                out.x = gx;
                guides.retain(|g| !matches!(g, Guide::V(_)));
                guides.push(Guide::V(gx));
            }
            let gy = (p.y / self.grid_mm).round() * self.grid_mm;
            if (gy - p.y).abs() < threshold {
                out.y = gy;
                guides.retain(|g| !matches!(g, Guide::H(_)));
                guides.push(Guide::H(gy));
            }
        }
        (out, guides)
    }

    fn snap_move(
        &self,
        rect: egui::Rect,
        raw: egui::Vec2,
        exclude: &[usize],
        threshold: f32,
        include_art: bool,
    ) -> (egui::Vec2, Vec<Guide>) {
        let mut guides = Vec::new();
        if !self.snap_grid && !self.snap_guides {
            return (raw, guides);
        }
        let moved = rect.translate(raw);
        let mut dx = raw.x;
        let mut dy = raw.y;
        if self.snap_guides {
            let (xs, ys) = self.snap_targets(exclude, include_art);
            let mut best_x = threshold;
            for t in &xs {
                for edge in [moved.min.x, moved.center().x, moved.max.x] {
                    let corr = t - edge;
                    if corr.abs() < best_x {
                        best_x = corr.abs();
                        dx = raw.x + corr;
                    }
                }
            }
            let mut best_y = threshold;
            for t in &ys {
                for edge in [moved.min.y, moved.center().y, moved.max.y] {
                    let corr = t - edge;
                    if corr.abs() < best_y {
                        best_y = corr.abs();
                        dy = raw.y + corr;
                    }
                }
            }
            let final_rect = rect.translate(egui::vec2(dx, dy));
            for t in xs {
                if [final_rect.min.x, final_rect.center().x, final_rect.max.x]
                    .iter()
                    .any(|edge| (edge - t).abs() <= 0.05)
                    && !guides
                        .iter()
                        .any(|g| matches!(g, Guide::V(x) if (*x - t).abs() < 1e-3))
                {
                    guides.push(Guide::V(t));
                }
            }
            for t in ys {
                if [final_rect.min.y, final_rect.center().y, final_rect.max.y]
                    .iter()
                    .any(|edge| (edge - t).abs() <= 0.05)
                    && !guides
                        .iter()
                        .any(|g| matches!(g, Guide::H(y) if (*y - t).abs() < 1e-3))
                {
                    guides.push(Guide::H(t));
                }
            }
        }
        if self.snap_grid {
            let mut best_x = threshold;
            let mut best_y = threshold;
            let mut gx = None;
            let mut gy = None;
            for edge in [moved.min.x, moved.center().x, moved.max.x] {
                let t = (edge / self.grid_mm).round() * self.grid_mm;
                let corr = t - edge;
                if corr.abs() < best_x {
                    best_x = corr.abs();
                    dx = raw.x + corr;
                    gx = Some(t);
                }
            }
            for edge in [moved.min.y, moved.center().y, moved.max.y] {
                let t = (edge / self.grid_mm).round() * self.grid_mm;
                let corr = t - edge;
                if corr.abs() < best_y {
                    best_y = corr.abs();
                    dy = raw.y + corr;
                    gy = Some(t);
                }
            }
            if let Some(t) = gx {
                guides.retain(|g| !matches!(g, Guide::V(_)));
                guides.push(Guide::V(t));
            }
            if let Some(t) = gy {
                guides.retain(|g| !matches!(g, Guide::H(_)));
                guides.push(Guide::H(t));
            }
        }
        (egui::vec2(dx, dy), guides)
    }

    fn add_text(&mut self, ctx: &egui::Context) {
        let font = match self.fonts.get(self.font_index) {
            Some(font) => font,
            None => {
                self.status = "importe uma fonte primeiro".to_string();
                return;
            }
        };
        let font_name = font.name.clone();
        let page = self.page();
        let origin = self
            .text_origin
            .unwrap_or(egui::pos2(page.0 * 0.3, page.1 * 0.4));
        let paths = match import::text_paths(font, &self.text, self.text_size, (origin.x, origin.y))
        {
            Ok(paths) => paths,
            Err(e) => {
                self.status = e;
                return;
            }
        };
        if paths.is_empty() {
            self.status = "texto sem contornos".to_string();
            return;
        }
        self.push_undo();
        let id = self.next_id();
        let start = self.doc.as_ref().map(|d| d.layers.len()).unwrap_or(0);
        if let Some(doc) = &mut self.doc {
            for p in paths {
                let points: Vec<egui::Pos2> = p
                    .points
                    .iter()
                    .map(|q| egui::pos2(q.x_mm, q.y_mm))
                    .collect();
                doc.layers.push(Layer {
                    kind: p.kind,
                    original: points.clone(),
                    points,
                    closed: p.closed,
                    visible: true,
                    group: Some(id),
                    owner: Some(id),
                    traced: false,
                    art: None,
                    color: LAYER_COLOR,
                });
            }
            doc.texts.push(TextItem {
                id,
                group: Some(id),
                font: font_name,
                text: self.text.clone(),
                size: self.text_size,
                origin,
                affine: Affine::identity(),
            });
        }
        let end = self.doc.as_ref().map(|d| d.layers.len()).unwrap_or(start);
        self.select_indices((start..end).collect(), false);
        self.tool = Tool::Select;
        self.status = "texto adicionado (editável)".to_string();
        ctx.request_repaint();
    }

    fn import_font(&mut self, ctx: &egui::Context) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Fontes", &["ttf", "otf"])
            .pick_file()
        else {
            return;
        };
        match import::install_font(&path) {
            Ok(font) => {
                self.status = format!("fonte importada: {}", font.name);
                self.fonts.retain(|f| f.name != font.name);
                self.fonts.push(font);
                self.font_index = self.fonts.len() - 1;
                import::register_ui_font(ctx, &self.fonts);
            }
            Err(e) => self.status = e,
        }
    }

    fn zoom_by(&mut self, factor: f32) {
        self.zoom = (self.zoom * factor).clamp(0.1, 40.0);
    }
}

fn fixed_scale(s: f32) -> f32 {
    if s.is_finite() {
        if s.abs() < 0.02 {
            0.02_f32.copysign(if s < 0.0 { -1.0 } else { 1.0 })
        } else {
            s
        }
    } else {
        0.02
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.space_down = ctx.input(|i| i.key_down(egui::Key::Space));
        let now = ctx.input(|i| i.time);
        if now - self.printer_poll > 5.0 {
            self.printer_poll = now;
            self.refresh_printers();
        }
        if now - self.prefs_poll > 60.0 {
            self.prefs_poll = now;
            self.save_prefs();
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::O)) {
            self.open_dialog(ctx);
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::S)) {
            self.save_project();
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Z)) {
            if ctx.input(|i| i.modifiers.shift) {
                self.redo();
            } else {
                self.undo();
            }
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Y)) {
            self.redo();
        }
        if !ctx.wants_keyboard_input()
            && ctx
                .input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace))
        {
            self.delete_selected();
        }
        if ctx.input(|i| i.key_pressed(egui::Key::F1)) {
            self.help_open = true;
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Plus)) {
            self.zoom_by(1.15);
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Minus)) {
            self.zoom_by(1.0 / 1.15);
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Num0)) {
            self.zoom = 1.0;
            self.pan = egui::Vec2::ZERO;
            self.status = "vista reposta".to_string();
        }
        if !ctx.wants_keyboard_input() {
            let (ctrl, shift, keys) = ctx.input(|i| {
                (
                    i.modifiers.command,
                    i.modifiers.shift,
                    [
                        i.key_pressed(egui::Key::V),
                        i.key_pressed(egui::Key::N),
                        i.key_pressed(egui::Key::T),
                        i.key_pressed(egui::Key::I),
                        i.key_pressed(egui::Key::H),
                        i.key_pressed(egui::Key::Escape),
                        i.key_pressed(egui::Key::A),
                        i.key_pressed(egui::Key::G),
                        i.key_pressed(egui::Key::ArrowLeft),
                        i.key_pressed(egui::Key::ArrowRight),
                        i.key_pressed(egui::Key::ArrowUp),
                        i.key_pressed(egui::Key::ArrowDown),
                    ],
                )
            });
            if keys[0] && !ctrl {
                self.tool = Tool::Select;
                self.status = "ferramenta: selecionar".to_string();
            }
            if keys[1] && !ctrl {
                self.tool = Tool::Nodes;
                self.status = "ferramenta: nós".to_string();
            }
            if keys[2] && !ctrl {
                self.tool = Tool::Text;
                self.status = "ferramenta: texto".to_string();
            }
            if keys[3] && !ctrl {
                self.tool = Tool::Image;
                self.status = "ferramenta: imagem".to_string();
            }
            if keys[4] && !ctrl {
                self.tool = Tool::Pan;
                self.status = "ferramenta: mover vista".to_string();
            }
            if keys[5] {
                self.drag = None;
                self.guides.clear();
                self.text_origin = None;
                self.last_band = None;
                self.selected.clear();
                self.selected_art = false;
                self.trace_preview = None;
                self.offset_preview = None;
                self.tool = Tool::Select;
                self.status = "seleção limpa".to_string();
            }
            if ctrl && keys[6] {
                if let Some(doc) = &self.doc {
                    self.selected = (0..doc.layers.len()).collect();
                }
                self.selected_art = false;
                self.expand_selection();
                self.status = format!("{} camada(s) selecionada(s)", self.selected.len());
            }
            if ctrl && keys[7] {
                if shift {
                    self.ungroup_selected();
                } else {
                    self.group_selected();
                }
            }
            if ctrl && ctx.input(|i| i.key_pressed(egui::Key::D)) {
                self.duplicate_selected();
            }
            let step = if shift { 10.0 } else { 1.0 };
            let (dx, dy) = (
                f32::from(i8::from(keys[9])) - f32::from(i8::from(keys[8])),
                f32::from(i8::from(keys[11])) - f32::from(i8::from(keys[10])),
            );
            if dx != 0.0 || dy != 0.0 {
                self.push_undo();
                if self.selected_art {
                    self.translate_art(dx * step, dy * step);
                } else {
                    self.translate_selected(dx * step, dy * step);
                }
                self.status = format!("movido {:.0} mm, {:.0} mm", dx * step, dy * step);
            }
        }

        if self.bt_rx.is_some() {
            let mut received = None;
            let mut closed = false;
            if let Some(rx) = &self.bt_rx {
                match rx.try_recv() {
                    Ok(devices) => received = Some(devices),
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => closed = true,
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                }
            }
            if let Some(devices) = received {
                self.bt_rx = None;
                self.bt_status = format!("{} dispositivo(s) encontrado(s)", devices.len());
                self.bt_devices = devices;
            } else if closed {
                self.bt_rx = None;
                self.bt_status = "procura Bluetooth terminou".to_string();
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(250));
            }
        }

        // Resultado das ligações abertas em segundo plano (USB/Bluetooth).
        if let Some(rx) = &self.machine_rx {
            match rx.try_recv() {
                Ok((slot, Ok(machine))) => {
                    self.machine_rx = None;
                    if slot < self.machines.len() {
                        let via = if machine.link == Link::Usb { "USB" } else { "Bluetooth" };
                        self.machines[slot] = machine;
                        self.status = format!("Máquina {} ligada por {}", slot + 1, via);
                    }
                }
                Ok((_, Err(e))) => {
                    self.machine_rx = None;
                    if e.contains("acesso negado") {
                        self.udev_hint = true;
                    }
                    self.status = e;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(250));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.machine_rx = None;
                }
            }
        }

        // Pré-visualização de vetorização em segundo plano: nunca bloqueia a
        // UI. O auto-preview acontece uma vez por imagem (para o modo Auto, que
        // não tem sliders) — depois de «Vetorizar» o contorno azul não volta
        // sozinho.
        if self.trace_rx.is_none()
            && !self.trace_dirty
            && self.trace_preview.is_none()
            && self.preview_shown_for != Some(self.selected_art_idx)
            && self.selected_art
            && self
                .doc
                .as_ref()
                .is_some_and(|d| !d.arts.is_empty())
        {
            self.trace_dirty = true;
        }
        if self.trace_dirty && self.trace_rx.is_none() {
            self.trace_dirty = false;
            match self.trace_job(TRACE_PX) {
                Some(job) => {
                    let (tx, rx) = std::sync::mpsc::channel();
                    self.trace_rx = Some(rx);
                    let art_idx = self.selected_art_idx;
                    std::thread::spawn(move || {
                        let _ = tx.send((art_idx, run_trace_job(&job)));
                    });
                    ctx.request_repaint_after(std::time::Duration::from_millis(60));
                }
                None => self.trace_preview = None,
            }
        }
        if let Some(rx) = &self.trace_rx {
            match rx.try_recv() {
                Ok((art_idx, paths)) => {
                    self.trace_rx = None;
                    self.trace_preview = Some(paths);
                    self.preview_shown_for = Some(art_idx);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(60));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.trace_rx = None;
                }
            }
        }

        // USB hotplug: enumerar é leve (não abre o dispositivo). Deteça
        // chegadas em slots livres e quedas de máquinas ligadas.
        if self
            .usb_poll
            .map_or(true, |t| t.elapsed() >= std::time::Duration::from_secs(2))
        {
            self.usb_poll = Some(std::time::Instant::now());
            if let Ok(devices) = list_cameo5() {
                for slot in 0..self.machines.len() {
                    if let (Some((bus, address)), true) = (
                        self.machines[slot].usb,
                        self.machines[slot].connected(),
                    ) {
                        if !devices.iter().any(|d| d.bus == bus && d.address == address) {
                            self.machines[slot] = Machine::empty();
                            self.usb_seen = false;
                            self.status = format!("Máquina {}: USB desapareceu", slot + 1);
                        }
                    }
                }
                let used: Vec<(u8, u8)> =
                    self.machines.iter().filter_map(|m| m.usb).collect();
                let free = devices
                    .iter()
                    .find(|d| !used.contains(&(d.bus, d.address)))
                    .map(|d| (d.bus, d.address));
                match free {
                    Some((bus, address)) if !self.usb_seen => {
                        self.usb_seen = true;
                        self.status = format!(
                            "Cameo 5 detetada (USB {bus:03}:{address:03}) — pronta a ligar"
                        );
                    }
                    None => self.usb_seen = false,
                    _ => {}
                }
            }
        }

        // Pré-visualização ao vivo do texto (ferramenta Texto): contornos
        // fantasma na posição escolhida enquanto se escreve.
        if self.tool == Tool::Text && self.doc.is_some() && !self.text.is_empty() {
            let origin = self.text_origin.unwrap_or_else(|| {
                let page = self.page();
                egui::pos2(page.0 * 0.3, page.1 * 0.4)
            });
            let key = (
                self.font_index,
                self.text.clone(),
                self.text_size,
                [origin.x, origin.y],
            );
            if key != self.text_preview_key {
                let paths = match self.fonts.get(self.font_index) {
                    Some(font) => {
                        import::text_paths(font, &self.text, self.text_size, (origin.x, origin.y))
                            .ok()
                    }
                    None => None,
                };
                self.text_preview = paths.map(|paths| {
                    paths
                        .iter()
                        .map(|p| {
                            p.points
                                .iter()
                                .map(|q| egui::pos2(q.x_mm, q.y_mm))
                                .collect()
                        })
                        .collect()
                });
                self.text_preview_key = key;
            }
        } else if self.text_preview.is_some() {
            self.text_preview = None;
        }

        egui::TopBottomPanel::top("toolbar")
            .frame(glass_frame(ctx).inner_margin(egui::Margin::symmetric(12.0, 8.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Sillinux")
                            .strong()
                            .size(18.0)
                            .color(egui::Color32::from_rgb(0, 90, 170)),
                    );
                    ui.separator();
                    if ui
                        .button("📂 Abrir")
                        .on_hover_text(t(self.lang, "Abrir ficheiro (Ctrl+O)"))
                        .clicked()
                    {
                        self.open_dialog(ctx);
                    }
                    if ui
                        .button("💾 Guardar")
                        .on_hover_text(t(self.lang, "Guardar projeto SVG (Ctrl+S)"))
                        .clicked()
                    {
                        self.save_project();
                    }
                    if ui
                        .button("✂ SVG corte")
                        .on_hover_text(t(self.lang, "Exportar SVG com linhas de corte"))
                        .clicked()
                    {
                        self.export_svg();
                    }
                    if ui
                        .button("🖨 PDF")
                        .on_hover_text(t(self.lang, "Gerar folha A4 com marcas"))
                        .clicked()
                    {
                        self.print_sheet();
                    }
                    ui.separator();
                    if ui
                        .button("↶ Desfazer")
                        .on_hover_text(t(self.lang, "Desfazer (Ctrl+Z)"))
                        .clicked()
                    {
                        self.undo();
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(t(self.lang, "❓")).on_hover_text(t(self.lang, "Ajuda e comandos")).clicked() {
                            self.help_open = !self.help_open;
                        }
                        ui.separator();
                        let label = if self.theme_dark { "☀" } else { "☾" };
                        if ui
                            .button(label)
                            .on_hover_text(if self.theme_dark {
                                "Mudar para tema claro"
                            } else {
                                "Mudar para tema escuro"
                            })
                            .clicked()
                        {
                            self.theme_dark = !self.theme_dark;
                            apply_theme(ctx, self.theme_dark);
                        }
                        ui.separator();
                        if ui.button(t(self.lang, "⤢")).on_hover_text(t(self.lang, "Ajustar à janela")).clicked() {
                            self.zoom = 1.0;
                            self.pan = egui::Vec2::ZERO;
                        }
                        ui.label(format!("{:.0}%", self.zoom * 100.0));
                        if ui.button(t(self.lang, "+")).on_hover_text(t(self.lang, "Aproximar")).clicked() {
                            self.zoom_by(1.15);
                        }
                        if ui.button(t(self.lang, "-")).on_hover_text(t(self.lang, "Afastar")).clicked() {
                            self.zoom_by(1.0 / 1.15);
                        }
                        ui.separator();
                        // Idioma / language
                        if ui
                            .button(self.lang.toggle_label())
                            .on_hover_text(t(self.lang, "Mudar o idioma / Switch language"))
                            .clicked()
                        {
                            self.lang = self.lang.other();
                            self.save_prefs();
                        }
                    });
                });
            });

        // Janela de boas-vindas: guia rápido para iniciantes sem documento.
        if self.doc.is_none() && !self.welcome_hidden {
            let mut open = true;
            egui::Window::new(
                egui::RichText::new(t(self.lang, "Bem-vindo ao Sillinux"))
                    .strong()
                    .size(17.0),
            )
            .open(&mut open)
            .default_width(430.0)
            .default_pos(egui::pos2(360.0, 140.0))
            .collapsible(false)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.label(t(self.lang, "Cortar na Cameo em três passos:"));
                ui.add_space(2.0);
                ui.label(t(self.lang, "1 · Abra uma ou mais imagens (.png, .jpg, .svg, .pdf, .psd…)."));
                ui.label(t(self.lang, "2 · Ajuste a cor se quiser e use «Vetorizar imagem» para criar as linhas."));
                ui.label(t(self.lang, "3 · Em «Ligações» ligue a máquina e confirme o corte."));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui
                        .button(format!("📂 {}", t(self.lang, "Abrir imagens")))
                        .clicked()
                    {
                        self.open_dialog(ctx);
                    }
                    if ui
                        .button(format!("🔤 {}", t(self.lang, "Importar fonte")))
                        .clicked()
                    {
                        self.import_font(ctx);
                    }
                });
                ui.horizontal(|ui| {
                    if ui
                        .button(format!("🛠 {}", t(self.lang, "Instalar regra udev (USB)")))
                        .clicked()
                    {
                        self.install_udev_rule();
                    }
                    if ui.button(format!("❓ {}", t(self.lang, "Ajuda"))).clicked() {
                        self.help_open = true;
                    }
                });
                ui.add_space(6.0);
                ui.separator();
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.welcome_hidden, t(self.lang, "Não mostrar novamente"));
                    if ui.button(self.lang.toggle_label()).clicked() {
                        self.lang = self.lang.other();
                        self.save_prefs();
                    }
                });
            });
            if !open {
                self.welcome_hidden = true;
            }
        }

        egui::TopBottomPanel::bottom("status")
            .frame(glass_frame(ctx).inner_margin(egui::Margin::symmetric(12.0, 5.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(&self.status);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let connected = self.machines.iter().filter(|m| m.connected()).count();
                        let printer = self
                            .printers
                            .get(self.printer_index)
                            .cloned()
                            .unwrap_or_else(|| "sem impressora".to_string());
                        ui.label(
                            egui::RichText::new(format!(
                                "M{connected}/2 · ▤ {printer} · {:.0}%",
                                self.zoom * 100.0
                            ))
                            .size(11.0),
                        );
                        if let Some(doc) = &self.doc {
                            ui.label(
                                egui::RichText::new(format!(
                                    "página {:.1} × {:.1} mm",
                                    doc.page.0, doc.page.1
                                ))
                                .size(11.0),
                            );
                        }
                    });
                });
            });

        egui::SidePanel::left("tools")
            .resizable(false)
            .exact_width(106.0)
            .frame(glass_frame(ctx).inner_margin(egui::Margin::symmetric(8.0, 8.0)))
            .show(ctx, |ui| {
                ui.add_space(6.0);
                ui.vertical_centered(|ui| {
                    if ui
                        .add_sized(
                            [80.0, 30.0],
                            egui::SelectableLabel::new(self.tool == Tool::Select, "↖ Selecionar"),
                        )
                        .clicked()
                    {
                        self.tool = Tool::Select;
                    }
                    if ui
                        .add_sized(
                            [80.0, 30.0],
                            egui::SelectableLabel::new(self.tool == Tool::Nodes, "⬡ Nós"),
                        )
                        .clicked()
                    {
                        self.tool = Tool::Nodes;
                    }
                    if ui
                        .add_sized(
                            [80.0, 30.0],
                            egui::SelectableLabel::new(self.tool == Tool::Text, "T Texto"),
                        )
                        .clicked()
                    {
                        self.tool = Tool::Text;
                    }
                    if ui
                        .add_sized(
                            [80.0, 30.0],
                            egui::SelectableLabel::new(self.tool == Tool::Image, "🖼 Imagem"),
                        )
                        .on_hover_text(t(self.lang, "mover a imagem e a vetorização"))
                        .clicked()
                    {
                        self.tool = Tool::Image;
                    }
                    if ui
                        .add_sized(
                            [80.0, 30.0],
                            egui::SelectableLabel::new(self.tool == Tool::Pan, "✥ Mover"),
                        )
                        .clicked()
                    {
                        self.tool = Tool::Pan;
                    }
                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(6.0);
                    ui.checkbox(&mut self.show_art, t(self.lang, "🖼 Arte"));
                    ui.checkbox(&mut self.show_paths, t(self.lang, "✂ Corte"))
                        .on_hover_text(t(self.lang, "mostrar linhas de corte"));
                    ui.checkbox(&mut self.show_marks, t(self.lang, "⌖ Marcas"))
                        .on_hover_text(t(self.lang, "mostrar marcas de registo"));
                    ui.checkbox(&mut self.show_grid, t(self.lang, "▦ Grelha"));
                });
            });

        egui::SidePanel::right("side")
            .resizable(true)
            .default_width(320.0)
            .frame(glass_frame(ctx).inner_margin(egui::Margin::symmetric(10.0, 8.0)))
            .show(ctx, |ui| self.side_panel(ui));

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(egui::Color32::TRANSPARENT))
            .show(ctx, |ui| self.canvas(ui));

        self.help_window(ctx);
        self.confirm_window(ctx);
    }
}

impl App {
    fn canvas(&mut self, ui: &mut egui::Ui) {
        if self.doc.is_none() {
            ui.centered_and_justified(|ui| {
                ui.label("Nenhum documento aberto.\nUse Abrir (Ctrl+O).");
            });
            return;
        }
        let page = self.page();
        let (response, painter) =
            ui.allocate_painter(ui.available_size(), egui::Sense::click_and_drag());
        let rect = response.rect;
        self.canvas_rect = Some(rect);
        paint_body_gradient(&painter, rect, self.theme_dark);

        let fit = ((rect.width() - 48.0) / page.0).min((rect.height() - 48.0) / page.1);
        let scale = fit * self.zoom;
        let page_origin = rect.center() - egui::vec2(page.0, page.1) * scale * 0.5 + self.pan;

        if let Some(mouse) = response.hover_pos() {
            let (scroll, fine) = ui.input(|i| {
                (
                    i.raw_scroll_delta.y,
                    i.modifiers.ctrl || i.modifiers.command,
                )
            });
            if scroll != 0.0 {
                let before = (mouse - page_origin) / scale;
                let factor = (scroll * if fine { 0.00055 } else { 0.0016 }).exp();
                self.zoom = (self.zoom * factor).clamp(0.1, 40.0);
                let new_scale = fit * self.zoom;
                let new_origin = mouse - egui::vec2(before.x * new_scale, before.y * new_scale);
                self.pan += new_origin - page_origin;
            }
        }

        let page_rect =
            egui::Rect::from_min_size(page_origin, egui::vec2(page.0 * scale, page.1 * scale));
        for (grow, alpha) in [(22.0_f32, 8u8), (12.0, 10), (5.0, 14)] {
            painter.rect_filled(
                page_rect.expand(grow),
                7.0,
                egui::Color32::from_rgba_unmultiplied(30, 70, 120, alpha),
            );
        }
        painter.rect_filled(page_rect, 3.0, egui::Color32::WHITE);
        painter.rect_stroke(
            page_rect,
            3.0,
            egui::Stroke::new(
                1.0_f32,
                egui::Color32::from_rgba_unmultiplied(80, 120, 160, 160),
            ),
        );

        let to_screen =
            |p: egui::Pos2| egui::pos2(page_origin.x + p.x * scale, page_origin.y + p.y * scale);
        let to_mm = |p: egui::Pos2| {
            egui::pos2((p.x - page_origin.x) / scale, (p.y - page_origin.y) / scale)
        };

        if self.show_art {
            if let Some(doc) = &self.doc {
                for (idx, art) in doc.arts.iter().enumerate() {
                    if !art.visible {
                        continue;
                    }
                    let offset = doc.art_offsets.get(idx).copied().unwrap_or_default();
                    let source = art.rect.translate(offset);
                    let placed =
                        egui::Rect::from_min_max(self.place(source.min), self.place(source.max));
                    let screen =
                        egui::Rect::from_min_max(to_screen(placed.min), to_screen(placed.max));
                    painter.image(
                        art.texture.id(),
                        screen,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                    if self.tool == Tool::Image && self.selected_art && self.selected_art_idx == idx
                    {
                        painter.rect_stroke(
                            screen,
                            2.0,
                            egui::Stroke::new(1.6_f32, egui::Color32::from_rgb(0, 136, 255)),
                        );
                    }
                }
            }
        }

        if self.show_grid {
            let minor = if self.theme_dark {
                egui::Color32::from_rgba_unmultiplied(150, 190, 225, 40)
            } else {
                egui::Color32::from_rgba_unmultiplied(110, 155, 200, 34)
            };
            let major = if self.theme_dark {
                egui::Color32::from_rgba_unmultiplied(170, 205, 235, 80)
            } else {
                egui::Color32::from_rgba_unmultiplied(90, 140, 190, 70)
            };
            let mut step = self.grid_mm;
            while step * scale < 11.0 {
                step *= 2.0;
            }
            let mut i = 0usize;
            let mut x = 0.0;
            while x <= page.0 + 1e-4 {
                let sx = page_origin.x + x * scale;
                let thick = i % 5 == 0;
                painter.line_segment(
                    [
                        egui::pos2(sx, page_rect.min.y),
                        egui::pos2(sx, page_rect.max.y),
                    ],
                    egui::Stroke::new(
                        if thick { 0.7_f32 } else { 0.35_f32 },
                        if thick { major } else { minor },
                    ),
                );
                x += step;
                i += 1;
            }
            i = 0;
            let mut y = 0.0;
            while y <= page.1 + 1e-4 {
                let sy = page_origin.y + y * scale;
                let thick = i % 5 == 0;
                painter.line_segment(
                    [
                        egui::pos2(page_rect.min.x, sy),
                        egui::pos2(page_rect.max.x, sy),
                    ],
                    egui::Stroke::new(
                        if thick { 0.7_f32 } else { 0.35_f32 },
                        if thick { major } else { minor },
                    ),
                );
                y += step;
                i += 1;
            }
        }

        if self.show_marks {
            let spec = self.mark_spec(page);
            for shape in regmark_shapes(&spec) {
                let pts: Vec<egui::Pos2> = shape
                    .points
                    .iter()
                    .map(|(x, y)| to_screen(egui::pos2(*x, *y)))
                    .collect();
                if shape.filled {
                    painter.add(egui::Shape::convex_polygon(
                        pts,
                        egui::Color32::from_gray(20),
                        egui::Stroke::NONE,
                    ));
                } else {
                    painter.add(egui::Shape::line(
                        pts,
                        egui::Stroke::new(1.5_f32, egui::Color32::from_gray(20)),
                    ));
                }
            }
        }

        let pointer_mm = response.hover_pos().map(to_mm);
        self.hover_vertex = None;
        if let (Some(mm), Some(sel)) = (pointer_mm, self.only_selected()) {
            self.hover_vertex = self.hit_vertex(sel, mm, 7.0 / scale).map(|v| (sel, v));
        }

        if self.show_paths {
            if let Some(doc) = &self.doc {
                let budget = if self.preview {
                    self.preview_progress as usize
                } else {
                    usize::MAX
                };
                let mut spent = 0usize;
                for (i, layer) in doc.layers.iter().enumerate() {
                    if !layer.visible {
                        continue;
                    }
                    let mut pts: Vec<egui::Pos2> = layer
                        .points
                        .iter()
                        .map(|p| to_screen(self.place(*p)))
                        .collect();
                    if spent >= budget {
                        continue;
                    }
                    if pts.len() > budget - spent {
                        pts.truncate(budget - spent);
                    }
                    spent += pts.len();
                    if pts.len() < 2 {
                        continue;
                    }
                    let selected = self.selected.contains(&i);
                    let stroke = if selected {
                        egui::Stroke::new(2.2_f32, egui::Color32::from_rgb(20, 200, 140))
                    } else if self.preview {
                        egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(80, 170, 240))
                    } else {
                        egui::Stroke::new(1.3_f32, layer.color)
                    };
                    if selected {
                        let halo = egui::Stroke::new(
                            3.6_f32,
                            egui::Color32::from_rgba_unmultiplied(20, 200, 140, 90),
                        );
                        if layer.closed && pts.len() > 2 {
                            let mut closed = pts.clone();
                            closed.push(pts[0]);
                            painter.add(egui::Shape::line(closed.clone(), halo));
                            painter.add(egui::Shape::line(
                                closed,
                                egui::Stroke::new(1.5_f32, layer.color),
                            ));
                        } else {
                            painter.add(egui::Shape::line(pts.clone(), halo));
                            painter.add(egui::Shape::line(
                                pts,
                                egui::Stroke::new(1.5_f32, layer.color),
                            ));
                        }
                    } else if layer.closed && pts.len() > 2 {
                        let mut closed = pts.clone();
                        closed.push(pts[0]);
                        painter.add(egui::Shape::line(closed, stroke));
                    } else {
                        painter.add(egui::Shape::line(pts, stroke));
                    }
                }
            }
        }

        if let Some(preview) = &self.trace_preview {
            let color = egui::Color32::from_rgb(0, 150, 255);
            for path in preview {
                let pts: Vec<egui::Pos2> = path
                    .points
                    .iter()
                    .map(|(x, y)| to_screen(self.place(egui::pos2(*x, *y))))
                    .collect();
                if pts.len() < 2 {
                    continue;
                }
                if path.closed && pts.len() > 2 {
                    let mut closed = pts.clone();
                    closed.push(pts[0]);
                    painter.add(egui::Shape::line(closed, egui::Stroke::new(1.5_f32, color)));
                } else {
                    painter.add(egui::Shape::line(pts, egui::Stroke::new(1.5_f32, color)));
                }
            }
        }

        // Área pendente de «Vetorizar área».
        if let Some(band) = self.last_band {
            let a = to_screen(self.place(band.min));
            let b = to_screen(self.place(band.max));
            let r = egui::Rect::from_min_max(a, b);
            painter.rect_stroke(
                r,
                0.0,
                egui::Stroke::new(1.3_f32, egui::Color32::from_rgb(200, 60, 220)),
            );
            painter.text(
                r.left_bottom() + egui::vec2(0.0, 14.0),
                egui::Align2::LEFT_BOTTOM,
                format!("{:.0}×{:.0} mm", band.width(), band.height()),
                egui::FontId::proportional(12.0),
                egui::Color32::from_rgb(200, 60, 220),
            );
        }

        // Contornos fantasma do texto em digitação.
        if let Some(preview) = &self.text_preview {
            let color = egui::Color32::from_rgba_unmultiplied(120, 90, 200, 210);
            for points in preview {
                let pts: Vec<egui::Pos2> = points
                    .iter()
                    .map(|p| to_screen(self.place(*p)))
                    .collect();
                if pts.len() < 2 {
                    continue;
                }
                painter.add(egui::Shape::line(pts, egui::Stroke::new(1.2_f32, color)));
            }
        }

        if let Some(preview) = &self.offset_preview {
            let color = if preview.external {
                egui::Color32::from_rgb(230, 140, 20)
            } else {
                egui::Color32::from_rgb(150, 80, 210)
            };
            for (_, contours) in &preview.contours {
                for contour in contours {
                    let pts: Vec<egui::Pos2> =
                        contour.iter().map(|p| to_screen(self.place(*p))).collect();
                    if pts.len() < 2 {
                        continue;
                    }
                    let mut closed = pts.clone();
                    closed.push(pts[0]);
                    painter.add(egui::Shape::line(closed, egui::Stroke::new(1.8_f32, color)));
                }
            }
        }

        if self.offset_preview.is_some() {
            let (external, count, distance) = match self.offset_preview.as_ref() {
                Some(p) => (p.external, p.contours.len(), p.distance.abs()),
                None => (true, 0, 0.0),
            };
            let color = if external {
                egui::Color32::from_rgb(230, 140, 20)
            } else {
                egui::Color32::from_rgb(150, 80, 210)
            };
            let bar_pos = egui::pos2(rect.center().x - 150.0, rect.bottom() - 52.0);
            egui::Area::new(egui::Id::new("offset_preview_bar"))
                .order(egui::Order::Foreground)
                .fixed_pos(bar_pos)
                .show(ui.ctx(), |ui| {
                    egui::Frame::menu(ui.style()).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.colored_label(
                                color,
                                format!(
                                    "Offset {} · {distance:.1} mm · {count} contorno(s)",
                                    if external { "externo" } else { "interno" }
                                ),
                            );
                            if ui
                                .add(primary_button(egui::RichText::new("✓ Aplicar")))
                                .clicked()
                            {
                                self.apply_offset_preview();
                            }
                            if ui.button(t(self.lang, "✕ Cancelar")).clicked() {
                                self.cancel_offset();
                            }
                        });
                    });
                });
        }

        if self.tool == Tool::Nodes {
            if let (Some(doc), Some(sel)) = (self.doc.as_ref(), self.only_selected()) {
                if let Some(layer) = doc.layers.get(sel) {
                    let hovered = self.hover_vertex;
                    for (vi, p) in layer.points.iter().enumerate() {
                        let s = to_screen(self.place(*p));
                        let hot = hovered == Some((sel, vi));
                        painter.circle_filled(
                            s,
                            if hot { 5.0 } else { 3.5 },
                            if hot {
                                egui::Color32::from_rgb(255, 190, 40)
                            } else {
                                egui::Color32::WHITE
                            },
                        );
                        painter.circle_stroke(
                            s,
                            if hot { 5.0 } else { 3.5 },
                            egui::Stroke::new(1.0_f32, egui::Color32::from_gray(40)),
                        );
                    }
                }
            }
        }

        let show_handles = matches!(self.tool, Tool::Select | Tool::Nodes)
            && !self.space_down
            && !self.selected.is_empty();
        if show_handles {
            if let Some(bbox) = self.selection_bbox() {
                let r = egui::Rect::from_min_max(to_screen(bbox.min), to_screen(bbox.max));
                painter.rect_stroke(
                    r,
                    0.0,
                    egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(20, 170, 120)),
                );
                for (handle, pos) in App::handle_positions(bbox, scale) {
                    if handle == Handle::Rotate {
                        let rp = to_screen(pos);
                        painter.line_segment(
                            [egui::pos2(r.center().x, r.min.y), rp],
                            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(20, 170, 120)),
                        );
                        painter.circle_filled(rp, 4.5, egui::Color32::WHITE);
                        painter.circle_stroke(
                            rp,
                            4.5,
                            egui::Stroke::new(1.2_f32, egui::Color32::from_rgb(20, 150, 105)),
                        );
                    } else {
                        let sp = to_screen(pos);
                        let r = egui::Rect::from_center_size(sp, egui::vec2(6.0, 6.0));
                        painter.rect_filled(r, 1.0, egui::Color32::WHITE);
                        painter.rect_stroke(
                            r,
                            1.0,
                            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(20, 150, 105)),
                        );
                    }
                }
                if !self.space_down {
                    if let Some(hover) = response.hover_pos() {
                        if let Some(handle) = self.hit_handle(bbox, to_mm(hover), scale) {
                            ui.ctx().set_cursor_icon(match handle {
                                Handle::Nw | Handle::Se => egui::CursorIcon::ResizeNwSe,
                                Handle::Ne | Handle::Sw => egui::CursorIcon::ResizeNeSw,
                                Handle::N | Handle::S => egui::CursorIcon::ResizeVertical,
                                Handle::E | Handle::W => egui::CursorIcon::ResizeHorizontal,
                                Handle::Rotate => egui::CursorIcon::Grab,
                            });
                        }
                    }
                }
            }
        }

        let show_art_handles = self.selected_art
            && matches!(self.tool, Tool::Select | Tool::Image)
            && !self.space_down;
        if show_art_handles {
            if let Some(bbox) = self.art_bbox() {
                let r = egui::Rect::from_min_max(to_screen(bbox.min), to_screen(bbox.max));
                painter.rect_stroke(
                    r,
                    2.0,
                    egui::Stroke::new(1.6_f32, egui::Color32::from_rgb(0, 136, 255)),
                );
                for (_, pos) in App::art_handles(bbox) {
                    let sp = to_screen(pos);
                    let handle = egui::Rect::from_center_size(sp, egui::vec2(8.0, 8.0));
                    painter.rect_filled(handle, 1.5, egui::Color32::WHITE);
                    painter.rect_stroke(
                        handle,
                        1.5,
                        egui::Stroke::new(1.3_f32, egui::Color32::from_rgb(0, 120, 230)),
                    );
                }
                if let Some(hover) = response.hover_pos() {
                    if let Some(handle) = self.hit_art_handle(bbox, to_mm(hover), scale) {
                        ui.ctx().set_cursor_icon(match handle {
                            Handle::Nw | Handle::Se => egui::CursorIcon::ResizeNwSe,
                            Handle::Ne | Handle::Sw => egui::CursorIcon::ResizeNeSw,
                            Handle::N | Handle::S => egui::CursorIcon::ResizeVertical,
                            Handle::E | Handle::W => egui::CursorIcon::ResizeHorizontal,
                            Handle::Rotate => egui::CursorIcon::Grab,
                        });
                    }
                }
            }
        }

        if let Some(Drag::Band { start, current, .. }) = &self.drag {
            let a = to_screen(*start);
            let b = to_screen(*current);
            let band = egui::Rect::from_two_pos(a, b);
            painter.rect_filled(band, 0.0, egui::Color32::from_white_alpha(28));
            painter.rect_stroke(
                band,
                0.0,
                egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(20, 160, 210)),
            );
        }

        if let Some(origin) = self.text_origin {
            let s = to_screen(origin);
            painter.circle_stroke(
                s,
                6.0,
                egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(230, 120, 20)),
            );
            painter.line_segment(
                [s - egui::vec2(9.0, 0.0), s + egui::vec2(9.0, 0.0)],
                egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(230, 120, 20)),
            );
            painter.line_segment(
                [s - egui::vec2(0.0, 9.0), s + egui::vec2(0.0, 9.0)],
                egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(230, 120, 20)),
            );
        }

        let mut guide_xs = Vec::new();
        let mut guide_ys = Vec::new();
        for guide in &self.guides {
            match guide {
                Guide::V(x) => {
                    if guide_xs.iter().any(|v: &f32| (*v - *x).abs() < 1e-3) {
                        continue;
                    }
                    guide_xs.push(*x);
                    let sx = page_origin.x + x * scale;
                    painter.line_segment(
                        [
                            egui::pos2(sx, page_rect.min.y),
                            egui::pos2(sx, page_rect.max.y),
                        ],
                        egui::Stroke::new(1.2_f32, egui::Color32::from_rgb(235, 60, 180)),
                    );
                }
                Guide::H(y) => {
                    if guide_ys.iter().any(|v: &f32| (*v - *y).abs() < 1e-3) {
                        continue;
                    }
                    guide_ys.push(*y);
                    let sy = page_origin.y + y * scale;
                    painter.line_segment(
                        [
                            egui::pos2(page_rect.min.x, sy),
                            egui::pos2(page_rect.max.x, sy),
                        ],
                        egui::Stroke::new(1.2_f32, egui::Color32::from_rgb(235, 60, 180)),
                    );
                }
            }
        }
        for x in &guide_xs {
            for y in &guide_ys {
                let s = egui::pos2(page_origin.x + x * scale, page_origin.y + y * scale);
                let d = 3.5;
                painter.add(egui::Shape::convex_polygon(
                    vec![
                        s + egui::vec2(0.0, -d),
                        s + egui::vec2(d, 0.0),
                        s + egui::vec2(0.0, d),
                        s + egui::vec2(-d, 0.0),
                    ],
                    egui::Color32::from_rgb(235, 60, 180),
                    egui::Stroke::NONE,
                ));
            }
        }

        let primary = egui::PointerButton::Primary;
        let panning = self.space_down || self.tool == Tool::Pan;
        if panning {
            if response.dragged_by(primary) {
                self.pan += response.drag_delta();
            }
            if response.dragged_by(primary) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            } else if response.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
            }
        }
        if response.dragged_by(egui::PointerButton::Middle) {
            self.pan += response.drag_delta();
        }
        if self.tool == Tool::Image && !panning {
            let over_handle = self.selected_art
                && response
                    .hover_pos()
                    .map(|p| {
                        self.art_bbox()
                            .map(|b| self.hit_art_handle(b, to_mm(p), scale).is_some())
                            .unwrap_or(false)
                    })
                    .unwrap_or(false);
            if !over_handle {
                if response.dragged_by(primary) {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                } else if response.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                }
            }
        }

        let (alt, ctrl, shift) = ui.input(|i| {
            (
                i.modifiers.alt,
                i.modifiers.ctrl || i.modifiers.command,
                i.modifiers.shift,
            )
        });

        if !panning && response.drag_started_by(primary) {
            if let Some(pos) = response.interact_pointer_pos() {
                let mm = to_mm(pos);
                let mut started = false;
                if !started && self.selected_art && matches!(self.tool, Tool::Select | Tool::Image)
                {
                    if let Some(bbox) = self.art_bbox() {
                    if let Some(handle) = self.hit_art_handle(bbox, mm, scale) {
                        self.push_undo();
                        let anchor = Self::art_anchor(bbox, handle, alt || ctrl);
                        // Traced layers are independent — scaling the image
                        // scales only the image.
                        let originals = Vec::new();
                            self.drag = Some(Drag::ArtScale {
                                handle,
                                anchor,
                                start: bbox,
                                originals,
                                offset: self.offset_preview.clone(),
                                trace: self.trace_preview.clone(),
                            });
                            started = true;
                        }
                    }
                }
                if !started && matches!(self.tool, Tool::Select | Tool::Nodes) {
                    if let Some(bbox) = self.selection_bbox() {
                        if let Some(handle) = self.hit_handle(bbox, mm, scale) {
                            self.push_undo();
                            self.offset_preview = None;
                            let originals = self.selected_originals();
                            self.drag = Some(match handle {
                                Handle::Rotate => {
                                    let center = bbox.center();
                                    let start_angle = (mm.y - center.y).atan2(mm.x - center.x);
                                    Drag::Rotate {
                                        center,
                                        start_angle,
                                        originals,
                                    }
                                }
                                _ => Drag::Scale {
                                    handle,
                                    bbox,
                                    originals,
                                },
                            });
                            started = true;
                        }
                    }
                }
                if !started && self.tool == Tool::Nodes {
                    if let Some(sel) = self.only_selected() {
                        if let Some(v) = self.hit_vertex(sel, mm, 7.0 / scale) {
                            self.push_undo();
                            self.offset_preview = None;
                            self.drag = Some(Drag::Vertex {
                                layer: sel,
                                index: v,
                            });
                            started = true;
                        }
                    }
                }
                if !started && self.tool == Tool::Image {
                    // Clicking an image makes it the one being dragged/edited.
                    if let Some(idx) = self.art_hit_at(mm) {
                        self.selected_art_idx = idx;
                    }
                    if let Some((art_start, originals)) = self.art_hit(mm) {
                        self.push_undo();
                        self.selected_art = true;
                        self.selected.clear();
                        self.drag = Some(Drag::Art {
                            start: mm,
                            art_start,
                            originals,
                            offset: self.offset_preview.clone(),
                            trace: self.trace_preview.clone(),
                        });
                        started = true;
                    }
                }
                if !started {
                    if let Some(layer) = self.hit_layer(mm, 6.0 / scale) {
                        if !self.selected.contains(&layer) {
                            let members = self.group_members(layer);
                            self.select_indices(members, ctrl);
                        }
                        self.push_undo();
                        self.offset_preview = None;
                        let originals = self.selected_originals();
                        let bbox = self
                            .selection_bbox()
                            .unwrap_or(egui::Rect::from_min_size(mm, egui::Vec2::ZERO));
                        self.drag = Some(Drag::Move {
                            start: mm,
                            bbox,
                            originals,
                        });
                    } else if self.tool == Tool::Select {
                        if let Some(idx) = self.art_hit_at(mm) {
                            self.selected_art_idx = idx;
                        }
                        if let Some((art_start, originals)) = self.art_hit(mm) {
                            self.push_undo();
                            self.selected_art = true;
                            self.selected.clear();
                            self.drag = Some(Drag::Art {
                                start: mm,
                                art_start,
                                originals,
                                offset: self.offset_preview.clone(),
                                trace: self.trace_preview.clone(),
                            });
                        } else {
                            self.drag = Some(Drag::Band {
                                start: mm,
                                current: mm,
                                additive: ctrl,
                            });
                        }
                    } else {
                        self.drag = Some(Drag::Band {
                            start: mm,
                            current: mm,
                            additive: ctrl,
                        });
                    }
                }
            }
        }

        if let (Some(drag), Some(pos)) = (self.drag.clone(), response.interact_pointer_pos()) {
            let mm = to_mm(pos);
            let threshold = (8.0 / scale).max(0.4);
            match drag {
                Drag::Vertex { layer, index } => {
                    let (snapped, guides) = self.snap_point(mm, &[layer], threshold);
                    self.guides = guides;
                    let p = self.unplace(snapped);
                    if let Some(doc) = &mut self.doc {
                        if let Some(l) = doc.layers.get_mut(layer) {
                            if index < l.points.len() {
                                l.points[index] = p;
                            }
                        }
                    }
                }
                Drag::Move {
                    start,
                    bbox,
                    originals,
                } => {
                    let raw = mm - start;
                    let exclude: Vec<usize> = originals.iter().map(|(i, _)| *i).collect();
                    let (delta, guides) = self.snap_move(bbox, raw, &exclude, threshold, true);
                    self.guides = guides;
                    let d = delta / self.place_scale;
                    if let Some(doc) = &mut self.doc {
                        for (idx, points) in &originals {
                            if let Some(layer) = doc.layers.get_mut(*idx) {
                                layer.points = points.iter().map(|p| *p + d).collect();
                            }
                        }
                    }
                }
                Drag::Scale {
                    handle,
                    bbox,
                    originals,
                } => {
                    let from_center = alt || ctrl;
                    self.apply_scale(&originals, bbox, handle, mm, from_center, shift);
                }
                Drag::Rotate {
                    center,
                    start_angle,
                    originals,
                } => {
                    let current = (mm.y - center.y).atan2(mm.x - center.x);
                    let mut delta = current - start_angle;
                    if shift {
                        let step = std::f32::consts::FRAC_PI_4;
                        delta = (delta / step).round() * step;
                    }
                    self.apply_rotation(&originals, center, delta);
                }
                Drag::Band {
                    start, additive, ..
                } => {
                    self.drag = Some(Drag::Band {
                        start,
                        current: mm,
                        additive,
                    });
                }
                Drag::Art {
                    start,
                    art_start,
                    originals,
                    offset,
                    trace,
                } => {
                    let raw = mm - start;
                    let bbox = self.doc.as_ref().and_then(|doc| {
                        doc.arts
                            .get(self.selected_art_idx)
                            .map(|art| art.rect.translate(art_start))
                    });
                    let (delta, guides) = match bbox {
                        Some(bbox) => self.snap_move(bbox, raw, &[], threshold, false),
                        None => (raw, Vec::new()),
                    };
                    self.guides = guides;
                    if let Some(doc) = &mut self.doc {
                        doc.art_offsets[self.selected_art_idx] = art_start + delta;
                        for (idx, points) in &originals {
                            if let Some(layer) = doc.layers.get_mut(*idx) {
                                layer.points = points.iter().map(|p| *p + delta).collect();
                            }
                        }
                    }
                    self.offset_preview = offset.clone().map(|mut preview| {
                        for (_, contours) in &mut preview.contours {
                            for contour in contours {
                                for p in contour {
                                    *p += delta;
                                }
                            }
                        }
                        preview
                    });
                    self.trace_preview = trace.clone().map(|mut preview| {
                        for path in &mut preview {
                            for p in &mut path.points {
                                p.0 += delta.x;
                                p.1 += delta.y;
                            }
                        }
                        preview
                    });
                }
                Drag::ArtScale {
                    handle,
                    anchor,
                    start,
                    originals,
                    offset,
                    trace,
                } => {
                    self.guides.clear();
                    let c = start.center();
                    let corner = match handle {
                        Handle::Nw => start.min,
                        Handle::N => egui::pos2(c.x, start.min.y),
                        Handle::Ne => egui::pos2(start.max.x, start.min.y),
                        Handle::E => egui::pos2(start.max.x, c.y),
                        Handle::Se => start.max,
                        Handle::S => egui::pos2(c.x, start.max.y),
                        Handle::Sw => egui::pos2(start.min.x, start.max.y),
                        Handle::W => egui::pos2(start.min.x, c.y),
                        Handle::Rotate => start.max,
                    };
                    let v0 = corner - anchor;
                    let v = mm - anchor;
                    let mut sx = if v0.x.abs() > 1e-4 { v.x / v0.x } else { 1.0 };
                    let mut sy = if v0.y.abs() > 1e-4 { v.y / v0.y } else { 1.0 };
                    let uniform =
                        !shift && !matches!(handle, Handle::N | Handle::S | Handle::E | Handle::W);
                    if uniform {
                        let len2 = v0.x * v0.x + v0.y * v0.y;
                        let s = if len2 > 1e-6 {
                            (v.x * v0.x + v.y * v0.y) / len2
                        } else {
                            1.0
                        };
                        sx = s;
                        sy = s;
                    }
                    sx = sx.clamp(0.01, 100.0);
                    sy = sy.clamp(0.01, 100.0);
                    if let Some(doc) = &mut self.doc {
                        // `start`/`anchor` are in placed (page) coordinates; the
                        // stored rect is not — remove the offset before storing,
                        // otherwise it is applied twice and the image drifts
                        // away from its selection box on every scale.
                        let off = doc
                            .art_offsets
                            .get(self.selected_art_idx)
                            .copied()
                            .unwrap_or_default();
                        if let Some(art) = doc.arts.get_mut(self.selected_art_idx) {
                            art.rect = scale_rect(start, anchor, sx, sy).translate(-off);
                        }
                        for (idx, points) in &originals {
                            if let Some(layer) = doc.layers.get_mut(*idx) {
                                layer.points = points
                                    .iter()
                                    .map(|p| scale_point(*p, anchor, sx, sy))
                                    .collect();
                            }
                        }
                    }
                    self.offset_preview = offset.clone().map(|mut preview| {
                        for (_, contours) in &mut preview.contours {
                            for contour in contours {
                                for p in contour {
                                    *p = scale_point(*p, anchor, sx, sy);
                                }
                            }
                        }
                        preview
                    });
                    self.trace_preview = trace.clone().map(|mut preview| {
                        for path in &mut preview {
                            for p in &mut path.points {
                                let q = scale_point(egui::pos2(p.0, p.1), anchor, sx, sy);
                                p.0 = q.x;
                                p.1 = q.y;
                            }
                        }
                        preview
                    });
                }
            }
        }

        if response.drag_stopped() {
            if let Some(Drag::Band {
                start,
                current,
                additive,
            }) = self.drag.clone()
            {
                if (current - start).length() < 1.0 {
                    if !additive {
                        self.selected.clear();
                    }
                } else {
                    let band = egui::Rect::from_two_pos(start, current);
                    self.last_band = Some(band);
                    let mut hits = Vec::new();
                    if let Some(doc) = &self.doc {
                        for (i, layer) in doc.layers.iter().enumerate() {
                            if !layer.visible {
                                continue;
                            }
                            if let Some(r) = layer.rect_placed(self.place_scale, self.place_offset)
                            {
                                if band.intersects(r) {
                                    hits.push(i);
                                }
                            }
                        }
                    }
                    self.select_indices(hits, additive);
                }
            }
            let art_scaled = matches!(self.drag, Some(Drag::ArtScale { .. }));
            self.drag = None;
            self.guides.clear();
            if art_scaled && self.trace_preview.is_some() {
                self.request_trace_preview();
            }
        }

        if self.tool == Tool::Nodes && response.double_clicked_by(primary) {
            if let Some(pos) = response.interact_pointer_pos() {
                let mm = to_mm(pos);
                if let Some(sel) = self.only_selected() {
                    if self.add_vertex(sel, mm, 6.0 / scale) {
                        self.status = "ponto adicionado".to_string();
                    }
                }
            }
        }

        if self.tool == Tool::Nodes && response.secondary_clicked() {
            if let Some(pos) = response.interact_pointer_pos() {
                let mm = to_mm(pos);
                if let Some(sel) = self.only_selected() {
                    if let Some(v) = self.hit_vertex(sel, mm, 7.0 / scale) {
                        self.delete_vertex(sel, v);
                        self.status = "ponto removido".to_string();
                    }
                }
            }
        }

        if response.clicked() {
            if let Some(pos) = response.interact_pointer_pos() {
                let mm = to_mm(pos);
                if self.tool == Tool::Text {
                    self.text_origin = Some(mm);
                    self.status = "origem do texto definida".to_string();
                } else if let Some(layer) = self.hit_layer(mm, 6.0 / scale) {
                    let members = self.group_members(layer);
                    if ctrl {
                        if self.selected.contains(&layer) {
                            self.selected.retain(|i| !members.contains(i));
                        } else {
                            for i in members {
                                if !self.selected.contains(&i) {
                                    self.selected.push(i);
                                }
                            }
                            self.selected.sort_unstable();
                        }
                    } else {
                        self.select_indices(members, false);
                    }
                } else if matches!(self.tool, Tool::Select | Tool::Image)
                    && self.art_hit(mm).is_some()
                {
                    // Left-click selects the clicked image (not just the last
                    // right-clicked one).
                    if let Some(idx) = self.art_hit_at(mm) {
                        self.selected_art_idx = idx;
                    }
                    self.selected.clear();
                    self.selected_art = true;
                    self.status = "imagem selecionada — arraste os cantos para escalar".to_string();
                } else {
                    let on_handle = self
                        .selection_bbox()
                        .map(|b| self.hit_handle(b, mm, scale).is_some())
                        .unwrap_or(false)
                        || (self.selected_art
                            && self
                                .art_bbox()
                                .map(|b| self.hit_art_handle(b, mm, scale).is_some())
                                .unwrap_or(false));
                    if !ctrl && !on_handle {
                        self.selected.clear();
                        self.selected_art = false;
                    }
                }
            }
        }

        if response.double_clicked() && self.tool != Tool::Nodes {
            self.zoom = 1.0;
            self.pan = egui::Vec2::ZERO;
        }

        if response.secondary_clicked() {
            if let Some(pos) = response.interact_pointer_pos() {
                let mm = to_mm(pos);
                if let Some(idx) = self.art_hit_at(mm) {
                    self.selected_art = true;
                    self.selected_art_idx = idx;
                    self.show_art_context_menu = true;
                    self.art_context_pos = Some(mm);
                } else if let Some(layer_idx) = self.hit_layer(mm, 7.0 / scale) {
                    self.show_layer_context_menu = true;
                    self.layer_context_idx = Some(layer_idx);
                } else if !self.selected.is_empty() {
                    self.show_selection_context_menu = true;
                    self.selection_context_pos = Some(mm);
                }
            }
        }

        if self.show_art_context_menu {
            let pos = self.art_context_pos.unwrap_or_default();
            egui::Area::new(egui::Id::new("art_context_menu"))
                .order(egui::Order::Foreground)
                .fixed_pos(pos)
                .show(ui.ctx(), |ui| {
                    egui::Frame::menu(ui.style()).show(ui, |ui| {
                        ui.set_min_width(200.0);
                        if ui.button(t(self.lang, "📋 Copiar imagem")).clicked() {
                            if let Some(doc) = &mut self.doc {
                                if let Some(art) = doc.arts.get(self.selected_art_idx) {
                                    self.clipboard_image = Some(art.clone());
                                    self.status = "imagem copiada".to_string();
                                }
                            }
                            self.show_art_context_menu = false;
                        }
                        if ui.button(t(self.lang, "📎 Colar imagem do clipboard")).clicked() {
                            if let Some(clip) = self.clipboard_image.take() {
                                if let Some(doc) = &mut self.doc {
                                    doc.arts.push(clip);
                                    doc.art_offsets.push(egui::Vec2::ZERO);
                                    self.selected_art = true;
                                    self.selected_art_idx = doc.arts.len() - 1;
                                    self.selected.clear();
                                    self.status = "imagem colada".to_string();
                                }
                            }
                            self.show_art_context_menu = false;
                        }
                        ui.separator();
                        if ui.button(t(self.lang, "🔄 Espelhar horizontal")).clicked() {
                            if let Some(doc) = &mut self.doc {
                                if let Some(art) = doc.arts.get_mut(self.selected_art_idx) {
                                    let cx = art.rect.center().x;
                                    art.rect = art.rect.translate(egui::vec2(-cx * 2.0, 0.0));
                                    doc.art_offsets[self.selected_art_idx].x =
                                        -doc.art_offsets[self.selected_art_idx].x;
                                }
                            }
                            self.show_art_context_menu = false;
                        }
                        if ui.button(t(self.lang, "🔄 Espelhar vertical")).clicked() {
                            if let Some(doc) = &mut self.doc {
                                if let Some(art) = doc.arts.get_mut(self.selected_art_idx) {
                                    let cy = art.rect.center().y;
                                    art.rect = art.rect.translate(egui::vec2(0.0, -cy * 2.0));
                                    doc.art_offsets[self.selected_art_idx].y =
                                        -doc.art_offsets[self.selected_art_idx].y;
                                }
                            }
                            self.show_art_context_menu = false;
                        }
                        if ui.button(t(self.lang, "↻ Girar 90°")).clicked() {
                            if let Some(doc) = &mut self.doc {
                                if let Some(art) = doc.arts.get_mut(self.selected_art_idx) {
                                    let w = art.rect.width();
                                    let h = art.rect.height();
                                    art.rect =
                                        egui::Rect::from_min_size(art.rect.min, egui::vec2(h, w));
                                }
                            }
                            self.show_art_context_menu = false;
                        }
                        ui.separator();
                        if ui.button(t(self.lang, "🖼 Rasterizar (criar traços de corte)")).clicked() {
                            self.trace_with_mode(TraceMethod::Edges);
                            self.show_art_context_menu = false;
                        }
                        if ui.button(t(self.lang, "✂️ Silhueta (contorno externo)")).clicked() {
                            self.trace_with_mode(TraceMethod::Outline);
                            self.show_art_context_menu = false;
                        }
                        ui.separator();
                        if ui.button(t(self.lang, "⤴ Offset externo da imagem")).clicked() {
                            self.offset_from_image(true);
                            self.show_art_context_menu = false;
                        }
                        if ui.button(t(self.lang, "⤵ Offset interno da imagem")).clicked() {
                            self.offset_from_image(false);
                            self.show_art_context_menu = false;
                        }
                        if self.offset_mm.abs() < 0.05 {
                            ui.small(t(self.lang, "Ajuste a distância do offset no painel «Vetorizar»."));
                        }
                        ui.separator();
                        if ui.button(t(self.lang, "🗑️ Remover imagem")).clicked() {
                            if let Some(doc) = &mut self.doc {
                                if self.selected_art_idx < doc.arts.len() {
                                    doc.arts.remove(self.selected_art_idx);
                                    doc.art_offsets.remove(self.selected_art_idx);
                                    if doc.arts.is_empty() {
                                        self.selected_art = false;
                                        self.selected_art_idx = 0;
                                    } else if self.selected_art_idx >= doc.arts.len() {
                                        self.selected_art_idx = doc.arts.len() - 1;
                                    }
                                }
                                self.status = "imagem removida".to_string();
                            }
                            self.show_art_context_menu = false;
                        }
                    });
                });

            if ui.input(|i| i.pointer.primary_clicked()) {
                self.show_art_context_menu = false;
            }
        }

        if self.show_layer_context_menu {
            if let Some(idx) = self.layer_context_idx {
                egui::Area::new(egui::Id::new("layer_context_menu"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(self.art_context_pos.unwrap_or_default())
                    .show(ui.ctx(), |ui| {
                        egui::Frame::menu(ui.style()).show(ui, |ui| {
                            ui.set_min_width(200.0);
                            if ui.button(t(self.lang, "✎ Editar nós")).clicked() {
                                self.selected = vec![idx];
                                self.expand_selection();
                                self.tool = Tool::Nodes;
                                self.status = format!("a editar nós da camada #{idx:02}");
                                self.show_layer_context_menu = false;
                            }
                            if ui
                                .button(t(self.lang, "⌒ Arredondar camada"))
                                .on_hover_text(t(self.lang,
                                    "Simplifica esta linha de forma arredondada (tolerância do «Simplif.»)"))
                                .clicked()
                            {
                                self.selected = vec![idx];
                                self.expand_selection();
                                self.round_selection();
                                self.show_layer_context_menu = false;
                            }
                            let layer_info = self
                                .doc
                                .as_ref()
                                .and_then(|d| d.layers.get(idx))
                                .map(|l| (l.traced, l.art.is_some(), l.group));
                            if let Some((traced, attached, group)) = layer_info {
                                if traced && attached {
                                    if ui
                                        .button(t(self.lang, "🔓 Separar da imagem"))
                                        .on_hover_text(t(self.lang,
                                            "A linha deixa de acompanhar a imagem e vira independente"))
                                        .clicked()
                                    {
                                        self.push_undo();
                                        if let Some(doc) = &mut self.doc {
                                            if let Some(l) = doc.layers.get_mut(idx) {
                                                l.art = None;
                                            }
                                        }
                                        self.status =
                                            "camada separada da imagem".to_string();
                                        self.show_layer_context_menu = false;
                                    }
                                }
                                if let Some(gid) = group {
                                    if ui
                                        .button(t(self.lang, "✂ Separar objetos"))
                                        .on_hover_text(t(self.lang,
                                            "Desagrupa: cada parte do vetor fica selecionável sozinha"))
                                        .clicked()
                                    {
                                        self.push_undo();
                                        if let Some(doc) = &mut self.doc {
                                            for l in doc.layers.iter_mut() {
                                                if l.group == Some(gid) {
                                                    l.group = None;
                                                }
                                            }
                                        }
                                        self.status =
                                            "vetor separado em objetos individuais".to_string();
                                        self.show_layer_context_menu = false;
                                    }
                                }
                            }
                            if ui.button(t(self.lang, "📋 Copiar camada")).clicked() {
                                if let Some(doc) = &self.doc {
                                    if let Some(layer) = doc.layers.get(idx) {
                                        self.clipboard_layer = Some(layer.clone());
                                        self.status = "camada copiada".to_string();
                                    }
                                }
                                self.show_layer_context_menu = false;
                            }
                            if ui.button(t(self.lang, "📎 Colar camada")).clicked() {
                                if let Some(layer) = self.clipboard_layer.take() {
                                    if let Some(doc) = &mut self.doc {
                                        doc.layers.push(layer);
                                        self.selected = vec![doc.layers.len() - 1];
                                    }
                                }
                                self.show_layer_context_menu = false;
                            }
                            ui.separator();
                            if ui.button(t(self.lang, "🔄 Espelhar H")).clicked() {
                                if let Some(doc) = &mut self.doc {
                                    if let Some(layer) = doc.layers.get_mut(idx) {
                                        let cx = layer.points.iter().map(|p| p.x).sum::<f32>()
                                            / layer.points.len().max(1) as f32;
                                        for p in &mut layer.points {
                                            p.x = 2.0 * cx - p.x;
                                        }
                                    }
                                }
                                self.show_layer_context_menu = false;
                            }
                            if ui.button(t(self.lang, "🔄 Espelhar V")).clicked() {
                                if let Some(doc) = &mut self.doc {
                                    if let Some(layer) = doc.layers.get_mut(idx) {
                                        let cy = layer.points.iter().map(|p| p.y).sum::<f32>()
                                            / layer.points.len().max(1) as f32;
                                        for p in &mut layer.points {
                                            p.y = 2.0 * cy - p.y;
                                        }
                                    }
                                }
                                self.show_layer_context_menu = false;
                            }
                            if ui.button(t(self.lang, "↻ Girar 90°")).clicked() {
                                if let Some(doc) = &mut self.doc {
                                    if let Some(layer) = doc.layers.get_mut(idx) {
                                        let cx = layer.points.iter().map(|p| p.x).sum::<f32>()
                                            / layer.points.len().max(1) as f32;
                                        let cy = layer.points.iter().map(|p| p.y).sum::<f32>()
                                            / layer.points.len().max(1) as f32;
                                        for p in &mut layer.points {
                                            let dx = p.x - cx;
                                            let dy = p.y - cy;
                                            p.x = cx - dy;
                                            p.y = cy + dx;
                                        }
                                    }
                                }
                                self.show_layer_context_menu = false;
                            }
                            ui.separator();
                            if ui.button(t(self.lang, "⤴ Offset externo")).clicked() {
                                self.selected = vec![idx];
                                self.preview_offset(true);
                                self.show_layer_context_menu = false;
                            }
                            if ui.button(t(self.lang, "⤵ Offset interno")).clicked() {
                                self.selected = vec![idx];
                                self.preview_offset(false);
                                self.show_layer_context_menu = false;
                            }
                            ui.separator();
                            if ui.button(t(self.lang, "🗑️ Apagar camada")).clicked() {
                                self.push_undo();
                                if let Some(doc) = &mut self.doc {
                                    if idx < doc.layers.len() {
                                        doc.layers.remove(idx);
                                    }
                                }
                                self.selected.retain(|x| *x != idx);
                                self.show_layer_context_menu = false;
                            }
                        });
                    });

                if ui.input(|i| i.pointer.primary_clicked()) {
                    self.show_layer_context_menu = false;
                }
            }
        }

        if self.show_selection_context_menu {
            egui::Area::new(egui::Id::new("selection_context_menu"))
                .order(egui::Order::Foreground)
                .fixed_pos(self.selection_context_pos.unwrap_or_default())
                .show(ui.ctx(), |ui| {
                    egui::Frame::menu(ui.style()).show(ui, |ui| {
                        ui.set_min_width(200.0);
                        let has_attached = self.doc.as_ref().is_some_and(|d| {
                            self.selected
                                .iter()
                                .any(|i| {
                                    d.layers
                                        .get(*i)
                                        .map(|l| l.traced && l.art.is_some())
                                        .unwrap_or(false)
                                })
                        });
                        if has_attached
                            && ui
                                .button(t(self.lang, "🔓 Separar da imagem"))
                                .on_hover_text(t(self.lang,
                                    "As linhas deixam de acompanhar a imagem"))
                                .clicked()
                            {
                                self.push_undo();
                                if let Some(doc) = &mut self.doc {
                                    for i in &self.selected {
                                        if let Some(l) = doc.layers.get_mut(*i) {
                                            l.art = None;
                                        }
                                    }
                                }
                                self.status = "linhas separadas da imagem".to_string();
                                self.show_selection_context_menu = false;
                            }
                        if ui
                            .button(t(self.lang, "⌒ Arredondar seleção"))
                            .on_hover_text(t(self.lang,
                                "Simplifica as linhas selecionadas de forma arredondada (tolerância do «Simplif.»)"))
                            .clicked()
                        {
                            self.round_selection();
                            self.show_selection_context_menu = false;
                        }
                        if ui
                            .button(t(self.lang, "✂ Manter só dentro da área"))
                            .on_hover_text(t(self.lang, "Recorta as linhas selecionadas à área arrastada"))
                            .clicked()
                        {
                            self.clip_layers_to_selection(true);
                            self.show_selection_context_menu = false;
                        }
                        if ui
                            .button(t(self.lang, "🗑 Apagar dentro da área"))
                            .on_hover_text(t(self.lang, "Remove as partes dentro da área arrastada"))
                            .clicked()
                        {
                            self.clip_layers_to_selection(false);
                            self.show_selection_context_menu = false;
                        }
                        if ui.button(t(self.lang, "📋 Copiar seleção")).clicked() {
                            if let Some(doc) = &self.doc {
                                let layers: Vec<_> = self
                                    .selected
                                    .iter()
                                    .filter_map(|i| doc.layers.get(*i).cloned())
                                    .collect();
                                if !layers.is_empty() {
                                    self.clipboard_layers = layers;
                                    self.status = "seleção copiada".to_string();
                                }
                            }
                            self.show_selection_context_menu = false;
                        }
                        if ui.button(t(self.lang, "📎 Colar seleção")).clicked() {
                            if !self.clipboard_layers.is_empty() {
                                self.push_undo();
                                if let Some(doc) = &mut self.doc {
                                    for layer in self.clipboard_layers.drain(..) {
                                        doc.layers.push(layer);
                                    }
                                    self.selected = (doc.layers.len() - self.clipboard_layers.len()
                                        ..doc.layers.len())
                                        .collect();
                                }
                            }
                            self.show_selection_context_menu = false;
                        }
                        ui.separator();
                        if ui.button(t(self.lang, "🔗 Agrupar")).clicked() {
                            self.group_selected();
                            self.show_selection_context_menu = false;
                        }
                        if ui.button(t(self.lang, "🔓 Desagrupar")).clicked() {
                            self.ungroup_selected();
                            self.show_selection_context_menu = false;
                        }
                        ui.separator();
                        if ui.button(t(self.lang, "🔄 Espelhar H")).clicked() {
                            self.mirror_selection(true);
                            self.show_selection_context_menu = false;
                        }
                        if ui.button(t(self.lang, "🔄 Espelhar V")).clicked() {
                            self.mirror_selection(false);
                            self.show_selection_context_menu = false;
                        }
                        if ui.button(t(self.lang, "↻ Girar 90°")).clicked() {
                            self.rotate_selection(90.0);
                            self.show_selection_context_menu = false;
                        }
                        ui.separator();
                        if ui.button(t(self.lang, "⤴ Offset externo da seleção")).clicked() {
                            self.preview_offset(true);
                            self.show_selection_context_menu = false;
                        }
                        if ui.button(t(self.lang, "⤵ Offset interno da seleção")).clicked() {
                            self.preview_offset(false);
                            self.show_selection_context_menu = false;
                        }
                        ui.separator();
                        if ui.button(t(self.lang, "🗑️ Apagar seleção")).clicked() {
                            self.delete_selected();
                            self.show_selection_context_menu = false;
                        }
                    });
                });

            if ui.input(|i| i.pointer.primary_clicked()) {
                self.show_selection_context_menu = false;
            }
        }
    }

    fn side_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Projeto").heading());
        });
        ui.separator();

        egui::ScrollArea::vertical().show(ui, |ui| {
            if let Some(doc) = &self.doc {
                egui::Grid::new("docinfo")
                    .num_columns(2)
                    .spacing([10.0, 3.0])
                    .show(ui, |ui| {
                        ui.label(t(self.lang, "Ficheiro"));
                        ui.label(
                            doc.path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_default(),
                        );
                        ui.end_row();
                        ui.label(t(self.lang, "Página"));
                        ui.label(format!("{:.1} × {:.1} mm", doc.page.0, doc.page.1));
                        ui.end_row();
                        ui.label(t(self.lang, "Traços"));
                        ui.label(doc.layers.len().to_string());
                        ui.end_row();
                        ui.label(t(self.lang, "Arte"));
                        ui.label(
                            doc.arts
                                .first()
                                .map(|a| format!("{} · {}×{} px", a.name, a.width, a.height))
                                .unwrap_or_else(|| "—".to_string()),
                        );
                        ui.end_row();
                    });
                for note in &doc.notes {
                    ui.small(note);
                }
            }

            ui.add_space(4.0);
            egui::CollapsingHeader::new(t(self.lang, "Alinhar"))
                .default_open(true)
                .show(ui, |ui| {
                    let enabled = !self.selected.is_empty() || self.selected_art;
                    ui.horizontal(|ui| {
                        ui.label(t(self.lang, "Página"));
                        if ui
                            .add_enabled(enabled, egui::Button::new("Esq"))
                            .on_hover_text(t(self.lang, "encostar à esquerda da página"))
                            .clicked()
                        {
                            self.align_page(Align::Left);
                        }
                        if ui
                            .add_enabled(enabled, egui::Button::new("C.H"))
                            .on_hover_text(t(self.lang, "centrar na horizontal da página"))
                            .clicked()
                        {
                            self.align_page(Align::CenterH);
                        }
                        if ui
                            .add_enabled(enabled, egui::Button::new("Dir"))
                            .on_hover_text(t(self.lang, "encostar à direita da página"))
                            .clicked()
                        {
                            self.align_page(Align::Right);
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(t(self.lang, "Página"));
                        if ui
                            .add_enabled(enabled, egui::Button::new("Topo"))
                            .on_hover_text(t(self.lang, "encostar ao topo da página"))
                            .clicked()
                        {
                            self.align_page(Align::Top);
                        }
                        if ui
                            .add_enabled(enabled, egui::Button::new("C.V"))
                            .on_hover_text(t(self.lang, "centrar na vertical da página"))
                            .clicked()
                        {
                            self.align_page(Align::CenterV);
                        }
                        if ui
                            .add_enabled(enabled, egui::Button::new("Base"))
                            .on_hover_text(t(self.lang, "encostar à base da página"))
                            .clicked()
                        {
                            self.align_page(Align::Bottom);
                        }
                    });
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(enabled, egui::Button::new("Centrar no canvas"))
                            .clicked()
                        {
                            self.center_on_canvas();
                        }
                        if ui
                            .add_enabled(enabled, egui::Button::new("Centrar entre si"))
                            .clicked()
                        {
                            self.align_relative(Align::CenterH);
                            self.align_relative(Align::CenterV);
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(t(self.lang, "Entre si"));
                        if ui
                            .add_enabled(enabled, egui::Button::new("Esq"))
                            .clicked()
                        {
                            self.align_relative(Align::Left);
                        }
                        if ui
                            .add_enabled(enabled, egui::Button::new("Dir"))
                            .clicked()
                        {
                            self.align_relative(Align::Right);
                        }
                        if ui
                            .add_enabled(enabled, egui::Button::new("Topo"))
                            .clicked()
                        {
                            self.align_relative(Align::Top);
                        }
                        if ui
                            .add_enabled(enabled, egui::Button::new("Base"))
                            .clicked()
                        {
                            self.align_relative(Align::Bottom);
                        }
                    });
                });

            ui.separator();
            egui::CollapsingHeader::new(t(self.lang, "Camadas"))
                .default_open(false)
                .show(ui, |ui| {
                    let mut selected = self.selected.clone();
                    let mut delete = None;
                    let mut reset = None;
                    let mut clicked_layer = false;
                    let mut edit_nodes = None;
                    let (mut art_select, mut art_move, mut art_delete) = (None, None, None);
                    if let Some(doc) = &mut self.doc {
                        // Imagens: cada imagem é uma camada própria — olho,
                        // seleção, ordem e apagar, independente das outras.
                        ui.small(t(self.lang, "imagens"));
                        // Copiar (índice, nome, visível) antes: o loop abaixo
                        // muta `doc.arts` pelo olho.
                        let arts_meta: Vec<(usize, String, bool)> = doc
                            .arts
                            .iter()
                            .enumerate()
                            .map(|(i, a)| {
                                (
                                    i,
                                    if a.name.is_empty() {
                                        format!("imagem {i}")
                                    } else {
                                        a.name.clone()
                                    },
                                    a.visible,
                                )
                            })
                            .collect();
                        for (i, name, visible) in arts_meta {
                            let mut vis = visible;
                            let sel = self.selected_art && self.selected_art_idx == i;
                            ui.horizontal(|ui| {
                                if ui
                                    .checkbox(&mut vis, "")
                                    .on_hover_text(t(self.lang, "mostrar/esconder imagem"))
                                    .changed()
                                {
                                    if let Some(a) = doc.arts.get_mut(i) {
                                        a.visible = vis;
                                    }
                                }
                                let label =
                                    ui.selectable_label(sel, egui::RichText::new(name).size(12.0));
                                if label.clicked() {
                                    art_select = Some(i);
                                }
                                if sel {
                                    if ui
                                        .small_button("▲")
                                        .on_hover_text(t(self.lang, "trazer para frente"))
                                        .clicked()
                                        && i > 0
                                    {
                                        art_move = Some((i, -1));
                                    }
                                    if ui
                                        .small_button("▼")
                                        .on_hover_text(t(self.lang, "enviar para trás"))
                                        .clicked()
                                    {
                                        art_move = Some((i, 1));
                                    }
                                    if ui
                                        .small_button("×")
                                        .on_hover_text(t(self.lang, "apagar imagem e os traços dela"))
                                        .clicked()
                                    {
                                        art_delete = Some(i);
                                    }
                                }
                            });
                        }
                        ui.separator();
                        egui::ScrollArea::vertical().max_height(210.0).show(ui, |ui| {
                            for (i, layer) in doc.layers.iter_mut().enumerate() {
                                ui.horizontal(|ui| {
                                    ui.checkbox(&mut layer.visible, t(self.lang, ""));
                                    let text = egui::RichText::new(format!(
                                        "#{i:02} {} {:.1}×{:.1}",
                                        kind_name(layer.kind),
                                        layer.width(),
                                        layer.height()
                                    ))
                                    .size(12.0);
                                    let label_response = ui.selectable_label(selected.contains(&i), text);
                                    if label_response.clicked() {
                                        clicked_layer = true;
                                        let add = ui.input(|inp| {
                                            inp.modifiers.ctrl || inp.modifiers.command
                                        });
                                        if add {
                                            if let Some(p) =
                                                selected.iter().position(|x| *x == i)
                                            {
                                                selected.remove(p);
                                            } else {
                                                selected.push(i);
                                                selected.sort_unstable();
                                            }
                                        } else {
                                            selected = vec![i];
                                        }
                                    }
                                    if label_response.secondary_clicked() {
                                        self.show_layer_context_menu = true;
                                        self.layer_context_idx = Some(i);
                                    }
                                    if ui
                                        .color_edit_button_srgba(&mut layer.color)
                                        .on_hover_text(t(self.lang, "cor da camada"))
                                        .changed()
                                    {
                                        clicked_layer = true;
                                    }
                                    if selected.contains(&i) {
                                        if ui
                                            .small_button("✎")
                                            .on_hover_text(t(self.lang, "editar nós desta camada"))
                                            .clicked()
                                        {
                                            edit_nodes = Some(i);
                                        }
                                        if ui
                                            .small_button("×")
                                            .on_hover_text(t(self.lang, "apagar traço"))
                                            .clicked()
                                        {
                                            delete = Some(i);
                                        }
                                        if ui
                                            .small_button("↺")
                                            .on_hover_text(t(self.lang, "repor original"))
                                            .clicked()
                                        {
                                            reset = Some(i);
                                        }
                                    }
                                });
                            }
                        });
                    }
                    if let Some(i) = art_select {
                        if self.selected_art_idx != i {
                            self.trace_preview = None;
                        }
                        self.selected_art_idx = i;
                        self.selected_art = true;
                        self.selected.clear();
                        if !matches!(self.tool, Tool::Image | Tool::Select) {
                            self.tool = Tool::Image;
                        }
                    }
                    if let Some((i, dir)) = art_move {
                        self.push_undo();
                        if let Some(doc) = &mut self.doc {
                            let j = i as isize + dir;
                            if j >= 0 && (j as usize) < doc.arts.len() {
                                let j = j as usize;
                                doc.arts.swap(i, j);
                                doc.art_offsets.swap(i, j);
                                for layer in doc.layers.iter_mut() {
                                    match layer.art {
                                        Some(a) if a == i => layer.art = Some(j),
                                        Some(a) if a == j => layer.art = Some(i),
                                        _ => {}
                                    }
                                }
                                if self.selected_art_idx == i {
                                    self.selected_art_idx = j;
                                }
                            }
                        }
                    }
                    if let Some(i) = art_delete {
                        self.push_undo();
                        if let Some(doc) = &mut self.doc {
                            if i < doc.arts.len() {
                                doc.arts.remove(i);
                                if i < doc.art_offsets.len() {
                                    doc.art_offsets.remove(i);
                                }
                                doc.layers.retain(|layer| layer.art != Some(i));
                                for layer in doc.layers.iter_mut() {
                                    if let Some(a) = layer.art.as_mut() {
                                        if *a > i {
                                            *a -= 1;
                                        }
                                    }
                                }
                                self.selected.clear();
                                if self.selected_art_idx >= doc.arts.len() {
                                    self.selected_art_idx = doc.arts.len().saturating_sub(1);
                                }
                            }
                        }
                        self.status = "imagem apagada (com os traços dela)".to_string();
                    }
                    if let Some(i) = edit_nodes {
                        self.selected_art = false;
                        self.selected = vec![i];
                        self.expand_selection();
                        self.tool = Tool::Nodes;
                        self.status = format!("a editar nós da camada #{i:02}");
                    }
                    if clicked_layer {
                        self.selected_art = false;
                        if !matches!(self.tool, Tool::Select | Tool::Nodes) {
                            self.tool = Tool::Select;
                        }
                    }
                    if let Some(i) = reset {
                        self.push_undo();
                        if let Some(doc) = &mut self.doc {
                            if let Some(layer) = doc.layers.get_mut(i) {
                                layer.points = layer.original.clone();
                            }
                        }
                    }
                    if let Some(i) = delete {
                        self.push_undo();
                        if let Some(doc) = &mut self.doc {
                            if i < doc.layers.len() {
                                doc.layers.remove(i);
                            }
                        }
                        selected.retain(|x| *x != i);
                    }
                    self.selected = selected;
                    self.expand_selection();
                    ui.add_space(4.0);
                    ui.small(format!("{} selecionado(s)", self.selected.len()));
                    ui.small(t(self.lang, "Arraste pontos/traços e pegas."));
                    ui.horizontal(|ui| {
                        let enabled = !self.selected.is_empty();
                        if ui
                            .add_enabled(enabled, egui::Button::new("Agrupar"))
                            .clicked()
                        {
                            self.group_selected();
                        }
                        if ui
                            .add_enabled(enabled, egui::Button::new("Desagrupar"))
                            .clicked()
                        {
                            self.ungroup_selected();
                        }
                    });
                    ui.horizontal(|ui| {
                        let enabled = !self.selected.is_empty();
                        if ui
                            .add_enabled(enabled, egui::Button::new("Inverter H"))
                            .clicked()
                        {
                            self.transform_selection(None, Some(true));
                        }
                        if ui
                            .add_enabled(enabled, egui::Button::new("Inverter V"))
                            .clicked()
                        {
                            self.transform_selection(None, Some(false));
                        }
                    });
                    ui.horizontal(|ui| {
                        let enabled = !self.selected.is_empty();
                        if ui
                            .add_enabled(enabled, egui::Button::new("Rodar 90°"))
                            .clicked()
                        {
                            self.transform_selection(
                                Some(std::f32::consts::FRAC_PI_2),
                                None,
                            );
                        }
                        if ui
                            .add_enabled(enabled, egui::Button::new("Repor original"))
                            .clicked()
                        {
                            self.reset_selected();
                        }
                    });
                    if ui
                        .add_enabled(
                            !self.selected.is_empty(),
                            egui::Button::new("Apagar seleção"),
                        )
                        .clicked()
                    {
                        self.delete_selected();
                    }
                });

            ui.separator();
            egui::CollapsingHeader::new(t(self.lang, "Design"))
                .default_open(false)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(t(self.lang, "X"));
                        ui.add(
                            egui::DragValue::new(&mut self.place_offset.x)
                                .speed(0.5)
                                .suffix(" mm"),
                        );
                        ui.label(t(self.lang, "Y"));
                        ui.add(
                            egui::DragValue::new(&mut self.place_offset.y)
                                .speed(0.5)
                                .suffix(" mm"),
                        );
                    });
                    ui.add(egui::Slider::new(&mut self.place_scale, 0.1..=3.0).text(t(self.lang, "Escala")));
                    if ui.button(t(self.lang, "Repor posição")).clicked() {
                        self.place_offset = egui::Vec2::ZERO;
                        self.place_scale = 1.0;
                    }
                });

            ui.separator();
            egui::CollapsingHeader::new(t(self.lang, "Cor"))
                .default_open(false)
                .show(ui, |ui| {
                    let art = self
                        .doc
                        .as_mut()
                        .and_then(|doc| doc.arts.get_mut(self.selected_art_idx));
                    let Some(art) = art else {
                        ui.small(t(self.lang, "selecione uma imagem (clique nela na folha)"));
                        return;
                    };
                    ui.small(format!("ajustes de «{}»", art.name));
                    let (mut hue, mut sat, mut vib) = (art.hue, art.saturation, art.vibrance);
                    let (mut contrast, mut brightness) = (art.contrast, art.brightness);
                    let mut changed = false;
                    changed |= ui
                        .add(
                            egui::Slider::new(&mut hue, -180.0..=180.0)
                                .text(t(self.lang, "Tonalidade"))
                                .suffix("°"),
                        )
                        .changed();
                    changed |= ui
                        .add(egui::Slider::new(&mut sat, 0.0..=2.0).text(t(self.lang, "Saturação")))
                        .changed();
                    changed |= ui
                        .add(egui::Slider::new(&mut vib, -1.0..=1.0).text(t(self.lang, "Vibração")))
                        .changed();
                    changed |= ui
                        .add(egui::Slider::new(&mut contrast, 0.0..=3.0).text(t(self.lang, "Contraste")))
                        .changed();
                    changed |= ui
                        .add(egui::Slider::new(&mut brightness, -1.0..=1.0).text(t(self.lang, "Brilho")))
                        .changed();
                    if ui.button(t(self.lang, "Repor cor")).clicked() {
                        hue = 0.0;
                        sat = 1.0;
                        vib = 0.0;
                        contrast = 1.0;
                        brightness = 0.0;
                        changed = true;
                    }
                    if changed {
                        if let Some(art) = self
                            .doc
                            .as_mut()
                            .and_then(|doc| doc.arts.get_mut(self.selected_art_idx))
                        {
                            art.hue = hue;
                            art.saturation = sat;
                            art.vibrance = vib;
                            art.contrast = contrast;
                            art.brightness = brightness;
                        }
                        self.update_art(ui.ctx());
                    }
                    ui.small(t(self.lang, "Efeitos só na imagem selecionada; a cor de cada camada define-se em «Camadas»."));
                });

            ui.separator();
            egui::CollapsingHeader::new(t(self.lang, "Encaixe"))
                .default_open(false)
                .show(ui, |ui| {
                    ui.checkbox(&mut self.snap_grid, t(self.lang, "Snap à grelha"));
                    ui.checkbox(&mut self.snap_guides, t(self.lang, "Guias inteligentes"));
                    ui.add(
                        egui::Slider::new(&mut self.grid_mm, 1.0..=20.0)
                            .text(t(self.lang, "Passo da grelha"))
                            .suffix(" mm"),
                    );
                    ui.small(t(self.lang, "Centro/pontas da página, centros e arestas dos objetos."));
                });

            ui.separator();
            egui::CollapsingHeader::new(t(self.lang, "Texto"))
                .default_open(false)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let current = self
                            .fonts
                            .get(self.font_index)
                            .map(|f| f.name.clone())
                            .unwrap_or_else(|| "—".to_string());
                        egui::ComboBox::from_label(t(self.lang, "Fonte"))
                            .selected_text(current)
                            .show_ui(ui, |ui| {
                                for (i, font) in self.fonts.iter().enumerate() {
                                    // Cada nome desenhado na própria fonte.
                                    let label = egui::RichText::new(&font.name).font(
                                        egui::FontId::new(
                                            15.0,
                                            egui::FontFamily::Name(format!("font-{i}").into()),
                                        ),
                                    );
                                    ui.selectable_value(&mut self.font_index, i, label);
                                }
                            });
                    });
                    ui.add(
                        egui::Slider::new(&mut self.text_size, 4.0..=120.0)
                            .text(t(self.lang, "Tamanho"))
                            .suffix(" mm"),
                    );
                    ui.add(egui::TextEdit::multiline(&mut self.text).desired_rows(2));
                    ui.horizontal(|ui| {
                        if ui.button(t(self.lang, "Adicionar texto")).clicked() {
                            self.add_text(ui.ctx());
                        }
                        if ui.button(t(self.lang, "Importar fonte")).clicked() {
                            self.import_font(ui.ctx());
                        }
                    });
                    ui.small(match self.text_origin {
                        Some(p) => format!("origem: {:.1}, {:.1} mm", p.x, p.y),
                        None => {
                            "clique no canvas (ferramenta Texto) para escolher a origem".to_string()
                        }
                    });
                    ui.small(format!("{} fontes instaladas", self.fonts.len()));

                    let texts: Vec<TextItem> = self
                        .doc
                        .as_ref()
                        .map(|d| d.texts.clone())
                        .unwrap_or_default();
                    if !texts.is_empty() {
                        ui.separator();
                        ui.label(egui::RichText::new("Textos editáveis").strong());
                    }
                    let mut actions = Vec::new();
                    for item in &texts {
                        let owners: Vec<usize> = self
                            .doc
                            .as_ref()
                            .map(|d| {
                                d.layers
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, l)| l.owner == Some(item.id))
                                    .map(|(i, _)| i)
                                    .collect()
                            })
                            .unwrap_or_default();
                        let selected = owners.iter().any(|i| self.selected.contains(i));
                        let preview: String = item.text.chars().take(18).collect();
                        if ui
                            .selectable_label(
                                selected,
                                format!("T{}: {}", item.id, preview.replace('\n', " ")),
                            )
                            .clicked()
                        {
                            self.select_indices(owners.clone(), false);
                        }
                        if selected {
                            let mut text = item.text.clone();
                            let mut font = item.font.clone();
                            let mut size = item.size;
                            let old_font = font.clone();
                            let mut color = owners
                                .first()
                                .and_then(|i| {
                                    self.doc
                                        .as_ref()
                                        .and_then(|d| d.layers.get(*i))
                                        .map(|l| l.color)
                                })
                                .unwrap_or(LAYER_COLOR);
                            let mut text_lost_focus = false;
                            let mut size_response: Option<egui::Response> = None;
                            ui.indent(("text", item.id), |ui| {
                                let tr = ui.add(
                                    egui::TextEdit::multiline(&mut text)
                                        .desired_rows(2)
                                        .desired_width(f32::INFINITY),
                                );
                                text_lost_focus = tr.lost_focus();
                                egui::ComboBox::from_label(t(self.lang, "Fonte"))
                                    .selected_text(font.clone())
                                    .show_ui(ui, |ui| {
                                        for (i, f) in self.fonts.iter().enumerate() {
                                            let label = egui::RichText::new(&f.name).font(
                                                egui::FontId::new(
                                                    15.0,
                                                    egui::FontFamily::Name(
                                                        format!("font-{i}").into(),
                                                    ),
                                                ),
                                            );
                                            ui.selectable_value(&mut font, f.name.clone(), label);
                                        }
                                    });
                                let sr = ui.add(
                                    egui::Slider::new(&mut size, 2.0..=200.0)
                                        .text(t(self.lang, "Tamanho"))
                                        .suffix(" mm"),
                                );
                                size_response = Some(sr);
                                ui.horizontal(|ui| {
                                    ui.color_edit_button_srgba(&mut color)
                                        .on_hover_text(t(self.lang, "cor do texto (preenchimento)"));
                                    if ui.button(t(self.lang, "Aplicar")).clicked() {
                                        actions.push(TextAction::Apply {
                                            id: item.id,
                                            text: text.clone(),
                                            font: font.clone(),
                                            size,
                                        });
                                    }
                                    if ui.button(t(self.lang, "Converter em caminho")).clicked() {
                                        actions.push(TextAction::Convert(item.id));
                                    }
                                    if ui.button(t(self.lang, "Apagar")).clicked() {
                                        actions.push(TextAction::Delete(item.id));
                                    }
                                });
                            });
                            // Edita ao vivo: aplicar sem pedir sempre «Aplicar».
                            if (text_lost_focus && text != item.text)
                                || font != old_font
                                || size_response
                                    .as_ref()
                                    .map(|sr| {
                                        (sr.changed() && !sr.dragged()) || sr.drag_stopped()
                                    })
                                    .unwrap_or(false)
                            {
                                actions.push(TextAction::Apply {
                                    id: item.id,
                                    text: text.clone(),
                                    font: font.clone(),
                                    size,
                                });
                            }
                            // Cor aplica-se de imediato às camadas do texto.
                            if let Some(doc) = &mut self.doc {
                                for i in &owners {
                                    if let Some(l) = doc.layers.get_mut(*i) {
                                        l.color = color;
                                    }
                                }
                            }
                        }
                    }
                    for action in actions {
                        match action {
                            TextAction::Apply {
                                id,
                                text,
                                font,
                                size,
                            } => self.apply_text(id, text, font, size),
                            TextAction::Convert(id) => self.convert_text(id),
                            TextAction::Delete(id) => self.delete_text(id),
                        }
                    }
                });

            ui.separator();
            let mut machine_action: Option<MachineAction> = None;
            egui::CollapsingHeader::new(t(self.lang, "Ligações"))
                .default_open(true)
                .show(ui, |ui| {
                    for slot in 0..2 {
                        let connected = self.machines[slot].connected();
                        let firmware = self.machines[slot].firmware.clone();
                        let state = self.machines[slot].status.clone();
                        let detail = self.machines[slot].detail.clone();
                        let mut link = self.machines[slot].link;
                        let dot = if connected {
                            egui::Color32::from_rgb(0, 168, 120)
                        } else {
                            egui::Color32::from_rgb(224, 74, 90)
                        };
                        egui::CollapsingHeader::new(format!("Silhouette {}", slot + 1))
                            .default_open(slot == 0)
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new("●").color(dot).size(15.0));
                                    ui.label(
                                        egui::RichText::new(if connected {
                                            "ligada"
                                        } else {
                                            "desligada"
                                        })
                                        .color(dot)
                                        .strong(),
                                    );
                                    ui.label(
                                        egui::RichText::new(format!("· {firmware}"))
                                            .size(11.0)
                                            .color(egui::Color32::from_gray(90)),
                                    );
                                });
                                ui.small(format!("{detail} · {state}"));
                                ui.horizontal(|ui| {
                                    if ui
                                        .selectable_label(link == Link::Usb, "USB")
                                        .clicked()
                                    {
                                        link = Link::Usb;
                                    }
                                    if ui
                                        .selectable_label(
                                            link == Link::Bluetooth,
                                            "Bluetooth",
                                        )
                                        .clicked()
                                    {
                                        link = Link::Bluetooth;
                                    }
                                });
                                self.machines[slot].link = link;
                                match link {
                                    Link::Usb => {
                                        if ui.button(t(self.lang, "Ligar USB")).clicked() {
                                            machine_action = Some(MachineAction::Usb(slot));
                                        }
                                    }
                                    Link::Bluetooth => {
                                        let current = self
                                            .bt_devices
                                            .get(self.machine_bt_choice[slot])
                                            .map(|d| {
                                                if d.name.is_empty() {
                                                    d.mac.clone()
                                                } else {
                                                    format!("{} · {}", d.name, d.mac)
                                                }
                                            })
                                            .unwrap_or_else(|| {
                                                "procure dispositivos".to_string()
                                            });
                                        egui::ComboBox::from_label(t(self.lang, "Dispositivo"))
                                            .selected_text(current)
                                            .show_ui(ui, |ui| {
                                                for (i, device) in
                                                    self.bt_devices.iter().enumerate()
                                                {
                                                    let text = if device.name.is_empty() {
                                                        device.mac.clone()
                                                    } else {
                                                        format!(
                                                            "{} · {}",
                                                            device.name, device.mac
                                                        )
                                                    };
                                                    ui.selectable_value(
                                                        &mut self.machine_bt_choice[slot],
                                                        i,
                                                        text,
                                                    );
                                                }
                                            });
                                        if ui.button(t(self.lang, "Ligar Bluetooth")).clicked() {
                                            if let Some(device) = self
                                                .bt_devices
                                                .get(self.machine_bt_choice[slot])
                                            {
                                                machine_action =
                                                    Some(MachineAction::Bluetooth(
                                                        slot,
                                                        device.mac.clone(),
                                                        device.name.clone(),
                                                    ));
                                            }
                                        }
                                    }
                                }
                                ui.horizontal(|ui| {
                                    if connected && ui.button(t(self.lang, "Desligar")).clicked() {
                                        machine_action =
                                            Some(MachineAction::Disconnect(slot));
                                    }
                                    if connected && ui.button(t(self.lang, "Atualizar estado")).clicked() {
                                        machine_action = Some(MachineAction::Refresh);
                                    }
                                });
                            });
                    }

                    ui.separator();
                    // Permissão USB: pista + solução com um clique (polkit).
                    if self.udev_hint {
                        ui.colored_label(
                            egui::Color32::from_rgb(255, 120, 100),
                            "USB bloqueado: falta a regra udev (permissão de dispositivo).",
                        );
                    }
                    if ui
                        .button("Instalar regra udev (USB, só uma vez)")
                        .on_hover_text(t(self.lang, "Escreve a regra em /etc/udev/rules.d e recarrega o udev; pede a senha uma vez"))
                        .clicked()
                    {
                        self.install_udev_rule();
                    }
                    ui.small(t(self.lang, "Permite ao utilizador abrir a máquina sem sudo (emparelhamento Bluetooth não precisa disto)."));
                    ui.separator();
                    let printer_dot = if self.printer_status.contains("pronta")
                        || self.printer_status.contains("a imprimir")
                    {
                        egui::Color32::from_rgb(0, 168, 120)
                    } else {
                        egui::Color32::from_rgb(255, 154, 74)
                    };
                    egui::CollapsingHeader::new(t(self.lang, "Impressora"))
                        .default_open(true)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("●").color(printer_dot).size(15.0));
                                ui.label(
                                    egui::RichText::new(&self.printer_status)
                                        .color(printer_dot)
                                        .strong(),
                                );
                            });
                            ui.horizontal(|ui| {
                                let current = self
                                    .printers
                                    .get(self.printer_index)
                                    .cloned()
                                    .unwrap_or_else(|| "sem filas CUPS".to_string());
                                let combo = egui::ComboBox::from_label(t(self.lang, "Fila"))
                                    .selected_text(current)
                                    .show_ui(ui, |ui| {
                                        for (i, name) in self.printers.iter().enumerate() {
                                            ui.selectable_value(
                                                &mut self.printer_index,
                                                i,
                                                name,
                                            );
                                        }
                                    });
                                if combo.response.changed() {
                                    self.printer_options = self
                                        .printers
                                        .get(self.printer_index)
                                        .is_some_and(|n| queue_has_epson_options(n));
                                    self.printer_status = self
                                        .printers
                                        .get(self.printer_index)
                                        .map(|n| printer_state(n))
                                        .unwrap_or_else(|| {
                                            "sem impressora CUPS".to_string()
                                        });
                                }
                                if ui.button(t(self.lang, "↻")).on_hover_text(t(self.lang, "atualizar")).clicked() {
                                    self.refresh_printers();
                                }
                            });
                            ui.horizontal(|ui| {
                                if ui
                                    .add_enabled(
                                        self.doc.is_some() && !self.printers.is_empty(),
                                        primary_button(egui::RichText::new("▤ Imprimir")),
                                    )
                                    .clicked()
                                {
                                    self.print_direct();
                                }
                                if ui
                                    .button("Testar")
                                    .on_hover_text(t(self.lang, "imprime uma página A4 de teste"))
                                    .clicked()
                                {
                                    self.test_print();
                                }
                                if ui
                                    .button("Testar escala")
                                    .on_hover_text(
                                        "imprime réguas em mm + marcas + quadrados conhecidos para medir com régua física",
                                    )
                                    .clicked()
                                {
                                    self.print_calibration();
                                }
                                if ui.button(t(self.lang, "Guardar PDF...")).clicked() {
                                    self.print_sheet();
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut self.print_art, t(self.lang, "Arte"));
                                ui.checkbox(&mut self.print_marks, t(self.lang, "Marcas"));
                                ui.checkbox(&mut self.print_paths, t(self.lang, "Linhas de corte"));
                            });
                            ui.horizontal_wrapped(|ui| {
                                ui.label(t(self.lang, "Qualidade"));
                                egui::ComboBox::from_id_salt("print_quality")
                                    .width(110.0)
                                    .selected_text(self.print_quality.label())
                                    .show_ui(ui, |ui| {
                                        for quality in PrintQuality::ALL {
                                            ui.selectable_value(
                                                &mut self.print_quality,
                                                quality,
                                                quality.label(),
                                            );
                                        }
                                    });
                                ui.label(t(self.lang, "Cor"));
                                egui::ComboBox::from_id_salt("print_color")
                                    .width(125.0)
                                    .selected_text(self.print_color.label())
                                    .show_ui(ui, |ui| {
                                        for color in PrintColor::ALL {
                                            ui.selectable_value(
                                                &mut self.print_color,
                                                color,
                                                color.label(),
                                            );
                                        }
                                    });
                            });
                            ui.horizontal(|ui| {
                                ui.label(t(self.lang, "Papel"));
                                egui::ComboBox::from_id_salt("print_media")
                                    .width(190.0)
                                    .selected_text(self.print_media.label())
                                    .show_ui(ui, |ui| {
                                        for media in PrintMedia::ALL {
                                            ui.selectable_value(
                                                &mut self.print_media,
                                                media,
                                                media.label(),
                                            );
                                        }
                                    });
                            });
                            ui.small(format!(
                                "Epson: cupsPrintQuality={} · ColorModel={} · print-color-mode={} · MediaType={}",
                                self.print_quality.cups(),
                                self.print_color.cups(),
                                self.print_color.ipp(),
                                self.print_media.cups()
                            ));
                            ui.small(&self.print_status);
                            if let Some(job) = self.print_job.clone() {
                                if ui.small_button(t(self.lang, "Ver estado do trabalho")).clicked() {
                                    self.print_status = job_state(&job);
                                }
                            }
                            ui.small(t(self.lang, "A4 a 100 %, sem ajustar à página."));
                            let selected = self
                                .printers
                                .get(self.printer_index)
                                .cloned()
                                .unwrap_or_default();
                            if !self.printer_options && !is_cutter_queue(&selected) {
                                ui.small(
                                    egui::RichText::new(format!(
                                        "A fila {selected} não expõe as opções do driver Epson; a impressão pode sair a preto ou não sair."
                                    ))
                                    .color(egui::Color32::from_rgb(255, 154, 74))
                                    .size(10.0),
                                );
                                if let Some(i) = self
                                    .printers
                                    .iter()
                                    .position(|p| !is_cutter_queue(p) && queue_has_epson_options(p))
                                {
                                    let name = self.printers[i].clone();
                                    if ui.small_button(format!("Usar {name}")).clicked() {
                                        self.printer_index = i;
                                        self.printer_options = true;
                                        self.printer_status = printer_state(&name);
                                        self.print_status =
                                            "fila Epson selecionada".to_string();
                                    }
                                }
                            }
                            ui.separator();
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("Bluetooth").strong());
                                if ui
                                    .add_enabled(
                                        self.bt_rx.is_none(),
                                        egui::Button::new("Procurar"),
                                    )
                                    .clicked()
                                {
                                    self.scan_bluetooth();
                                }
                            });
                            for device in self.bt_devices.clone() {
                                ui.horizontal(|ui| {
                                    let label = if device.name.is_empty() {
                                        device.mac.clone()
                                    } else {
                                        format!("{} · {}", device.name, device.mac)
                                    };
                                    ui.label(egui::RichText::new(label).size(12.0));
                                    if ui.small_button(t(self.lang, "Ligar impressora")).clicked() {
                                        self.pair_bluetooth(
                                            device.mac.clone(),
                                            device.name.clone(),
                                        );
                                    }
                                });
                            }
                            ui.small(&self.bt_status);
                        });
                });
            match machine_action {
                Some(MachineAction::Usb(slot)) => self.connect_usb(slot),
                Some(MachineAction::Bluetooth(slot, mac, name)) => {
                    self.connect_bt(slot, mac, name)
                }
                Some(MachineAction::Disconnect(slot)) => self.disconnect_machine(slot),
                Some(MachineAction::Refresh) => self.refresh_machines(),
                None => {}
            }

            ui.separator();
            egui::CollapsingHeader::new(t(self.lang, "Marcas de registo"))
                .default_open(false)
                .show(ui, |ui| {
                    egui::ComboBox::from_label(t(self.lang, "Estilo"))
                        .selected_text(match self.mark_style {
                            RegMarkStyle::FourCorner => "3 L (4 cantos)",
                            RegMarkStyle::Standard => "2 L (3 pontos)",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.mark_style,
                                RegMarkStyle::FourCorner,
                                "3 L (4 cantos)",
                            );
                            ui.selectable_value(
                                &mut self.mark_style,
                                RegMarkStyle::Standard,
                                "2 L (3 pontos)",
                            );
                        });
                    ui.add(
                        egui::Slider::new(&mut self.mark_size, 6.0..=30.0)
                            .text(t(self.lang, "Braço dos L"))
                            .suffix(" mm"),
                    );
                    ui.add(
                        egui::Slider::new(&mut self.mark_square, 5.0..=20.0)
                            .text(t(self.lang, "Quadrado"))
                            .suffix(" mm"),
                    );
                    ui.add(
                        egui::Slider::new(&mut self.mark_stroke, 0.3..=1.0)
                            .text(t(self.lang, "Espessura"))
                            .suffix(" mm"),
                    );
                    ui.add(
                        egui::Slider::new(&mut self.mark_inset, 10.0..=50.0)
                            .text(t(self.lang, "Margem (inset)"))
                            .suffix(" mm"),
                    );
                    ui.small(t(self.lang, "Limites do Studio: braço 6–30 mm, espessura 0,3–1,0 mm, inset ≥ 10 mm."));
                    ui.separator();
                    ui.heading(t(self.lang, "Busca das marcas (TB123)"));
                    ui.add(
                        egui::Slider::new(&mut self.mark_search_offset_top, 0.0..=20.0)
                            .text(t(self.lang, "Offset topo (mm)"))
                            .suffix(" mm"),
                    );
                    ui.add(
                        egui::Slider::new(&mut self.mark_search_offset_left, 0.0..=20.0)
                            .text(t(self.lang, "Offset esquerda (mm)"))
                            .suffix(" mm"),
                    );
                    ui.small(t(self.lang, "Busca começa N mm antes das marcas; padrão 10 mm."));
                    ui.separator();
                    ui.heading(t(self.lang, "Segmentos (mídia longa)"));
                    ui.add(
                        egui::Slider::new(&mut self.mark_step_size, 0.0..=500.0)
                            .text(t(self.lang, "Passo (mm)"))
                            .suffix(" mm"),
                    );
                    ui.add(
                        egui::Slider::new(&mut self.mark_step_direction, 0..=1)
                            .text(t(self.lang, "Direção: 0=vertical, 1=horizontal")),
                    );
                    ui.small(t(self.lang, "Passo > 0 habilita marcas segmentadas (TB23 repetido)."));
                    if ui
                        .button("Studio: quadrado 10 mm, braço 20 mm, 0,5 mm")
                        .on_hover_text(
                            "Geometria do Silhouette Studio (TB51,400 TB53,10); reimprima as marcas",
                        )
                        .clicked()
                    {
                        self.mark_size = 20.0;
                        self.mark_square = 10.0;
                        self.mark_stroke = 0.5;
                        self.mark_inset = 15.875;
                        self.mark_search_offset_top = 10.0;
                        self.mark_search_offset_left = 10.0;
                        self.mark_step_size = 0.0;
                        self.mark_step_direction = 0;
                    }
                    if let Some(doc) = &self.doc {
                        let spec = self.mark_spec(doc.page);
                        let arm_su = (spec.arm_mm * 20.0).round() as i32;
                        let stroke_su = (spec.stroke_mm * 20.0).round() as i32;
                        let h_su = (spec.mark_height_mm() * 20.0).round() as i32;
                        let w_su = (spec.mark_width_mm() * 20.0).round() as i32;
                        let top_su = (spec.scan_margin_top_mm() * 20.0).round() as i32;
                        let left_su = (spec.scan_margin_left_mm() * 20.0).round() as i32;
                        ui.small(format!(
                            "TB51,{arm_su}  TB53,{stroke_su}  TB123,{h_su},{w_su},{top_su},{left_su}"
                        ));
                        ui.small(t(self.lang, "Imprima a 100 % e não altere a folha depois."));
                    }
                });

            ui.separator();
            egui::CollapsingHeader::new(t(self.lang, "Imagem"))
                .default_open(true)
                .show(ui, |ui| {
                    let has_art = self.doc.as_ref().is_some_and(|d| !d.arts.is_empty());
                    if !has_art {
                        ui.small(t(self.lang, "Abra uma imagem (.png, .jpg, .webp, .tiff, .psd)."));
                        return;
                    }
                    if let Some(rect) = self.art_bbox() {
                        ui.small(format!(
                            "{:.1} × {:.1} mm · {:.0} % do tamanho natural",
                            rect.width(),
                            rect.height(),
                            self.art_size_percent().unwrap_or(100.0)
                        ));
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .add(primary_button(egui::RichText::new("🖼 Selecionar")))
                            .clicked()
                        {
                            self.selected_art = true;
                            self.selected.clear();
                            self.tool = Tool::Image;
                            self.status =
                                "imagem selecionada — arraste os cantos para escalar".to_string();
                        }
                        if ui.button(t(self.lang, "Ajustar à página")).clicked() {
                            self.fit_art_to_page();
                        }
                    });
                    ui.horizontal(|ui| {
                        if ui.button(t(self.lang, "Centrar no canvas")).clicked() {
                            self.selected_art = true;
                            self.selected.clear();
                            self.center_on_canvas();
                        }
                        if ui.button(t(self.lang, "Tamanho 100 %")).clicked() {
                            self.reset_art_size();
                        }
                    });
                    ui.small(t(self.lang, "Cantos escalam proporcionalmente; Shift liberta o aspeto; Alt/Ctrl escala pelo centro."));
                });

            ui.separator();
            egui::CollapsingHeader::new(t(self.lang, "Vetorizar imagem"))
                .default_open(false)
                .show(ui, |ui| {
                    let has_art = self.doc.as_ref().is_some_and(|d| !d.arts.is_empty());
                    if !has_art {
                        ui.small(
                            "Abra uma imagem (.png, .jpg, .webp, .tiff, .psd) para vetorizar.",
                        );
                        return;
                    }
                    let mut preview_changed = false;
                    egui::ComboBox::from_label(t(self.lang, "Método"))
                        .selected_text(match self.trace_method {
                            TraceMethod::Auto => t(self.lang, "Auto (recomendado)"),
                            TraceMethod::Outline => t(self.lang, "Apenas contorno"),
                            TraceMethod::Edges => t(self.lang, "Todas as linhas (bordas)"),
                            TraceMethod::Studio => t(self.lang, "Qualidade Studio (curvas)"),
                        })
                        .show_ui(ui, |ui| {
                            for (method, label) in [
                                (TraceMethod::Auto, t(self.lang, "Auto (recomendado)")),
                                (TraceMethod::Outline, t(self.lang, "Apenas contorno")),
                                (TraceMethod::Edges, t(self.lang, "Todas as linhas (bordas)")),
                                (
                                    TraceMethod::Studio,
                                    t(self.lang, "Qualidade Studio (curvas)"),
                                ),
                            ] {
                                if ui
                                    .selectable_value(&mut self.trace_method, method, label)
                                    .clicked()
                                {
                                    preview_changed = true;
                                }
                            }
                        });
                    match self.trace_method {
                        TraceMethod::Outline => {
                            if ui
                                .add(
                                    egui::Slider::new(&mut self.trace_threshold, 0.0..=255.0)
                                        .text(t(self.lang, "Limiar de tinta")),
                                )
                                .changed()
                            {
                                preview_changed = true;
                            }
                        }
                        TraceMethod::Edges => {
                            if ui
                                .add(
                                    egui::Slider::new(&mut self.trace_edge, 5.0..=255.0)
                                        .text(t(self.lang, "Sensibilidade")),
                                )
                                .changed()
                            {
                                preview_changed = true;
                            }
                        }
                        // The Studio engine thresholds internally; only noise
                        // and the contour controls below apply to it.
                        TraceMethod::Studio | TraceMethod::Auto => {
                            // In Auto this only feeds the fallback; the value is
                            // disabled so it cannot suggest false control.
                            ui.add_enabled(
                                self.trace_method == TraceMethod::Studio,
                                egui::Slider::new(
                                    &mut self.trace_studio_noise,
                                    0.0..=10.0,
                                )
                                .text(t(self.lang, "Manchas (%)"))
                                .suffix(" %"),
                            );
                            if self.trace_method == TraceMethod::Studio
                                && self.trace_studio_noise > 3.0
                            {
                                ui.colored_label(
                                    egui::Color32::from_rgb(230, 130, 30),
                                    t(
                                        self.lang,
                                        "Manchas altas removem peças pequenas soltas",
                                    ),
                                );
                            }
                            if ui
                                .checkbox(
                                    &mut self.trace_outer_only,
                                    t(self.lang, "Apenas contorno externo"),
                                )
                                .changed()
                            {
                                preview_changed = true;
                            }
                            if ui
                                .checkbox(
                                    &mut self.trace_apply_offset,
                                    t(self.lang, "Aplicar offset externo"),
                                )
                                .changed()
                            {
                                preview_changed = true;
                            }
                            if self.trace_apply_offset {
                                ui.colored_label(
                                    egui::Color32::from_rgb(230, 130, 30),
                                    t(
                                        self.lang,
                                        "Offset externo ATIVO: os contornos nascem deslocados para fora",
                                    ),
                                );
                                if ui
                                    .add(
                                        egui::Slider::new(&mut self.offset_mm, 0.1..=10.0)
                                            .text(t(self.lang, "Distância"))
                                            .suffix(" mm"),
                                    )
                                    .changed()
                                {
                                    preview_changed = true;
                                }
                            }
                        }
                    }
                    // Refinement sliders work in every engine: in the vtracer
                    // ones they map to segment length, corner smoothing and a
                    // minimum path length; in Auto they stay disabled.
                    let studio_family = self.trace_method.is_studio_family();
                    let auto = self.trace_method == TraceMethod::Auto;
                    if ui
                        .add_enabled(
                            !auto,
                            egui::Slider::new(&mut self.trace_simplify, 0.0..=2.0)
                                .text(t(self.lang, "Simplif."))
                                .suffix(" mm"),
                        )
                        .changed()
                    {
                        preview_changed = true;
                    }
                    if ui
                        .add_enabled(
                            !auto,
                            egui::Slider::new(&mut self.trace_min_length, 0.0..=20.0)
                                .text(t(self.lang, "Mín."))
                                .suffix(" mm"),
                        )
                        .changed()
                    {
                        preview_changed = true;
                    }
                    if self.trace_method == TraceMethod::Outline {
                        if ui
                            .add(
                                egui::Slider::new(&mut self.trace_min_area, 0.0..=50.0)
                                    .text(t(self.lang, "Ruído (mm²)"))
                                    .suffix(" mm²"),
                            )
                            .changed()
                        {
                            preview_changed = true;
                        }
                    }
                    if ui
                        .add_enabled(
                            !auto,
                            egui::Slider::new(&mut self.trace_smooth, 0..=4)
                                .text(t(self.lang, "Suavizar"))
                                .integer(),
                        )
                        .changed()
                    {
                        preview_changed = true;
                    }
                    let _ = studio_family;
                    ui.checkbox(&mut self.trace_separate, t(self.lang, "Separar linhas"));
                    if preview_changed {
                        self.request_trace_preview();
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .add(primary_button(egui::RichText::new("✎ Vetorizar")))
                            .clicked()
                        {
                            self.vectorize();
                        }
                        if ui
                            .button(t(self.lang, "⬚ Vetorizar área"))
                            .on_hover_text(t(self.lang,
                                "Arraste uma área no canvas e vetorize só ela, com o dobro da resolução — bom para detalhes pequenos"))
                            .clicked()
                        {
                            self.trace_area();
                        }
                        if ui.button(t(self.lang, "🗑 Apagar vetorização")).clicked() {
                            self.delete_traced();
                        }
                    });
                    if let Some(preview) = &self.trace_preview {
                        ui.colored_label(
                            egui::Color32::from_rgb(0, 140, 230),
                            format!(
                                "Pré-visualização ao vivo: {} caminho(s) — linhas azuis no canvas",
                                preview.len()
                            ),
                        );
                        if ui.small_button(t(self.lang, "✕ Limpar pré-visualização")).clicked() {
                            self.trace_preview = None;
                        }
                    } else {
                        ui.small(t(self.lang, "Mova os controlos para ver a vetorização antes de aplicar."));
                    }
                    let traced_count = self
                        .doc
                        .as_ref()
                        .map(|d| d.layers.iter().filter(|l| l.traced).count())
                        .unwrap_or(0);
                    ui.small(if traced_count > 0 {
                        format!("{traced_count} linha(s) vetorizada(s); vetorizar substitui-as.")
                    } else {
                        "Vetorizar substitui sempre a vetorização anterior.".to_string()
                    });
                    ui.separator();
                    ui.label(egui::RichText::new("Offset das linhas").strong());
                    ui.add(
                        egui::Slider::new(&mut self.offset_mm, 0.5..=10.0)
                            .text(t(self.lang, "Distância"))
                            .suffix(" mm"),
                    );
                    ui.horizontal(|ui| {
                        if ui.button(t(self.lang, "Externo +")).clicked() {
                            self.preview_offset(true);
                        }
                        if ui.button(t(self.lang, "Interno −")).clicked() {
                            self.preview_offset(false);
                        }
                    });
                    if let Some(preview) = &self.offset_preview {
                        let color = if preview.external {
                            egui::Color32::from_rgb(230, 140, 20)
                        } else {
                            egui::Color32::from_rgb(150, 80, 210)
                        };
                        ui.colored_label(
                            color,
                            format!(
                                "Pré-visualização {} ({:.1} mm, {} contorno(s))",
                                if preview.external { "externa" } else { "interna" },
                                preview.distance.abs(),
                                preview.contours.len()
                            ),
                        );
                        ui.horizontal(|ui| {
                            if ui
                                .add(primary_button(egui::RichText::new("✓ Aplicar offset")))
                                .clicked()
                            {
                                self.apply_offset_preview();
                            }
                            if ui.button(t(self.lang, "✕ Cancelar")).clicked() {
                                self.cancel_offset();
                            }
                        });
                    }
                    let selected = self.selected.len();
                    let hint = if selected > 0 {
                        format!("Aplica-se a {selected} elemento(s) selecionado(s).")
                    } else {
                        "Sem seleção: aplica-se às linhas vetorizadas.".to_string()
                    };
                    ui.add(egui::Label::new(egui::RichText::new(hint).size(10.0)).wrap());
                    ui.separator();
                    if ui.button(t(self.lang, "🗑 Apagar seleção")).clicked() {
                        self.delete_selected();
                    }
                });

            ui.separator();
            egui::CollapsingHeader::new(t(self.lang, "Corte"))
                .default_open(true)
                .show(ui, |ui| {
                    let mut preset = 0usize;
                    egui::ComboBox::from_label(t(self.lang, "Material"))
                        .selected_text(["Papel", "Vinil", "Cartão", "Personalizado"][preset.min(3)])
                        .show_ui(ui, |ui| {
                            for (i, name) in
                                ["Papel", "Vinil", "Cartão", "Personalizado"].iter().enumerate()
                            {
                                if ui.selectable_value(&mut preset, i, *name).clicked() {
                                    match i {
                                        0 => {
                                            self.settings.force = 4;
                                            self.settings.speed = 3;
                                            self.settings.depth = 1;
                                        }
                                        1 => {
                                            self.settings.force = 8;
                                            self.settings.speed = 5;
                                            self.settings.depth = 2;
                                        }
                                        2 => {
                                            self.settings.force = 14;
                                            self.settings.speed = 2;
                                            self.settings.depth = 4;
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        });
                    let mut tool = if self.settings.tool == 2 { 1 } else { 0 };
                    egui::ComboBox::from_label(t(self.lang, "Ferramenta"))
                        .selected_text(if tool == 0 { "Lâmina" } else { "Caneta" })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut tool, 0, t(self.lang, "Lâmina"));
                            ui.selectable_value(&mut tool, 1, t(self.lang, "Caneta"));
                        });
                    self.settings.tool = if tool == 0 { 1 } else { 2 };
                    ui.add(egui::Slider::new(&mut self.settings.speed, 1..=10).text(t(self.lang, "Velocidade")));
                    ui.add(egui::Slider::new(&mut self.settings.force, 1..=33).text(t(self.lang, "Força")));
                    ui.add(
                        egui::Slider::new(&mut self.settings.depth, 1..=10).text(t(self.lang, "Profundidade")),
                    );
                    ui.horizontal(|ui| {
                        ui.label(t(self.lang, "Cortar em"));
                        ui.checkbox(&mut self.machine_cut[0], t(self.lang, "M1"));
                        ui.checkbox(&mut self.machine_cut[1], t(self.lang, "M2"));
                    });
                    ui.checkbox(
                        &mut self.scan_marks,
                        "Ler marcas de registo antes de cortar",
                    )
                    .on_hover_text(
                        "O sensor procura o quadrado e os L; se não encontrar, não corta",
                    );
                    if self.scan_marks {
                        ui.small(t(self.lang, "As marcas impressas têm de corresponder ao painel Marcas de registo."));
                    } else {
                        ui.small(t(self.lang, "Sem leitura de marcas: o corte usa a origem da página."));
                    }
                    ui.small(t(self.lang, "M2 desmarcada não recebe o trabalho."));
                    ui.horizontal(|ui| {
                        for (slot, machine) in self.machines.iter().enumerate() {
                            let connected = machine.connected();
                            let selected = self.machine_cut[slot];
                            let dot = if connected && selected {
                                egui::Color32::from_rgb(0, 168, 120)
                            } else if selected {
                                egui::Color32::from_rgb(255, 154, 74)
                            } else {
                                egui::Color32::from_rgb(180, 180, 180)
                            };
                            ui.label(
                                egui::RichText::new(format!(
                                    "● M{} {} · {}",
                                    slot + 1,
                                    if connected { "ligada" } else { "off" },
                                    if selected { "corta" } else { "não corta" }
                                ))
                                .color(dot)
                                .size(11.0),
                            );
                        }
                    });
                    ui.checkbox(&mut self.dry_run, t(self.lang, "Dry-run (não envia)"));
                    if ui
                        .checkbox(&mut self.preview, "Pré-visualizar ordem")
                        .changed()
                    {
                        self.preview_progress = 0.0;
                    }
                    if self.preview {
                        let total: f32 = self
                            .doc
                            .as_ref()
                            .map(|d| {
                                d.layers
                                    .iter()
                                    .filter(|l| l.visible)
                                    .map(|l| l.points.len())
                                    .sum()
                            })
                            .unwrap_or(0) as f32;
                        if self.preview_progress < total {
                            self.preview_progress = (self.preview_progress + 30.0).min(total);
                            ui.ctx().request_repaint();
                        }
                        ui.add(
                            egui::Slider::new(&mut self.preview_progress, 0.0..=total)
                                .text(t(self.lang, "Progresso")),
                        );
                    }
                    let can_cut = self.doc.as_ref().is_some_and(|d| !d.layers.is_empty());
                    let any_machine = self.machines.iter().any(|m| m.connected());
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                can_cut,
                                primary_button(egui::RichText::new("✂ Cortar...")),
                            )
                            .clicked()
                        {
                            self.confirm = true;
                        }
                        if ui
                            .add_enabled(any_machine, egui::Button::new("⌖ Testar marcas"))
                            .on_hover_text(
                                "Lê as marcas sem cortar; a resposta do sensor fica no registo",
                            )
                            .clicked()
                        {
                            self.test_marks();
                        }
                    });
                    if self.last_scan != "—" {
                        ui.small(format!("Última leitura: {}", self.last_scan));
                    }
                    ui.small(t(self.lang, "Registo: ~/.cache/sillinux/cut.log"));
                });
        });
    }

    fn help_window(&mut self, ctx: &egui::Context) {
        if !self.help_open {
            return;
        }
        let mut open = self.help_open;
        egui::Window::new(t(self.lang, "❓ Ajuda e comandos"))
            .open(&mut open)
            .default_width(580.0)
            .default_pos(egui::pos2(320.0, 110.0))
            .resizable(true)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(560.0)
                    .show(ui, |ui| {
                        ui.heading(t(self.lang, "Atalhos"));
                        egui::Grid::new("help_shortcuts")
                            .num_columns(2)
                            .spacing([18.0, 4.0])
                            .striped(true)
                            .show(ui, |ui| {
                                for (key, action) in [
                                    ("Ctrl+O", "Abrir .studio3, .svg, .pdf, .psd, .tiff, .png, .jpg, .webp"),
                                    ("Ctrl+S", "Guardar o projeto em SVG (arte + linhas de corte)"),
                                    ("Ctrl+Z", "Desfazer"),
                                    ("Ctrl+Y ou Ctrl+Shift+Z", "Refazer"),
                                    ("Ctrl+D", "Duplicar seleção (ou imagem) 5 mm ao lado"),
                                    ("Ctrl+0", "Repor zoom e vista"),
                                    ("Ctrl+A", "Selecionar todas as camadas"),
                                    ("Ctrl+G", "Agrupar seleção"),
                                    ("Ctrl+Shift+G", "Desagrupar seleção"),
                                    ("V / N / T / I / H", "Ferramentas: selecionar, nós, texto, imagem, vista"),
                                    ("Esc", "Cancelar arrasto e limpar seleção"),
                                    ("Setas", "Mover seleção 1 mm (Shift: 10 mm)"),
                                    ("Delete / Backspace", "Apagar seleção ou vetorização"),
                                    ("F1", "Abrir esta ajuda"),
                                    ("Roda", "Zoom no cursor"),
                                    ("Ctrl + roda", "Zoom fino"),
                                    ("Espaço + arrastar", "Mover a vista"),
                                    ("Arrastar no vazio", "Seleção por borracha"),
                                    ("Ctrl+clique", "Juntar/retirar da seleção"),
                                    ("Shift", "Escala uniforme / rotação de 15°"),
                                    ("Alt ou Ctrl", "Escala a partir do centro"),
                                    ("Duplo clique (Nós)", "Adicionar ponto"),
                                    ("Clique direito (Nós)", "Remover ponto"),
                                ] {
                                    ui.label(egui::RichText::new(key).monospace().strong());
                                    ui.label(action);
                                    ui.end_row();
                                }
                            });

                        ui.add_space(8.0);
                        ui.separator();
                        ui.heading(t(self.lang, "Três identificadores"));
                        ui.label(t(self.lang, "Ligações mostra sempre: Silhouette 1, Silhouette 2 e Impressora."));
                        ui.label(t(self.lang, "• Cada Silhouette liga por USB ou Bluetooth e mostra firmware e estado."));
                        ui.label(t(self.lang, "• Atualizar estado relê firmware/estado; Desligar liberta a porta."));
                        ui.label(t(self.lang, "• Em Corte marque M1 e/ou M2; M2 desmarcada não recebe o trabalho."));
                        ui.label(t(self.lang, "• «Ler marcas» procura o quadrado e os L antes de cortar; se falhar, não corta."));
                        ui.label(t(self.lang, "• A impressora mostra a fila CUPS e o estado (pronta / a imprimir)."));

                        ui.add_space(8.0);
                        ui.separator();
                        ui.heading(t(self.lang, "Impressão"));
                        ui.label(t(self.lang, "Use A4, 100 %, sem ajustar à página. A app chama:"));
                        ui.label(
                            egui::RichText::new(
                                "lp -d <fila> -o print-scaling=none -o fit-to-page=false \\\n  -o cupsPrintQuality=High -o ColorModel=RGB -o print-color-mode=color \\\n  -o MediaType=Stationery",
                            )
                            .monospace()
                            .size(11.0),
                        );
                        ui.label(t(self.lang, "Qualidade, cor e papel usam os valores do driver Epson (lpoptions -l)."));
                        ui.label(t(self.lang, "Nunca imprimir para a fila CAMEO-5: é a cortadora."));

                        ui.add_space(8.0);
                        ui.separator();
                        ui.heading(t(self.lang, "Vetorização e offset"));
                        ui.label(t(self.lang, "Abra uma imagem e use «Vetorizar imagem»."));
                        ui.label(t(self.lang, "• Apenas contorno: segue a silhueta do conteúdo."));
                        ui.label(t(self.lang, "• Todas as linhas: deteta bordas e reduz a linhas de 1 px."));
                        ui.label(t(self.lang, "• Separar linhas: cada contorno fica editável; desmarcado, ficam agrupados."));
                        ui.label(t(self.lang, "• Simplificar e comprimento mínimo refinam; Vetorizar substitui o anterior."));
                        ui.label(t(self.lang, "• Ferramenta Imagem: arraste a imagem; a vetorização nasce separada."));
                        ui.label(t(self.lang, "• Offset externo/interno mostra uma pré-visualização; Aplicar confirma, Cancelar descarta."));
                        ui.label(t(self.lang, "• Apagar vetorização remove todas as linhas geradas; Delete apaga a seleção."));

                        ui.add_space(8.0);
                        ui.separator();
                        ui.heading(t(self.lang, "Marcas de registo"));
                        ui.label(t(self.lang, "Quadrado de 5 mm no canto superior esquerdo e três L com braço de 20 mm."));
                        ui.label(t(self.lang, "Espessura 0,3 mm; imprimir a 100 % e usar a mesma geometria na leitura (TB123)."));
                        ui.label(t(self.lang, "Se a leitura falhar, confirme a escala de impressão e reimprima as marcas."));
                        ui.label(t(self.lang, "«Testar marcas» lê as marcas sem cortar e mostra a resposta do sensor."));
                        ui.label(t(self.lang, "Registo de cortes e leituras: ~/.cache/sillinux/cut.log"));
                        ui.label(t(self.lang, "A app lê as marcas antes de cortar; uma leitura falhada cancela o corte."));
                        ui.label(t(self.lang, "A leitura de marcas no Cameo 5 ainda não está validada fisicamente."));

                        ui.add_space(8.0);
                        ui.separator();
                        ui.heading(t(self.lang, "Linha de comandos"));
                        for command in [
                            "silhouette inspect desenho.studio3",
                            "silhouette svg desenho.studio3 saida.svg",
                            "silhouette regmarks marcas.svg --four-corner",
                            "silhouette print-sheet desenho.studio3 folha.pdf --paths",
                            "silhouette probe",
                            "silhouette cut-file desenho.studio3 --dry-run",
                            "silhouette cut-file desenho.studio3 --yes",
                        ] {
                            ui.label(
                                egui::RichText::new(command)
                                    .monospace()
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(24, 104, 200)),
                            );
                        }
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new(
                                "Cortes e digitalizações reais exigem confirmação e máquina ligada.",
                            )
                            .size(11.0)
                            .italics(),
                        );
                    });
            });
        self.help_open = open;
    }

    fn confirm_window(&mut self, ctx: &egui::Context) {
        if !self.confirm {
            return;
        }
        let mut open = true;
        let mut go = false;
        let mut cancel = false;
        egui::Window::new(t(self.lang, "Confirmar corte"))
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(t(self.lang, "A lâmina vai mover-se sobre o tapete."));
                ui.label(t(self.lang, "Confirme a origem da página e a lâmina instalada."));
                ui.separator();
                ui.label(format!(
                    "ferramenta {} · velocidade {} · força {} · profundidade {}",
                    self.settings.tool,
                    self.settings.speed,
                    self.settings.force,
                    self.settings.depth
                ));
                let mut chosen: Vec<String> = Vec::new();
                for slot in 0..2 {
                    if self.machine_cut[slot] {
                        chosen.push(format!(
                            "Silhouette {}{}",
                            slot + 1,
                            if self.machines[slot].connected() {
                                ""
                            } else {
                                " (desligada)"
                            }
                        ));
                    }
                }
                ui.label(format!(
                    "Destino: {}",
                    if chosen.is_empty() {
                        "nenhuma máquina escolhida".to_string()
                    } else {
                        chosen.join(" + ")
                    }
                ));
                ui.label(format!(
                    "Marcas de registo: {}",
                    if self.scan_marks {
                        "ler antes de cortar"
                    } else {
                        "não ler"
                    }
                ));
                if self.dry_run {
                    ui.label(t(self.lang, "Modo dry-run: nada será enviado."));
                }
                ui.horizontal(|ui| {
                    if ui
                        .button(if self.dry_run { "Simular" } else { "Cortar" })
                        .clicked()
                    {
                        go = true;
                    }
                    if ui.button(t(self.lang, "Cancelar")).clicked() {
                        cancel = true;
                    }
                });
            });
        if go {
            self.confirm = false;
            self.run_cut();
        } else if cancel || !open {
            self.confirm = false;
        }
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1320.0, 880.0])
            .with_min_inner_size([960.0, 620.0])
            .with_title("Sillinux"),
        ..Default::default()
    };
    eframe::run_native(
        "Sillinux",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
