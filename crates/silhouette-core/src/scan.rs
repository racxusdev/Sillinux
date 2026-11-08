//! Low-level scanners over the raw container bytes.

use crate::b64;
use crate::image::{self, Image};
use crate::{StringEntry, TrailingChunk, MAGIC};

/// Find the first occurrence of the file magic at or after `from`.
pub(crate) fn find_magic(d: &[u8], from: usize) -> Option<usize> {
    if from >= d.len() {
        return None;
    }
    d[from..]
        .windows(MAGIC.len())
        .position(|w| w == MAGIC)
        .map(|p| p + from)
}

/// Scan the whole buffer for validated PNG and JPEG images.
///
/// The scan is linear: after a validated image, it resumes past its end, so
/// images are never reported twice or overlapping.
pub(crate) fn images(d: &[u8]) -> Vec<Image<'_>> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 3 <= d.len() {
        if d[i..].starts_with(&image::PNG_SIG) {
            if let Some(img) = image::png_at(d, i) {
                i = img.end;
                out.push(img);
                continue;
            }
        }
        if d[i..].starts_with(&image::JPEG_SOI) {
            if let Some(img) = image::jpeg_at(d, i) {
                i = img.end;
                out.push(img);
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Extract `u32`-LE length-prefixed UTF-8 strings in `[start, end)`.
///
/// The framing was verified on real files: a 4-byte little-endian length
/// followed by exactly that many UTF-8 bytes. To avoid false positives in the
/// binary object graph, a candidate is only accepted when:
///
/// * `1 <= len <= 4096`,
/// * the bytes are valid UTF-8 made of printable ASCII plus `\t\r\n`,
/// * it contains at least `max(2, len/3)` ASCII letters.
pub(crate) fn strings<'a>(d: &'a [u8], start: usize, end: usize) -> Vec<StringEntry<'a>> {
    let end = end.min(d.len());
    let mut out = Vec::new();
    let mut i = start;

    while i + 4 <= end {
        let len = u32::from_le_bytes([d[i], d[i + 1], d[i + 2], d[i + 3]]) as usize;
        if (1..=4096).contains(&len) && i + 4 + len <= end {
            let raw = &d[i + 4..i + 4 + len];
            if let Ok(s) = std::str::from_utf8(raw) {
                let printable = s.chars().all(|c| {
                    (c as u32) >= 32 && (c as u32) < 127 || matches!(c, '\t' | '\r' | '\n')
                });
                let letters = s.chars().filter(|c| c.is_ascii_alphabetic()).count();
                if printable && letters >= (len / 3).max(2) {
                    out.push(StringEntry {
                        offset: i,
                        value: s,
                    });
                    i += 4 + len;
                    continue;
                }
            }
        }
        i += 1;
    }

    out
}

/// Scan for `u64`-LE length-prefixed base64 chunks that decode to a JSON object.
///
/// The base64 text is line-wrapped with CRLF every 76 characters, and the
/// stored length includes those line breaks.
pub(crate) fn trailing_chunks(d: &[u8]) -> Vec<TrailingChunk> {
    const WINDOW: usize = 128 * 1024;
    let start = d.len().saturating_sub(WINDOW);
    let mut out = Vec::new();
    let mut i = start;

    while i + 8 <= d.len() {
        let len = u64::from_le_bytes(d[i..i + 8].try_into().unwrap()) as usize;
        if (16..=1_000_000).contains(&len) && i + 8 + len <= d.len() {
            let blob = &d[i + 8..i + 8 + len];
            let clean: Vec<u8> = blob
                .iter()
                .copied()
                .filter(|&b| b != b'\r' && b != b'\n')
                .collect();
            if !clean.is_empty() && clean.len() % 4 == 0 {
                if let Some(decoded) = b64::decode(&clean) {
                    if decoded.starts_with(b"{") {
                        out.push(TrailingChunk {
                            offset: i,
                            encoded_len: len,
                            decoded,
                        });
                        i += 8 + len;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }

    out
}

/// Extract `"root_uuid":"<32 hex>"` from decoded JSON bytes.
pub(crate) fn root_uuid(json: &[u8]) -> Option<String> {
    const KEY: &[u8] = b"\"root_uuid\":\"";
    let at = json.windows(KEY.len()).position(|w| w == KEY)?;
    let start = at + KEY.len();
    let id = json.get(start..start + 32)?;
    if id
        .iter()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
    {
        Some(String::from_utf8_lossy(id).into_owned())
    } else {
        None
    }
}
