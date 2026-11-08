//! User preferences persisted in `~/.config/sillinux/prefs.json`.
//!
//! Only UI-level settings are stored (trace/print/cut parameters and the last
//! CUPS printer). Documents stay where the user saved them; machine sessions
//! are never persisted — they are re-established on demand.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use silhouette_driver::CutSettings;

use crate::{App, PrintColor, PrintMedia, PrintQuality};

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Prefs {
    trace_mode: Option<String>,
    trace_threshold: Option<f32>,
    trace_edge: Option<f32>,
    trace_simplify: Option<f32>,
    trace_min_length: Option<f32>,
    trace_min_area: Option<f32>,
    trace_studio_noise: Option<f32>,
    trace_smooth: Option<u8>,
    trace_separate: Option<bool>,
    offset_mm: Option<f32>,
    print_marks: Option<bool>,
    print_paths: Option<bool>,
    print_art: Option<bool>,
    print_quality: Option<String>,
    print_color: Option<String>,
    print_media: Option<String>,
    tool: Option<u8>,
    speed: Option<u8>,
    force: Option<u8>,
    depth: Option<u8>,
    last_printer: Option<String>,
    lang: Option<String>,
    welcome_hidden: Option<bool>,
}

fn prefs_path() -> Option<PathBuf> {
    let dir = match std::env::var("XDG_CONFIG_HOME") {
        Ok(dir) if !dir.is_empty() => PathBuf::from(dir).join("sillinux"),
        _ => {
            let home = std::env::var("HOME").ok()?;
            if home.is_empty() {
                return None;
            }
            PathBuf::from(home).join(".config").join("sillinux")
        }
    };
    Some(dir.join("prefs.json"))
}

impl Prefs {
    /// Read the stored preferences; missing/corrupt file means defaults.
    fn load() -> Self {
        let Some(path) = prefs_path() else {
            return Self::default();
        };
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Write the preferences; failures are surfaced to the caller.
    fn save(&self) -> Result<(), String> {
        let Some(path) = prefs_path() else {
            return Err("sem $HOME: não foi possível guardar preferências".to_string());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(|e| e.to_string())
    }
}

fn quality_key(q: PrintQuality) -> &'static str {
    match q {
        PrintQuality::Normal => "normal",
        PrintQuality::High => "high",
    }
}

fn quality_from(key: &str) -> Option<PrintQuality> {
    match key {
        "normal" => Some(PrintQuality::Normal),
        "high" => Some(PrintQuality::High),
        _ => None,
    }
}

fn color_key(c: PrintColor) -> &'static str {
    match c {
        PrintColor::Rgb => "rgb",
        PrintColor::Gray => "gray",
        PrintColor::AutoGray => "autogray",
        PrintColor::ProcessGray => "processgray",
    }
}

fn color_from(key: &str) -> Option<PrintColor> {
    match key {
        "rgb" => Some(PrintColor::Rgb),
        "gray" => Some(PrintColor::Gray),
        "autogray" => Some(PrintColor::AutoGray),
        "processgray" => Some(PrintColor::ProcessGray),
        _ => None,
    }
}

fn media_key(m: PrintMedia) -> &'static str {
    match m {
        PrintMedia::Stationery => "stationery",
        PrintMedia::Photographic => "photographic",
        PrintMedia::PhotographicMatte => "photographic-matte",
        PrintMedia::PhotographicGlossy => "photographic-glossy",
        PrintMedia::PhotographicSemiGloss => "photographic-semigloss",
        PrintMedia::PhotographicHighGloss => "photographic-highgloss",
        PrintMedia::StationeryCoated => "stationery-coated",
        PrintMedia::Envelope => "envelope",
    }
}

fn media_from(key: &str) -> Option<PrintMedia> {
    match key {
        "stationery" => Some(PrintMedia::Stationery),
        "photographic" => Some(PrintMedia::Photographic),
        "photographic-matte" => Some(PrintMedia::PhotographicMatte),
        "photographic-glossy" => Some(PrintMedia::PhotographicGlossy),
        "photographic-semigloss" => Some(PrintMedia::PhotographicSemiGloss),
        "photographic-highgloss" => Some(PrintMedia::PhotographicHighGloss),
        "stationery-coated" => Some(PrintMedia::StationeryCoated),
        "envelope" => Some(PrintMedia::Envelope),
        _ => None,
    }
}

/// Apply the stored preferences to a fresh `App` (called from `App::new`).
pub fn load_into(app: &mut App, printers: &[String]) {
    let stored = Prefs::load();
    if let Some(mode) = stored
        .trace_mode
        .as_deref()
        .and_then(crate::TraceMethod::from_key)
    {
        app.trace_method = mode;
    }
    app.trace_threshold = stored.trace_threshold.unwrap_or(app.trace_threshold);
    app.trace_edge = stored.trace_edge.unwrap_or(app.trace_edge);
    app.trace_simplify = stored.trace_simplify.unwrap_or(app.trace_simplify);
    app.trace_min_length = stored.trace_min_length.unwrap_or(app.trace_min_length);
    app.trace_min_area = stored.trace_min_area.unwrap_or(app.trace_min_area);
    app.trace_studio_noise = stored.trace_studio_noise.unwrap_or(app.trace_studio_noise);
    app.trace_smooth = stored.trace_smooth.unwrap_or(app.trace_smooth);
    app.trace_separate = stored.trace_separate.unwrap_or(app.trace_separate);
    app.offset_mm = stored.offset_mm.unwrap_or(app.offset_mm);
    app.print_marks = stored.print_marks.unwrap_or(app.print_marks);
    app.print_paths = stored.print_paths.unwrap_or(app.print_paths);
    app.print_art = stored.print_art.unwrap_or(app.print_art);
    app.print_quality = stored
        .print_quality
        .as_deref()
        .and_then(quality_from)
        .unwrap_or(app.print_quality);
    app.print_color = stored
        .print_color
        .as_deref()
        .and_then(color_from)
        .unwrap_or(app.print_color);
    app.print_media = stored
        .print_media
        .as_deref()
        .and_then(media_from)
        .unwrap_or(app.print_media);
    if let (Some(tool), Some(speed), Some(force), Some(depth)) =
        (stored.tool, stored.speed, stored.force, stored.depth)
    {
        app.settings = CutSettings {
            tool,
            speed,
            force,
            depth,
        };
    }
    if let Some(printer) = &stored.last_printer {
        if let Some(i) = printers.iter().position(|q| q == printer) {
            app.printer_index = i;
        }
    }
    if let Some(lang) = stored.lang.as_deref().and_then(crate::i18n::Lang::from_key) {
        app.lang = lang;
    }
    if let Some(hidden) = stored.welcome_hidden {
        app.welcome_hidden = hidden;
    }
}

/// Persist the current `App` settings (called on close and after cuts).
pub fn save_from(app: &App) -> Result<(), String> {
    let prefs = Prefs {
        trace_mode: Some(app.trace_method.key().to_string()),
        trace_threshold: Some(app.trace_threshold),
        trace_edge: Some(app.trace_edge),
        trace_simplify: Some(app.trace_simplify),
        trace_min_length: Some(app.trace_min_length),
        trace_min_area: Some(app.trace_min_area),
        trace_studio_noise: Some(app.trace_studio_noise),
        trace_smooth: Some(app.trace_smooth),
        trace_separate: Some(app.trace_separate),
        offset_mm: Some(app.offset_mm),
        print_marks: Some(app.print_marks),
        print_paths: Some(app.print_paths),
        print_art: Some(app.print_art),
        print_quality: Some(quality_key(app.print_quality).to_string()),
        print_color: Some(color_key(app.print_color).to_string()),
        print_media: Some(media_key(app.print_media).to_string()),
        tool: Some(app.settings.tool),
        speed: Some(app.settings.speed),
        force: Some(app.settings.force),
        depth: Some(app.settings.depth),
        last_printer: app.printers.get(app.printer_index).cloned(),
        lang: Some(app.lang.key().to_string()),
        welcome_hidden: Some(app.welcome_hidden),
    };
    prefs.save()
}
