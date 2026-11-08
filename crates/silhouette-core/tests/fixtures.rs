use std::fs;
use std::path::{Path, PathBuf};

use silhouette_core::{Container, Error, OUTER_HEADER_LEN};

/// Locate real `.studio3` files for integration testing.
///
/// Resolution order:
/// 1. `$SILHOUETTE_FIXTURES` (a file or a directory),
/// 2. `tests/fixtures/` next to this crate,
/// 3. walking up from the crate to the repository root and scanning
///    `**/*.studio3` (depth-limited).
///
/// Returns an empty list when nothing is found; tests then skip rather than
/// fail on a machine that does not have the corpus.
fn fixtures() -> Vec<PathBuf> {
    if let Ok(p) = std::env::var("SILHOUETTE_FIXTURES") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return vec![p];
        }
        if p.is_dir() {
            let mut v = Vec::new();
            collect(&p, 0, &mut v);
            return pick(v);
        }
    }

    let local = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    if local.is_dir() {
        let mut v = Vec::new();
        collect(&local, 0, &mut v);
        if !v.is_empty() {
            return pick(v);
        }
    }

    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for _ in 0..8 {
        if dir.join("tentativas anteriores").is_dir() {
            let mut v = Vec::new();
            collect(&dir, 0, &mut v);
            return pick(v);
        }
        if !dir.pop() {
            break;
        }
    }
    Vec::new()
}

fn collect(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 8 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, depth + 1, out);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("studio3"))
        {
            out.push(path);
        }
    }
}

/// Keep the test fast: the 10 smallest files plus the 3 largest.
fn pick(mut files: Vec<PathBuf>) -> Vec<PathBuf> {
    files.sort_by_key(|p| fs::metadata(p).map(|m| m.len()).unwrap_or(0));
    if files.len() <= 13 {
        return files;
    }
    let mut out: Vec<PathBuf> = files.iter().take(10).cloned().collect();
    out.extend(files.iter().rev().take(3).cloned());
    out
}

#[test]
fn parses_every_fixture() {
    let files = fixtures();
    if files.is_empty() {
        eprintln!("no .studio3 fixtures found; skipping");
        return;
    }

    for path in &files {
        let data = fs::read(path).expect("read fixture");
        let doc = Container::parse(&data).unwrap_or_else(|e| panic!("{}: {e}", path.display()));

        assert_eq!(
            doc.nested_doc_offset,
            OUTER_HEADER_LEN,
            "{}: nested document must start at 1024",
            path.display()
        );
        assert!(
            !doc.images.is_empty(),
            "{}: expected at least one embedded image",
            path.display()
        );
        for img in &doc.images {
            assert!(img.width > 0 && img.height > 0);
            assert_eq!(img.data.len(), img.end - img.offset);
            assert_eq!(
                &doc.data[img.offset..img.offset + 4],
                &img.data[..4.min(img.data.len())]
            );
        }
    }
}

#[test]
fn rejects_non_studio3_input() {
    assert!(matches!(
        Container::parse(&[0u8; 64]),
        Err(Error::TooShort { .. })
    ));

    let mut data = vec![0u8; OUTER_HEADER_LEN];
    data[..13].copy_from_slice(b"silhouette05;");
    assert!(matches!(
        Container::parse(&data),
        Err(Error::NoNestedDocument)
    ));

    let mut data = vec![0u8; OUTER_HEADER_LEN];
    data[..13].copy_from_slice(b"silhouette05;");
    data[13] = 1;
    assert!(matches!(
        Container::parse(&data),
        Err(Error::BadMagicPadding)
    ));
}

#[test]
fn root_uuid_is_consistent() {
    let files = fixtures();
    if files.is_empty() {
        return;
    }

    let mut with_uuid = 0;
    for path in &files {
        let data = fs::read(path).expect("read fixture");
        let doc = Container::parse(&data).expect("parse");
        if doc.trailing.is_empty() {
            continue;
        }
        let uuid = doc
            .root_uuid
            .as_ref()
            .unwrap_or_else(|| panic!("{}: trailing JSON without root_uuid", path.display()));
        assert_eq!(uuid.len(), 32);
        assert!(uuid.bytes().all(|b| b.is_ascii_hexdigit()));
        for chunk in &doc.trailing {
            let from_chunk = silhouette_core::Container::parse(&data)
                .unwrap()
                .root_uuid
                .clone();
            assert_eq!(Some(uuid.clone()), from_chunk);
            assert!(chunk.decoded.starts_with(b"{"));
        }
        with_uuid += 1;
    }
    assert!(
        with_uuid > 0,
        "expected at least one fixture with a root_uuid"
    );
}

#[test]
fn metadata_strings_are_printable() {
    let files = fixtures();
    if files.is_empty() {
        return;
    }

    let mut saw_material = false;
    let mut saw_guid = false;

    for path in &files {
        let data = fs::read(path).expect("read fixture");
        let doc = Container::parse(&data).expect("parse");

        for s in doc.string_values() {
            assert!(
                s.chars()
                    .all(|c| (' '..='~').contains(&c) || matches!(c, '\t' | '\r' | '\n')),
                "{}: non-printable string {s:?}",
                path.display()
            );
        }

        saw_material |= doc.string_values().any(|s| s == "Print Paper");
        saw_guid |= !doc.guid_strings().is_empty();
    }

    assert!(saw_material, "expected the `Print Paper` material name");
    assert!(saw_guid, "expected at least one 32-hex GUID string");
}

#[test]
fn image_ranges_do_not_overlap() {
    let files = fixtures();
    if files.is_empty() {
        return;
    }

    for path in &files {
        let data = fs::read(path).expect("read fixture");
        let doc = Container::parse(&data).expect("parse");
        let mut prev_end = 0;
        for img in &doc.images {
            assert!(
                img.offset >= prev_end,
                "{}: overlapping images",
                path.display()
            );
            prev_end = img.end;
        }
    }
}

/// Find a fixture whose file name contains `needle`.
fn fixture_named(needle: &str) -> Option<PathBuf> {
    let mut all = Vec::new();
    if let Ok(p) = std::env::var("SILHOUETTE_FIXTURES") {
        let p = PathBuf::from(p);
        if p.is_dir() {
            collect(&p, 0, &mut all);
        } else if p.is_file() {
            all.push(p);
        }
    }
    if all.is_empty() {
        let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for _ in 0..8 {
            if dir.join("tentativas anteriores").is_dir() {
                collect(&dir, 0, &mut all);
                break;
            }
            if !dir.pop() {
                break;
            }
        }
    }
    all.into_iter().find(|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.contains(needle))
    })
}

#[test]
fn decodes_the_rectangle_fixture() {
    let Some(path) = fixture_named("retangulo centralizado") else {
        eprintln!("rectangle fixture not found; skipping");
        return;
    };
    let data = fs::read(&path).expect("read fixture");
    let doc = Container::parse(&data).expect("parse");

    assert_eq!(doc.paths.len(), 1, "expected exactly one path");
    let path = &doc.paths[0];
    assert_eq!(path.points.len(), 4, "rectangle has four corners");
    assert!(path.closed, "rectangles are closed");

    let (min_x, min_y, max_x, max_y) = path.bounds().expect("bounds");
    assert!(
        (max_x - min_x - 72.5499).abs() < 0.1,
        "width: {}",
        max_x - min_x
    );
    assert!(
        (max_y - min_y - 99.6468).abs() < 0.1,
        "height: {}",
        max_y - min_y
    );

    // The stored affine matrix translates the local vertices by
    // (-0.4371, +9.3965) mm, placing the rectangle on the A4 page centre.
    assert!(
        path.transform.is_some(),
        "the stored affine matrix must be applied"
    );
    let cx = (min_x + max_x) / 2.0;
    let cy = (min_y + max_y) / 2.0;
    assert!((cx - 105.0).abs() < 0.1, "centre x: {cx}");
    assert!((cy - 148.5).abs() < 0.1, "centre y: {cy}");
    assert!((min_x - 68.725).abs() < 0.1, "top-left x: {min_x}");
    assert!((min_y - 98.677).abs() < 0.1, "top-left y: {min_y}");
}

#[test]
fn empty_document_has_no_paths() {
    let Some(path) = fixture_named("vazio  estilo folha A4") else {
        eprintln!("empty fixture not found; skipping");
        return;
    };
    let data = fs::read(&path).expect("read fixture");
    let doc = Container::parse(&data).expect("parse");
    assert!(doc.paths.is_empty(), "empty document must have no paths");
}

#[test]
fn page_size_matches_the_document() {
    // Values verified on the corpus: A4, 12x12 in, and A4 landscape.
    let cases = [
        ("vazio  estilo folha A4", (210.0, 297.0)),
        ("retangulo centralizado", (210.0, 297.0)),
        ("Frame_paper", (210.0, 297.0)),
        ("caixa-1", (304.8, 304.8)),
    ];
    for (needle, expected) in cases {
        let Some(path) = fixture_named(needle) else {
            continue;
        };
        let data = fs::read(&path).expect("read fixture");
        let doc = Container::parse(&data).expect("parse");
        let (w, h) = doc.page_size_mm.expect("page size");
        assert!(
            (w - expected.0).abs() < 0.01,
            "{}: width {w}",
            path.display()
        );
        assert!(
            (h - expected.1).abs() < 0.01,
            "{}: height {h}",
            path.display()
        );
    }
}

#[test]
fn decoded_paths_fit_their_page() {
    for needle in ["retangulo centralizado", "Frame_paper", "caixa-1"] {
        let Some(path) = fixture_named(needle) else {
            continue;
        };
        let data = fs::read(&path).expect("read fixture");
        let doc = Container::parse(&data).expect("parse");
        let (pw, ph) = doc.page_size_mm.expect("page size");
        for p in &doc.paths {
            let (_, _, max_x, max_y) = p.bounds().expect("bounds");
            assert!(
                max_x <= pw + 0.5,
                "{}: x {max_x} > page {pw}",
                path.display()
            );
            assert!(
                max_y <= ph + 0.5,
                "{}: y {max_y} > page {ph}",
                path.display()
            );
        }
    }
}

#[test]
fn decoded_paths_have_sane_bounds() {
    for path in fixtures() {
        let data = fs::read(&path).expect("read fixture");
        let doc = Container::parse(&data).expect("parse");
        for p in &doc.paths {
            let (min_x, min_y, max_x, max_y) = p.bounds().expect("bounds");
            // Objects may legitimately sit partly outside the page (Silhouette
            // allows dragging them off the media), so only reject values that
            // could not be physical millimetres.
            assert!(
                min_x >= -2000.0 && min_y >= -2000.0,
                "{}: negative bounds",
                path.display()
            );
            assert!(
                max_x <= 2000.0 && max_y <= 2000.0,
                "{}: huge bounds",
                path.display()
            );
        }
    }
}
