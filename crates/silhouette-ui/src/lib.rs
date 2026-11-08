//! Presentation logic for the Silhouette UI.
//!
//! This crate deliberately has **no GUI dependency**. It turns the parser and
//! driver types into the small, testable pieces a canvas/panel UI needs, so the
//! toolkit decision (see `docs/UI_DECISION.md`) stays reversible.
//!
//! ```no_run
//! let bytes = std::fs::read("design.studio3")?;
//! let summary = silhouette_ui::DocumentSummary::parse("design.studio3", &bytes)?;
//! println!("{} images, {} strings", summary.images.len(), summary.strings);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![forbid(unsafe_code)]

use silhouette_core::{Container, ImageKind};
use silhouette_driver::{Job, Status};

/// One embedded raster, as shown in the layer panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageSummary {
    pub kind: ImageKind,
    pub width: u32,
    pub height: u32,
    pub offset: usize,
    pub bytes: usize,
}

/// What the UI can display about an open document today.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentSummary {
    pub path: String,
    pub bytes: usize,
    pub nested_doc_offset: usize,
    pub strings: usize,
    pub guids: usize,
    pub images: Vec<ImageSummary>,
    pub root_uuid: Option<String>,
    /// Always `false` until the geometry decoder lands (milestone M1).
    pub geometry_decoded: bool,
}

impl DocumentSummary {
    pub fn parse(path: &str, data: &[u8]) -> Result<Self, silhouette_core::Error> {
        let doc = Container::parse(data)?;
        Ok(Self::from_container(path, &doc))
    }

    pub fn from_container(path: &str, doc: &Container<'_>) -> Self {
        Self {
            path: path.to_string(),
            bytes: doc.data.len(),
            nested_doc_offset: doc.nested_doc_offset,
            strings: doc.strings.len(),
            guids: doc.guid_strings().len(),
            images: doc
                .images
                .iter()
                .map(|i| ImageSummary {
                    kind: i.kind,
                    width: i.width,
                    height: i.height,
                    offset: i.offset,
                    bytes: i.data.len(),
                })
                .collect(),
            root_uuid: doc.root_uuid.clone(),
            geometry_decoded: !doc.paths.is_empty(),
        }
    }

    /// One-line status for the title bar.
    pub fn headline(&self) -> String {
        let geometry = if self.geometry_decoded {
            "geometry decoded"
        } else {
            "geometry not decoded"
        };
        format!(
            "{} — {} image(s), {} strings, {geometry}",
            self.path,
            self.images.len(),
            self.strings
        )
    }
}

/// Human-readable label for the cutter status.
pub fn status_label(status: Status) -> &'static str {
    match status {
        Status::Ready => "Ready",
        Status::Moving => "Moving",
        Status::Unloaded => "Media not detected",
        Status::Paused => "Paused",
        Status::Cancelled => "Cancelled",
        Status::Unknown(_) => "Unknown",
    }
}

/// The job preview: one display line per command, exactly as it will be sent.
pub fn job_preview(job: &Job) -> Vec<String> {
    job.commands()
        .iter()
        .map(|cmd| {
            let body = String::from_utf8_lossy(cmd);
            let body = body.trim_end_matches('\u{3}');
            format!("{body}  [{} B]", cmd.len())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use silhouette_driver::golden_square_cut;

    #[test]
    fn preview_has_one_line_per_command() {
        let job = golden_square_cut();
        let lines = job_preview(&job);
        assert_eq!(lines.len(), job.len());
        assert_eq!(lines[0], "\u{1b}\u{4}  [2 B]");
        assert!(lines.iter().any(|l| l.starts_with("M200,200")));
    }

    #[test]
    fn status_labels_cover_all_variants() {
        for s in [
            Status::Ready,
            Status::Moving,
            Status::Unloaded,
            Status::Paused,
            Status::Cancelled,
            Status::Unknown(b'9'),
        ] {
            assert!(!status_label(s).is_empty());
        }
    }
}
