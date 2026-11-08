//! Parser for the Silhouette Studio `.studio3` container.
//!
//! # Scope
//!
//! This crate implements **only what was verified against real files and the
//! device**. It deliberately does not guess at the vector-geometry encoding.
//!
//! Verified and implemented here:
//!
//! * 13-byte magic `silhouette05;` at offset 0, followed by 3 NUL bytes.
//! * 16 little-endian `u64` header fields at `0x10`..`0x90`.
//! * A second `silhouette05;` document always starts at offset **1024**.
//! * `u32`-LE length-prefixed UTF-8 strings (material names, layer names,
//!   32-hex UUIDs, object names) between the nested document and the first
//!   embedded image.
//! * Embedded PNG and JPEG images, with dimensions and byte ranges.
//! * Trailing `u64`-length-prefixed, base64-encoded JSON chunks (line-wrapped
//!   with CRLF) carrying `{"root_uuid": ..., "children": ...}`.
//! * Polygonal paths (`create_line`, `create_rectangle`): 5-byte vertex marker
//!   plus `f32` millimetre coordinates. See [`geometry`].
//! * Page size: two `f32` millimetres at offsets **1149** and **1153** (width,
//!   height). Verified on 36 files covering A4, A4 landscape, US Letter, 12×12
//!   in (304.8 mm) and 9×12 in; the remaining 15 are calibration/template files
//!   whose fields are zero.
//!
//! **Not implemented**: bezier curves (`create_curve`) and text shapes. Raster
//! images are extracted but not traced; a raster-only document therefore has no
//! paths. Nothing in this crate fabricates paths from the opaque bytes.
//!
//! # Example
//!
//! ```no_run
//! let bytes = std::fs::read("design.studio3")?;
//! let doc = silhouette_core::Container::parse(&bytes)?;
//! println!("{} images, uuid {:?}", doc.images.len(), doc.root_uuid);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![forbid(unsafe_code)]

mod b64;
mod error;
mod geometry;
mod image;
mod regmarks;
mod scan;
mod trace;

pub use error::Error;
pub use geometry::{
    decode as decode_paths, raster_placement, to_svg, Path, PathKind, Point, RasterPlacement,
};
pub use image::{Image, ImageKind};
pub use regmarks::{regmark_shapes, regmarks_svg, RegMarkShape, RegMarkSpec, RegMarkStyle};
pub use trace::{
    offset_polygon, point_in_polygon, polish, simplify, trace_rgba, TraceMode, TraceOptions,
    TracedPath,
};

/// File signature of the outer document.
pub const MAGIC: &[u8; 13] = b"silhouette05;";
/// Size of the fixed outer header. The nested document starts at this offset.
pub const OUTER_HEADER_LEN: usize = 1024;
/// Number of `u64` fields in the outer header (`0x10`..`0x90`).
pub const HEADER_FIELDS: usize = 16;

/// One `u32`-length-prefixed UTF-8 string found in the metadata region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StringEntry<'a> {
    /// Byte offset of the `u32` length prefix.
    pub offset: usize,
    /// The decoded string.
    pub value: &'a str,
}

/// One `u64`-length-prefixed base64 chunk that decodes to JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrailingChunk {
    /// Byte offset of the `u64` length prefix.
    pub offset: usize,
    /// The length prefix value (including the CRLF line breaks in the base64 text).
    pub encoded_len: usize,
    /// The decoded JSON bytes.
    pub decoded: Vec<u8>,
}

/// A parsed `.studio3` container.
#[derive(Debug, Clone)]
pub struct Container<'a> {
    /// Raw file bytes.
    pub data: &'a [u8],
    /// The 16 outer header fields, exactly as stored. Semantics are **unknown**;
    /// do not rely on them without further evidence.
    pub header: [u64; HEADER_FIELDS],
    /// Offset of the nested `silhouette05;` document (1024 in every sample).
    pub nested_doc_offset: usize,
    /// Length-prefixed UTF-8 strings from the metadata region.
    pub strings: Vec<StringEntry<'a>>,
    /// Embedded raster images, ordered by offset.
    pub images: Vec<Image<'a>>,
    /// Trailing base64/JSON chunks.
    pub trailing: Vec<TrailingChunk>,
    /// `root_uuid` from the trailing JSON, when present.
    pub root_uuid: Option<String>,
    /// Page size in millimetres (`width`, `height`), read from offsets 1149 and
    /// 1153. `None` for calibration/template files where those fields are zero.
    pub page_size_mm: Option<(f32, f32)>,
    /// Decoded polygonal paths (see [`geometry`] for the verified encoding and
    /// its limits).
    pub paths: Vec<Path>,
}

impl<'a> Container<'a> {
    /// Parse a `.studio3` file from memory.
    pub fn parse(data: &'a [u8]) -> Result<Self, Error> {
        if data.len() < OUTER_HEADER_LEN {
            return Err(Error::TooShort {
                need: OUTER_HEADER_LEN,
                got: data.len(),
            });
        }
        if &data[..MAGIC.len()] != MAGIC {
            return Err(Error::BadMagic);
        }
        if data[MAGIC.len()..MAGIC.len() + 3] != [0, 0, 0] {
            return Err(Error::BadMagicPadding);
        }

        let mut header = [0u64; HEADER_FIELDS];
        for (i, slot) in header.iter_mut().enumerate() {
            let start = 0x10 + i * 8;
            *slot = u64::from_le_bytes(data[start..start + 8].try_into().unwrap());
        }

        let nested_doc_offset =
            scan::find_magic(data, OUTER_HEADER_LEN).ok_or(Error::NoNestedDocument)?;

        let images = scan::images(data);
        let strings_end = images.first().map(|i| i.offset).unwrap_or(data.len());
        let strings = scan::strings(data, nested_doc_offset, strings_end);
        let trailing = scan::trailing_chunks(data);
        let root_uuid = trailing.iter().find_map(|c| scan::root_uuid(&c.decoded));
        let page_size_mm = page_size(data);
        // Contours in raster documents live on the artwork's pixel grid; give
        // the decoder the page size and largest image so it can map them to
        // page millimetres via the repeated raster transform.
        let largest_image = images
            .iter()
            .max_by_key(|i| u64::from(i.width) * u64::from(i.height))
            .map(|i| (i.width, i.height));
        let paths = geometry::decode_with_hints(
            data,
            geometry::DecodeHints::from_page_and_image(page_size_mm, largest_image),
        );

        Ok(Self {
            data,
            header,
            nested_doc_offset,
            strings,
            images,
            trailing,
            root_uuid,
            page_size_mm,
            paths,
        })
    }

    /// All metadata strings, in file order.
    pub fn string_values(&self) -> impl Iterator<Item = &'a str> + '_ {
        self.strings.iter().map(|s| s.value)
    }

    /// Every 32-character lowercase hex string found in the metadata region
    /// (Silhouette object/layer identifiers).
    pub fn guid_strings(&self) -> Vec<&'a str> {
        self.string_values()
            .filter(|s| {
                s.len() == 32
                    && s.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
            .collect()
    }
}

/// Offset of the page width (`f32` LE, millimetres).
pub const PAGE_WIDTH_OFFSET: usize = 1149;
/// Offset of the page height (`f32` LE, millimetres).
pub const PAGE_HEIGHT_OFFSET: usize = 1153;

/// Read the page size from the serialized document settings.
///
/// Returns `None` when the fields are absent, non-finite or outside a plausible
/// range (10..=2000 mm). Calibration/template files store zero here.
fn page_size(data: &[u8]) -> Option<(f32, f32)> {
    let w = f32::from_le_bytes(
        data.get(PAGE_WIDTH_OFFSET..PAGE_WIDTH_OFFSET + 4)?
            .try_into()
            .ok()?,
    );
    let h = f32::from_le_bytes(
        data.get(PAGE_HEIGHT_OFFSET..PAGE_HEIGHT_OFFSET + 4)?
            .try_into()
            .ok()?,
    );
    let sane = |v: f32| v.is_finite() && (10.0..=2000.0).contains(&v);
    (sane(w) && sane(h)).then_some((w, h))
}
