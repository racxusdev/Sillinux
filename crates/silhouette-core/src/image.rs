//! Embedded raster images (PNG / JPEG).

/// Kind of embedded raster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Png,
    Jpeg,
}

/// A raster image embedded in the container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Image<'a> {
    /// Byte offset of the image signature.
    pub offset: usize,
    /// Byte offset just past the end of the image.
    pub end: usize,
    pub kind: ImageKind,
    pub width: u32,
    pub height: u32,
    /// The raw encoded image bytes (`data[offset..end]`).
    pub data: &'a [u8],
}

pub(crate) const PNG_SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
pub(crate) const JPEG_SOI: [u8; 3] = [0xff, 0xd8, 0xff];

fn be16(d: &[u8], at: usize) -> Option<u16> {
    let s = d.get(at..at + 2)?;
    Some(u16::from_be_bytes([s[0], s[1]]))
}

fn be32(d: &[u8], at: usize) -> Option<u32> {
    let s = d.get(at..at + 4)?;
    Some(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

/// Validate and parse a PNG beginning at `off`.
pub(crate) fn png_at(d: &[u8], off: usize) -> Option<Image<'_>> {
    if d.get(off..off + 8)? != PNG_SIG {
        return None;
    }
    // First chunk must be IHDR with length 13.
    if be32(d, off + 8)? != 13 || d.get(off + 12..off + 16)? != b"IHDR" {
        return None;
    }
    let width = be32(d, off + 16)?;
    let height = be32(d, off + 20)?;
    if width == 0 || height == 0 || width > 100_000 || height > 100_000 {
        return None;
    }
    // Find IEND: length(4) == 0, type(4) == "IEND", crc(4).
    let mut p = off + 8;
    let limit = (off + 64 * 1024 * 1024).min(d.len());
    while p + 12 <= limit {
        if &d[p..p + 4] == b"IEND" {
            let end = p + 8;
            return Some(Image {
                offset: off,
                end,
                kind: ImageKind::Png,
                width,
                height,
                data: d.get(off..end)?,
            });
        }
        p += 1;
    }
    None
}

/// Validate and parse a JPEG beginning at `off`.
pub(crate) fn jpeg_at(d: &[u8], off: usize) -> Option<Image<'_>> {
    if d.get(off..off + 3)? != JPEG_SOI {
        return None;
    }
    let mut i = off + 2;
    let limit = (off + 64 * 1024 * 1024).min(d.len());
    while i + 4 <= limit {
        if d[i] != 0xff {
            return None;
        }
        let marker = d[i + 1];
        // Standalone markers (no length field).
        if marker == 0xd9 {
            return None; // EOI before any SOF: not a real image
        }
        if (0xd0..=0xd7).contains(&marker) || marker == 0x01 {
            i += 2;
            continue;
        }
        let len = usize::from(be16(d, i + 2)?);
        if len < 2 {
            return None;
        }
        let is_sof =
            (0xc0..=0xcf).contains(&marker) && marker != 0xc4 && marker != 0xc8 && marker != 0xcc;
        if is_sof {
            let height = u32::from(be16(d, i + 5)?);
            let width = u32::from(be16(d, i + 7)?);
            if width == 0 || height == 0 {
                return None;
            }
            let end = find_eoi(d, i, limit)?;
            return Some(Image {
                offset: off,
                end,
                kind: ImageKind::Jpeg,
                width,
                height,
                data: d.get(off..end)?,
            });
        }
        i += 2 + len;
    }
    None
}

fn find_eoi(d: &[u8], from: usize, limit: usize) -> Option<usize> {
    let mut p = from;
    while p + 2 <= limit {
        if d[p] == 0xff && d[p + 1] == 0xd9 {
            return Some(p + 2);
        }
        p += 1;
    }
    None
}
