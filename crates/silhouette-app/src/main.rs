//! `silhouette` CLI.
//!
//! ```text
//! silhouette inspect <file.studio3>   Parse a design (no hardware)
//! silhouette svg <file.studio3>       Export the decoded paths as SVG
//! silhouette regmarks                 Write printable registration marks
//! silhouette print-sheet <file>       PDF with marks + artwork/paths (no cutter)
//! silhouette golden                   Print the verified GPGL job as hex
//! silhouette probe                    Query the cutter (read-only, no motion)
//! silhouette cut --yes                Send the verified job to the cutter
//! silhouette cut-file <file> --yes    Cut the paths decoded from a design
//! ```

use std::process::{Command, ExitCode};

use silhouette_app::pdf;
use silhouette_core::{
    raster_placement, regmarks_svg, Container, ImageKind, RegMarkSpec, RegMarkStyle,
};
use silhouette_driver::{
    golden_square_cut, order_for_cut, polyline_area, polyline_epilogue, polyline_job,
    polyline_paths, polyline_prelude, CutMapping, CutSettings, Job, RegMarkScan, Session, Status,
    UsbTransport,
};

const USAGE: &str = "\
silhouette — Silhouette Studio tooling

USAGE:
    silhouette inspect <file.studio3>
    silhouette svg <file.studio3> [out.svg] [--page WxH]
    silhouette regmarks [out.svg] [--page WxH] [options]
    silhouette print-sheet <file.studio3> [out.pdf] [options]
    silhouette golden
    silhouette probe
    silhouette cut --yes
    silhouette cut-file <file.studio3> [--dry-run] [--yes] [options]

COMMANDS:
    inspect      Parse a .studio3 container and print its verified sections
    svg          Write the decoded paths as an SVG (millimetres)
    regmarks     Write printable registration marks (square + Ls) as SVG
    print-sheet  Write a PDF with the marks and the design; no cutter is moved
    golden       Print the hardware-verified 20 mm square job as hex
    probe        Query firmware, status and tool codes (read-only, no motion)
    cut          Send that job to a connected Cameo 5 (moves the carriage!)
    cut-file     Cut the line/rectangle paths decoded from a .studio3 design

OPTIONS (regmarks):
    --page WxH           Page size in mm        (default 210x297)
    --inset-mm N         Edge -> mark corner    (default 10)
    --inset-mm N             Edge -> mark corner    (default 15.875)
    --arm-mm N             L arm length           (default 20)
    --square-mm N          Square side            (default 10)
    --stroke-mm N          Mark stroke width      (default 0.5)
    --search-offset-top-mm N  Search starts N mm before top mark (default 10)
    --search-offset-left-mm N Search starts N mm before left mark (default 10)
    --step-size-mm N       Step size for segments (default 0 = off)
    --step-direction N     Step direction: 0=vertical, 1=horizontal (default 0)
    --four-corner          Add the fourth L       (default: square + three Ls)

OPTIONS (print-sheet):
    --page WxH               Page size in mm        (default: from the document)
    --inset-mm N             Edge -> mark corner    (default 15.875)
    --arm-mm N               L arm length           (default 20)
    --square-mm N            Square side            (default 10)
    --stroke-mm N            Mark stroke width      (default 0.5)
    --search-offset-top-mm N Search starts N mm before top mark (default 10)
    --search-offset-left-mm N Search starts N mm before left mark (default 10)
    --step-size-mm N         Step size for segments (default 0 = off)
    --step-direction N       Step direction: 0=vertical, 1=horizontal (default 0)
    --four-corner            Add the fourth L       (default: square + three Ls)
    --marks-only             Do not draw artwork or cut paths
    --no-art                 Do not draw the embedded raster artwork
    --paths                  Draw the decoded cut paths as thin lines
    --no-paths               Never draw cut paths
    --art-inset-mm N         Margin used when no stored placement is found (default 20)
    --print                  Send the PDF to CUPS with `lp` after writing it
    --printer NAME           CUPS queue for --print (default: system default)
    --quality Q              cupsPrintQuality: Normal or High (default Normal)
    --color C                ColorModel: RGB, Gray, AutoGray or ProcessGray
    --paper P                MediaType: Stationery, Photographic, PhotographicMatte,
                             PhotographicGlossy, PhotographicSemiGloss,
                             PhotographicHighGloss, StationeryCoated or Envelope

OPTIONS (cut-file):
    --dry-run        Print the GPGL commands, touch no hardware
    --yes            Required for a real cut (moves the carriage)
    --page H         Page height in mm (default: from the document)
    --flip-y         Mirror y (machine y-up); default is y-down per reference
    --x-offset-mm N  Empirical x calibration offset (machine frame)
    --y-offset-mm N  Empirical y calibration offset (+ moves the job down)
    --regmark        Scan printed registration marks before cutting
    --regmark-inset-mm N     Edge -> mark corner used by the scan (default 15.875)
    --regmark-arm-mm N       L arm length used by the scan      (default 20)
    --regmark-stroke-mm N    Mark stroke width used by the scan (default 0.5)
    --regmark-search-offset-top-mm N    Search starts N mm before top mark (default 10)
    --regmark-search-offset-left-mm N   Search starts N mm before left mark (default 10)
    --regmark-step-size-mm N     Step size for segments (default 0 = off)
    --regmark-step-direction N   Step direction: 0=vertical, 1=horizontal (default 0)
    --speed N        Speed 1..10        (default 3)
    --force N        Force 1..10        (default 4)
    --depth N        Blade depth 1..10  (default 1)
    --tool N         Tool holder       (default 1)

The `svg` page defaults to the document size (A4 when absent); override with
`--page 210x297`. `regmarks` writes the marks as SVG; `print-sheet` writes a
PDF with the marks plus artwork/vector paths and can send it to CUPS. The
machine reads the marks optically (`TB123`) and then cuts relative to the
top-left mark.

WARNING: the registration scan is UNVERIFIED on the Cameo 5. The mark sheet
must be printed (not cut) and its geometry must match the scan parameters.
Do not send print jobs to the `CAMEO-5` CUPS queue: that URI is the cutter.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("inspect") => match args.get(1) {
            Some(path) => inspect(path),
            None => usage(),
        },
        Some("svg") => match args.get(1) {
            Some(path) => svg(path, &args[2..]),
            None => usage(),
        },
        Some("regmarks") => regmarks(&args[1..]),
        Some("print-sheet") => print_sheet(&args[1..]),
        Some("golden") => golden(),
        Some("probe") => probe(),
        Some("cut") => cut(&args[1..]),
        Some("cut-file") => cut_file(&args[1..]),
        Some("-h" | "--help" | "help") | None => usage(),
        Some(other) => {
            eprintln!("unknown command: {other}\n");
            usage()
        }
    }
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(1)
}

fn inspect(path: &str) -> ExitCode {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("cannot read {path}: {e}");
            return ExitCode::from(2);
        }
    };

    let doc = match Container::parse(&data) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{path}: {e}");
            return ExitCode::from(2);
        }
    };

    println!("file            {path}");
    println!("bytes           {}", data.len());
    println!("nested document offset {}", doc.nested_doc_offset);
    match doc.page_size_mm {
        Some((w, h)) => println!("page size       {w} x {h} mm"),
        None => println!("page size       unknown (fields are zero)"),
    }
    println!("header          {:?}", doc.header);
    println!("images          {}", doc.images.len());
    for (i, img) in doc.images.iter().enumerate() {
        println!(
            "  #{i} {:?} {}x{} at {}..{} ({} bytes)",
            img.kind,
            img.width,
            img.height,
            img.offset,
            img.end,
            img.data.len()
        );
    }

    println!("strings         {}", doc.strings.len());
    for entry in doc.strings.iter().take(20) {
        println!("  {:>8}  {:?}", entry.offset, entry.value);
    }
    if doc.strings.len() > 20 {
        println!("  ... {} more", doc.strings.len() - 20);
    }

    let guids = doc.guid_strings();
    println!("guid strings    {}", guids.len());
    println!("root_uuid       {:?}", doc.root_uuid);
    println!("trailing chunks {}", doc.trailing.len());

    println!("paths           {}", doc.paths.len());
    let points: usize = doc.paths.iter().map(|p| p.points.len()).sum();
    println!("  vertices      {points}");
    if let Some((min_x, min_y, max_x, max_y)) = overall_bounds(&doc.paths) {
        println!("  bounds        x {min_x:.3}..{max_x:.3}  y {min_y:.3}..{max_y:.3} mm");
    }
    for path in &doc.paths {
        let kind = match path.kind {
            silhouette_core::PathKind::Line => "line",
            silhouette_core::PathKind::Rectangle => "rectangle",
            silhouette_core::PathKind::Curve => "curve",
            silhouette_core::PathKind::Other => "other (?)",
        };
        let (min_x, min_y, max_x, max_y) = path.bounds().unwrap_or((0.0, 0.0, 0.0, 0.0));
        println!(
            "  {kind:<12} {:>3} pts  {:.3} x {:.3} mm at ({min_x:.3}, {min_y:.3}){}",
            path.points.len(),
            max_x - min_x,
            max_y - min_y,
            if path.closed { "  closed" } else { "" }
        );
    }

    println!();
    println!("note: curved (`create_curve`) and raster-only designs are not decoded;");
    println!("      see docs/FORMAT.md for the verified encoding and its limits.");
    ExitCode::SUCCESS
}

fn overall_bounds(paths: &[silhouette_core::Path]) -> Option<(f32, f32, f32, f32)> {
    let mut it = paths.iter().filter_map(|p| p.bounds());
    let first = it.next()?;
    Some(it.fold(first, |acc, b| {
        (
            acc.0.min(b.0),
            acc.1.min(b.1),
            acc.2.max(b.2),
            acc.3.max(b.3),
        )
    }))
}

fn svg(path: &str, args: &[String]) -> ExitCode {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("cannot read {path}: {e}");
            return ExitCode::from(2);
        }
    };

    let doc = match Container::parse(&data) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{path}: {e}");
            return ExitCode::from(2);
        }
    };

    let mut out: Option<&str> = None;
    let mut page = doc.page_size_mm.unwrap_or((210.0, 297.0));
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--page" => {
                let Some(spec) = args.get(i + 1) else {
                    eprintln!("--page needs a value like 210x297");
                    return ExitCode::from(1);
                };
                match parse_page(spec) {
                    Some(p) => page = p,
                    None => {
                        eprintln!("invalid --page {spec:?}; expected WxH in millimetres");
                        return ExitCode::from(1);
                    }
                }
                i += 2;
            }
            other => {
                out = Some(other);
                i += 1;
            }
        }
    }

    let out = match out {
        Some(p) => p.to_string(),
        None => std::path::Path::new(path)
            .with_extension("svg")
            .to_string_lossy()
            .into_owned(),
    };

    let svg = silhouette_core::to_svg(&doc.paths, page.0, page.1);
    if let Err(e) = std::fs::write(&out, &svg) {
        eprintln!("cannot write {out}: {e}");
        return ExitCode::from(2);
    }

    println!(
        "{} paths, {} vertices -> {out} ({} x {} mm)",
        doc.paths.len(),
        doc.paths.iter().map(|p| p.points.len()).sum::<usize>(),
        page.0,
        page.1
    );
    ExitCode::SUCCESS
}

fn parse_page(spec: &str) -> Option<(f32, f32)> {
    let (w, h) = spec.split_once(['x', 'X'])?;
    let w: f32 = w.trim().parse().ok()?;
    let h: f32 = h.trim().parse().ok()?;
    (w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0).then_some((w, h))
}

fn regmarks(args: &[String]) -> ExitCode {
    let mut out: Option<&str> = None;
    let mut spec = RegMarkSpec::a4();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--page" => {
                let Some(value) = args.get(i + 1) else {
                    eprintln!("--page needs a value like 210x297");
                    return ExitCode::from(1);
                };
                match parse_page(value) {
                    Some((w, h)) => {
                        spec.page_width_mm = w;
                        spec.page_height_mm = h;
                    }
                    None => {
                        eprintln!("invalid --page {value:?}; expected WxH in millimetres");
                        return ExitCode::from(1);
                    }
                }
                i += 2;
            }
            "--inset-mm"
            | "--arm-mm"
            | "--square-mm"
            | "--stroke-mm"
            | "--search-offset-top-mm"
            | "--search-offset-left-mm"
            | "--step-size-mm"
            | "--step-direction" => {
                let Some(value) = args.get(i + 1).and_then(|v| v.parse::<f32>().ok()) else {
                    eprintln!("{} needs a positive number in millimetres", args[i]);
                    return ExitCode::from(1);
                };
                if !(value.is_finite() && value >= 0.0) {
                    eprintln!("{} must be non-negative: {value}", args[i]);
                    return ExitCode::from(1);
                }
                match args[i].as_str() {
                    "--inset-mm" => spec.inset_mm = value,
                    "--arm-mm" => spec.arm_mm = value,
                    "--square-mm" => spec.square_mm = value,
                    "--stroke-mm" => spec.stroke_mm = value,
                    "--search-offset-top-mm" => spec.search_offset_top_mm = value,
                    "--search-offset-left-mm" => spec.search_offset_left_mm = value,
                    "--step-size-mm" => spec.step_size_mm = value,
                    _ => spec.step_direction = value.round() as i32,
                }
                i += 2;
            }
            "--four-corner" => {
                spec.style = RegMarkStyle::FourCorner;
                i += 1;
            }
            other if !other.starts_with('-') && out.is_none() => {
                out = Some(other);
                i += 1;
            }
            other => {
                eprintln!("unknown option {other}");
                return ExitCode::from(1);
            }
        }
    }

    if spec.inset_mm >= spec.page_width_mm.min(spec.page_height_mm) / 2.0 {
        eprintln!(
            "inset {} mm leaves no mark area on a {} x {} mm page",
            spec.inset_mm, spec.page_width_mm, spec.page_height_mm
        );
        return ExitCode::from(1);
    }

    let svg = regmarks_svg(&spec);
    let path = out.unwrap_or("regmarks.svg");
    if path == "-" {
        print!("{svg}");
    } else if let Err(e) = std::fs::write(path, &svg) {
        eprintln!("cannot write {path}: {e}");
        return ExitCode::from(2);
    }

    let arm_su = (spec.arm_mm * 20.0).round() as i32;
    let stroke_su = (spec.stroke_mm * 20.0).round() as i32;
    let width_su = (spec.mark_width_mm() * 20.0).round() as i32;
    let height_su = (spec.mark_height_mm() * 20.0).round() as i32;
    let top_su = (spec.scan_margin_top_mm() * 20.0).round() as i32;
    let left_su = (spec.scan_margin_left_mm() * 20.0).round() as i32;

    eprintln!(
        "page {:.1} x {:.1} mm, inset {:.2}, square {:.2}, arm {:.2}, stroke {:.2}",
        spec.page_width_mm,
        spec.page_height_mm,
        spec.inset_mm,
        spec.square_mm,
        spec.arm_mm,
        spec.stroke_mm
    );
    eprintln!(
        "scan TB51,{arm_su} TB53,{stroke_su} TB123,{height_su},{width_su},{top_su},{left_su}"
    );
    eprintln!(
        "cut-file --regmark --regmark-inset-mm {:.2} --regmark-arm-mm {:.2} \
         --regmark-stroke-mm {:.2}",
        spec.inset_mm, spec.arm_mm, spec.stroke_mm
    );
    if path != "-" {
        println!("wrote {path}");
    }
    ExitCode::SUCCESS
}

fn print_sheet(args: &[String]) -> ExitCode {
    let mut input: Option<&str> = None;
    let mut out: Option<&str> = None;
    let mut spec = RegMarkSpec::for_page(210.0, 297.0);
    let mut page_override: Option<(f32, f32)> = None;
    let mut marks_only = false;
    let mut no_art = false;
    let mut force_paths = false;
    let mut no_paths = false;
    let art_inset_mm = 20.0f32;
    let mut send_to_printer = false;
    let mut printer: Option<String> = None;
    let mut quality = String::from("Normal");
    let mut color = String::from("RGB");
    let mut paper = String::from("Stationery");

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--page" => {
                let Some(value) = args.get(i + 1) else {
                    eprintln!("--page needs a value like 210x297");
                    return ExitCode::from(1);
                };
                match parse_page(value) {
                    Some(p) => page_override = Some(p),
                    None => {
                        eprintln!("invalid --page {value:?}; expected WxH in millimetres");
                        return ExitCode::from(1);
                    }
                }
                i += 2;
            }
            "--inset-mm"
            | "--arm-mm"
            | "--square-mm"
            | "--stroke-mm"
            | "--art-inset-mm"
            | "--search-offset-top-mm"
            | "--search-offset-left-mm"
            | "--step-size-mm"
            | "--step-direction" => {
                let Some(value) = args.get(i + 1).and_then(|v| v.parse::<f32>().ok()) else {
                    eprintln!("{} needs a positive number in millimetres", args[i]);
                    return ExitCode::from(1);
                };
                if !(value.is_finite() && value >= 0.0) {
                    eprintln!("{} must be non-negative: {value}", args[i]);
                    return ExitCode::from(1);
                }
                match args[i].as_str() {
                    "--inset-mm" => spec.inset_mm = value,
                    "--arm-mm" => spec.arm_mm = value,
                    "--square-mm" => spec.square_mm = value,
                    "--stroke-mm" => spec.stroke_mm = value,
                    "--search-offset-top-mm" => spec.search_offset_top_mm = value,
                    "--search-offset-left-mm" => spec.search_offset_left_mm = value,
                    "--step-size-mm" => spec.step_size_mm = value,
                    _ => spec.step_direction = value.round() as i32,
                }
                i += 2;
            }
            "--four-corner" => {
                spec.style = RegMarkStyle::FourCorner;
                i += 1;
            }
            "--marks-only" => {
                marks_only = true;
                i += 1;
            }
            "--no-art" => {
                no_art = true;
                i += 1;
            }
            "--paths" => {
                force_paths = true;
                i += 1;
            }
            "--no-paths" => {
                no_paths = true;
                i += 1;
            }
            "--print" => {
                send_to_printer = true;
                i += 1;
            }
            "--printer" => {
                let Some(value) = args.get(i + 1) else {
                    eprintln!("--printer needs a CUPS queue name");
                    return ExitCode::from(1);
                };
                printer = Some(value.clone());
                i += 2;
            }
            "--quality" => {
                let Some(value) = args.get(i + 1) else {
                    eprintln!("--quality needs Normal or High");
                    return ExitCode::from(1);
                };
                if value != "Normal" && value != "High" {
                    eprintln!("--quality must be Normal or High, got {value:?}");
                    return ExitCode::from(1);
                }
                quality = value.clone();
                i += 2;
            }
            "--color" => {
                let Some(value) = args.get(i + 1) else {
                    eprintln!("--color needs RGB, Gray, AutoGray or ProcessGray");
                    return ExitCode::from(1);
                };
                if !matches!(value.as_str(), "RGB" | "Gray" | "AutoGray" | "ProcessGray") {
                    eprintln!("--color must be RGB, Gray, AutoGray or ProcessGray, got {value:?}");
                    return ExitCode::from(1);
                }
                color = value.clone();
                i += 2;
            }
            "--paper" | "--media-type" => {
                let Some(value) = args.get(i + 1) else {
                    eprintln!("--paper needs an Epson MediaType value");
                    return ExitCode::from(1);
                };
                if !matches!(
                    value.as_str(),
                    "Stationery"
                        | "Photographic"
                        | "PhotographicMatte"
                        | "PhotographicGlossy"
                        | "PhotographicSemiGloss"
                        | "PhotographicHighGloss"
                        | "StationeryCoated"
                        | "Envelope"
                ) {
                    eprintln!("unsupported Epson MediaType {value:?}");
                    return ExitCode::from(1);
                }
                paper = value.clone();
                i += 2;
            }
            other if !other.starts_with('-') => {
                if input.is_none() {
                    input = Some(other);
                } else if out.is_none() {
                    out = Some(other);
                } else {
                    eprintln!("unexpected argument {other}");
                    return ExitCode::from(1);
                }
                i += 1;
            }
            other => {
                eprintln!("unknown option {other}");
                return ExitCode::from(1);
            }
        }
    }

    let Some(input) = input else {
        eprintln!("print-sheet needs a .studio3 file");
        return ExitCode::from(1);
    };

    let data = match std::fs::read(input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("cannot read {input}: {e}");
            return ExitCode::from(2);
        }
    };
    let doc = match Container::parse(&data) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{input}: {e}");
            return ExitCode::from(2);
        }
    };

    let (pw, ph) = page_override.or(doc.page_size_mm).unwrap_or((210.0, 297.0));
    spec.page_width_mm = pw;
    spec.page_height_mm = ph;

    if spec.inset_mm >= pw.min(ph) / 2.0 {
        eprintln!(
            "inset {} mm leaves no mark area on a {pw} x {ph} mm page",
            spec.inset_mm
        );
        return ExitCode::from(1);
    }
    if art_inset_mm >= pw.min(ph) / 2.0 {
        eprintln!("--art-inset-mm {art_inset_mm} leaves no artwork area on the page");
        return ExitCode::from(1);
    }

    let mut selected: Vec<&silhouette_core::Image> = Vec::new();
    if !marks_only && !no_art {
        for img in &doc.images {
            if img.kind == ImageKind::Png && img.width.max(img.height) > 600 {
                selected.push(img);
            }
        }
    }

    let mut rects: Vec<(f32, f32, f32, f32)> = Vec::new();
    let mut placement_note = String::from("no raster artwork selected");
    if let Some(largest) = selected
        .iter()
        .max_by_key(|i| u64::from(i.width) * u64::from(i.height))
    {
        let detected = raster_placement(&data, largest.width, largest.height, pw, ph);
        let base = detected
            .filter(|p| {
                let w = p.width_mm(largest.width);
                let h = p.height_mm(largest.height);
                p.x_mm > -5.0 && p.y_mm > -5.0 && p.x_mm + w < pw + 5.0 && p.y_mm + h < ph + 5.0
            })
            .map(|p| {
                placement_note = format!(
                    "stored transform: {:.6} mm/px at ({:.2}, {:.2}) mm",
                    p.scale, p.x_mm, p.y_mm
                );
                (
                    p.x_mm,
                    p.y_mm,
                    p.width_mm(largest.width),
                    p.height_mm(largest.height),
                )
            })
            .unwrap_or_else(|| {
                let avail_w = (pw - 2.0 * art_inset_mm).max(10.0);
                let avail_h = (ph - 2.0 * art_inset_mm).max(10.0);
                let s = (avail_w / largest.width as f32).min(avail_h / largest.height as f32);
                let w = largest.width as f32 * s;
                let h = largest.height as f32 * s;
                placement_note = format!(
                    "fit placement: {:.1} mm margin (no stored transform found)",
                    art_inset_mm
                );
                ((pw - w) / 2.0, (ph - h) / 2.0, w, h)
            });

        for img in &selected {
            let s = (base.2 / img.width as f32).min(base.3 / img.height as f32);
            let w = img.width as f32 * s;
            let h = img.height as f32 * s;
            rects.push((
                base.0 + (base.2 - w) / 2.0,
                base.1 + (base.3 - h) / 2.0,
                w,
                h,
            ));
        }
    }

    let sheet_images: Vec<pdf::SheetImage<'_>> = selected
        .iter()
        .zip(&rects)
        .map(|(img, (x, y, w, h))| pdf::SheetImage {
            label: format!("#{} {}x{}", img.offset, img.width, img.height),
            png: img.data,
            x_mm: *x,
            y_mm: *y,
            width_mm: *w,
            height_mm: *h,
        })
        .collect();

    let draw_paths = !marks_only
        && !no_paths
        && (force_paths || (sheet_images.is_empty() && !doc.paths.is_empty()));
    let empty_paths: &[silhouette_core::Path] = &[];
    let sheet = pdf::Sheet {
        page_width_mm: pw,
        page_height_mm: ph,
        marks: spec,
        draw_marks: true,
        paths: if draw_paths { &doc.paths } else { empty_paths },
        images: &sheet_images,
    };

    let pdf = match pdf::render(&sheet) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("cannot render the sheet: {e}");
            return ExitCode::from(2);
        }
    };

    let out = out.map(str::to_owned).unwrap_or_else(|| {
        std::path::Path::new(input)
            .with_extension("print.pdf")
            .to_string_lossy()
            .into_owned()
    });
    if let Err(e) = std::fs::write(&out, &pdf) {
        eprintln!("cannot write {out}: {e}");
        return ExitCode::from(2);
    }

    let arm_su = (spec.arm_mm * 20.0).round() as i32;
    let stroke_su = (spec.stroke_mm * 20.0).round() as i32;
    let width_su = (spec.mark_width_mm() * 20.0).round() as i32;
    let height_su = (spec.mark_height_mm() * 20.0).round() as i32;
    let top_su = (spec.scan_margin_top_mm() * 20.0).round() as i32;
    let left_su = (spec.scan_margin_left_mm() * 20.0).round() as i32;

    println!("page            {pw:.1} x {ph:.1} mm");
    println!(
        "marks           inset {:.2}, square {:.2}, arm {:.2}, stroke {:.2}",
        spec.inset_mm, spec.square_mm, spec.arm_mm, spec.stroke_mm
    );
    if sheet_images.is_empty() {
        println!("artwork         none ({placement_note})");
    } else {
        println!(
            "artwork         {} layer(s), {placement_note}",
            sheet_images.len()
        );
        for img in &sheet_images {
            println!(
                "  {} at ({:.2}, {:.2}) mm, {:.2} x {:.2} mm",
                img.label, img.x_mm, img.y_mm, img.width_mm, img.height_mm
            );
        }
    }
    println!(
        "paths           {} decoded, {}",
        doc.paths.len(),
        if draw_paths {
            "drawn thin"
        } else {
            "not drawn"
        }
    );
    println!("wrote           {out} ({} bytes)", pdf.len());
    eprintln!(
        "scan            TB51,{arm_su} TB53,{stroke_su} \
         TB123,{height_su},{width_su},{top_su},{left_su}"
    );
    eprintln!(
        "cut-file        --regmark --regmark-inset-mm {:.2} \
         --regmark-arm-mm {:.2} --regmark-stroke-mm {:.2}",
        spec.inset_mm, spec.arm_mm, spec.stroke_mm
    );
    eprintln!(
        "UNVERIFIED      print at 100% (no \"fit to page\") and scan this sheet \
         before cutting; the raster placement is derived from the file, not yet \
         captured from paper."
    );

    if send_to_printer {
        if let Some(queue) = &printer {
            let lower = queue.to_ascii_lowercase();
            if lower.contains("cameo") || lower.contains("silhouette") {
                eprintln!(
                    "refusing to print to {queue}: that CUPS queue points at the cutter \
                     (usb://Silhouette/...), not at a printer"
                );
                return ExitCode::from(1);
            }
        }
        let mut cmd = Command::new("lp");
        if let Some(queue) = &printer {
            cmd.args(["-d", queue]);
        }
        if (pw - 210.0).abs() < 0.5 && (ph - 297.0).abs() < 0.5 {
            cmd.args(["-o", "media=A4"]);
        } else {
            eprintln!(
                "note            custom page {pw:.1} x {ph:.1} mm; configure a matching \
                 CUPS media if the queue rejects it"
            );
        }
        cmd.args(["-o", "print-scaling=none", "-o", "fit-to-page=false"]);
        cmd.arg("-o")
            .arg(format!("cupsPrintQuality={quality}"))
            .arg("-o")
            .arg(format!("ColorModel={color}"))
            .arg("-o")
            .arg(format!("MediaType={paper}"))
            .arg(&out);
        match cmd.output() {
            Ok(o) if o.status.success() => {
                print!("{}", String::from_utf8_lossy(&o.stdout));
            }
            Ok(o) => {
                eprintln!("lp failed ({}):", o.status);
                eprint!("{}", String::from_utf8_lossy(&o.stderr));
                return ExitCode::from(6);
            }
            Err(e) => {
                eprintln!("cannot run lp: {e}");
                return ExitCode::from(6);
            }
        }
    }

    ExitCode::SUCCESS
}

fn golden() -> ExitCode {
    let job = golden_square_cut();
    println!("# verified job — 20 mm square, depth 1, force 4, speed 3");
    println!(
        "# {} commands, {} bytes, one USB write per line",
        job.len(),
        job.total_bytes()
    );
    print!("{}", job.hex_dump());
    ExitCode::SUCCESS
}

fn probe() -> ExitCode {
    let transport = match UsbTransport::open_cameo5() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("cannot open 0b4d:1140: {e}");
            return ExitCode::from(4);
        }
    };
    let mut session = Session::new(transport.with_write_timeout_ms(10_000));

    if let Err(e) = session.initialize() {
        eprintln!("init failed: {e}");
        return ExitCode::from(4);
    }

    let fw = session.firmware();
    let status = session.status();
    let tools = session.tools();

    match fw {
        Ok(v) => println!("firmware   {v}"),
        Err(e) => println!("firmware   unavailable ({e})"),
    }
    match status {
        Ok(v) => println!("status     {}", status_name(v)),
        Err(e) => println!("status     unavailable ({e})"),
    }
    match tools {
        Ok((a, b)) => println!("tools      holder1={a} holder2={b}"),
        Err(e) => println!("tools      unavailable ({e})"),
    }

    println!("usb        ok (read-only; nothing was moved)");
    ExitCode::SUCCESS
}

fn cut(args: &[String]) -> ExitCode {
    if !args.iter().any(|a| a == "--yes") {
        eprintln!(
            "refusing to move the cutter without --yes.\n\
             This sends the hardware-verified job: it lowers the blade (depth 1)\n\
             and cuts a 20 mm square at force 4.\n\
             Run `silhouette golden` to inspect the exact bytes first."
        );
        return ExitCode::from(3);
    }

    let mut transport = match UsbTransport::open_cameo5() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("cannot open 0b4d:1140: {e}");
            return ExitCode::from(4);
        }
    };
    transport = transport.with_write_timeout_ms(30_000);

    let mut session = Session::new(transport);

    if let Err(e) = session.initialize() {
        eprintln!("init failed: {e}");
        return ExitCode::from(4);
    }

    match session.firmware() {
        Ok(fw) => println!("firmware        {fw}"),
        Err(e) => println!("firmware        unavailable ({e})"),
    }
    match session.status() {
        Ok(s) => println!("status          {:?}", status_name(s)),
        Err(e) => println!("status          unavailable ({e})"),
    }

    let job = golden_square_cut();
    println!("sending         {} commands", job.len());
    if let Err(e) = session.run(&job) {
        eprintln!("job failed: {e}");
        return ExitCode::from(4);
    }

    println!("done            square sent; blade lifted and tool deselected");
    ExitCode::SUCCESS
}

#[allow(unused_variables, unused_assignments)]
fn cut_file(args: &[String]) -> ExitCode {
    let mut file: Option<&str> = None;
    let mut dry_run = false;
    let mut yes = false;
    let mut page_h: Option<f32> = None;
    let mut flip_y = false;
    let mut offset_x_mm = 0.0f32;
    let mut offset_y_mm = 0.0f32;
    let mut regmark = false;
    let mut regmark_inset_mm = 15.875f32;
    let mut regmark_arm_mm = 20.0f32;
    let mut regmark_stroke_mm = 0.5f32;
    let mut regmark_search_offset_top_mm = 10.0f32;
    let mut regmark_search_offset_left_mm = 10.0f32;
    let mut regmark_step_size_mm = 0.0f32;
    let mut regmark_step_direction = 0i32;
    let mut settings = CutSettings::default();

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        let need_value = |i: usize| -> Option<&str> { args.get(i + 1).map(String::as_str) };
        match arg {
            "--dry-run" => {
                dry_run = true;
                i += 1;
            }
            "--yes" => {
                yes = true;
                i += 1;
            }
            "--flip-y" => {
                flip_y = true;
                i += 1;
            }
            "--page" => {
                let Some(v) = need_value(i).and_then(|v| v.parse::<f32>().ok()) else {
                    eprintln!("--page needs a height in millimetres");
                    return ExitCode::from(1);
                };
                if !(10.0..=2000.0).contains(&v) {
                    eprintln!("--page height out of range: {v}");
                    return ExitCode::from(1);
                }
                page_h = Some(v);
                i += 2;
            }
            "--x-offset-mm" | "--y-offset-mm" => {
                let Some(v) = need_value(i).and_then(|v| v.parse::<f32>().ok()) else {
                    eprintln!("{arg} needs a value in millimetres");
                    return ExitCode::from(1);
                };
                if !(-100.0..=100.0).contains(&v) {
                    eprintln!("{arg} out of range (-100..=100 mm): {v}");
                    return ExitCode::from(1);
                }
                if arg == "--x-offset-mm" {
                    offset_x_mm = v;
                } else {
                    offset_y_mm = v;
                }
                i += 2;
            }
            "--regmark" => {
                regmark = true;
                i += 1;
            }
            "--regmark-inset-mm"
            | "--regmark-arm-mm"
            | "--regmark-stroke-mm"
            | "--regmark-search-offset-top-mm"
            | "--regmark-search-offset-left-mm"
            | "--regmark-step-size-mm"
            | "--regmark-step-direction" => {
                let Some(v) = need_value(i).and_then(|v| v.parse::<f32>().ok()) else {
                    eprintln!("{arg} needs a value in millimetres");
                    return ExitCode::from(1);
                };
                if !(v.is_finite() && v >= 0.0) {
                    eprintln!("{arg} must be a non-negative length: {v}");
                    return ExitCode::from(1);
                }
                match arg {
                    "--regmark-inset-mm" => regmark_inset_mm = v,
                    "--regmark-arm-mm" => regmark_arm_mm = v,
                    "--regmark-stroke-mm" => regmark_stroke_mm = v,
                    "--regmark-search-offset-top-mm" => regmark_search_offset_top_mm = v,
                    "--regmark-search-offset-left-mm" => regmark_search_offset_left_mm = v,
                    "--regmark-step-size-mm" => regmark_step_size_mm = v,
                    _ => regmark_step_direction = v.round() as i32,
                }
                i += 2;
            }
            "--speed" | "--force" | "--depth" | "--tool" => {
                let Some(v) = need_value(i).and_then(|v| v.parse::<u8>().ok()) else {
                    eprintln!("{arg} needs a number");
                    return ExitCode::from(1);
                };
                match arg {
                    "--speed" => settings.speed = v,
                    "--force" => settings.force = v,
                    "--depth" => settings.depth = v,
                    _ => settings.tool = v,
                }
                i += 2;
            }
            other if other.starts_with('-') => {
                eprintln!("unknown option {other}");
                return ExitCode::from(1);
            }
            other => {
                file = Some(other);
                i += 1;
            }
        }
    }

    let Some(file) = file else {
        return usage();
    };

    let data = match std::fs::read(file) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("cannot read {file}: {e}");
            return ExitCode::from(2);
        }
    };
    let doc = match Container::parse(&data) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{file}: {e}");
            return ExitCode::from(2);
        }
    };

    let (page_w, default_h) = doc.page_size_mm.unwrap_or((210.0, 297.0));
    let page_h = page_h.unwrap_or(default_h);

    let mut polylines: Vec<(Vec<(f32, f32)>, bool)> = Vec::new();
    let mut skipped = 0usize;
    for p in &doc.paths {
        match p.kind {
            silhouette_core::PathKind::Line
            | silhouette_core::PathKind::Rectangle
            | silhouette_core::PathKind::Curve => {
                polylines.push((
                    p.points.iter().map(|q| (q.x_mm, q.y_mm)).collect(),
                    p.closed,
                ));
            }
            _ => skipped += 1,
        }
    }
    let polylines = order_for_cut(polylines);

    println!("file            {file}");
    println!("page            {page_w} x {page_h} mm");
    println!("cuttable paths  {}", polylines.len());
    if skipped > 0 {
        println!("skipped         {skipped} path(s) of unknown type");
    }
    println!(
        "settings        tool {} speed {} force {} depth {}",
        settings.tool, settings.speed, settings.force, settings.depth
    );

    if polylines.is_empty() {
        eprintln!("\nno cuttable paths: this design is raster-only.");
        eprintln!("Trace it in Silhouette Studio first.");
        return ExitCode::from(2);
    }

    let out_of_page = polylines
        .iter()
        .flat_map(|(p, _)| p.iter())
        .any(|(x, y)| *x < 0.0 || *y < 0.0 || *x > page_w + 0.5 || *y > page_h + 0.5);
    if out_of_page {
        println!("warning         some vertices lie outside the page rectangle");
    }

    if regmark && regmark_inset_mm >= page_w.min(page_h) / 2.0 {
        eprintln!(
            "--regmark-inset-mm {regmark_inset_mm} leaves no mark area on a \
             {page_w} x {page_h} mm page"
        );
        return ExitCode::from(1);
    }

    let mapping = if flip_y {
        CutMapping::flipped(page_h)
    } else {
        CutMapping::new(page_h)
    };
    let mapping = if regmark {
        mapping.with_origin(regmark_inset_mm, regmark_inset_mm)
    } else {
        mapping
    }
    .with_offset(offset_x_mm, offset_y_mm);

    let scan = regmark.then(|| RegMarkScan {
        arm_su: (regmark_arm_mm * 20.0).round() as i32,
        stroke_su: (regmark_stroke_mm * 20.0).round() as i32,
        height_su: ((page_h - 2.0 * regmark_inset_mm) * 20.0).round() as i32,
        width_su: ((page_w - 2.0 * regmark_inset_mm) * 20.0).round() as i32,
        top_su: ((regmark_inset_mm - regmark_search_offset_top_mm).max(0.0) * 20.0).round() as i32,
        left_su: ((regmark_inset_mm - regmark_search_offset_left_mm).max(0.0) * 20.0).round()
            as i32,
        quad: false,
    });

    let prelude = polyline_prelude(settings);
    let mut cut = polyline_paths(&polylines, mapping);
    for cmd in polyline_epilogue().commands() {
        cut.push(cmd.clone());
    }
    let job = polyline_job(&polylines, mapping, settings);

    let (area_h, area_w) = if let Some(scan) = &scan {
        (scan.height_su, scan.width_su)
    } else {
        (
            (page_h * 20.0).round() as i32,
            (page_w * 20.0).round() as i32,
        )
    };
    let area = polyline_area(area_h, area_w, settings.tool);

    let scan_commands = scan.as_ref().map(RegMarkScan::commands).unwrap_or_default();
    let command_count = if regmark {
        prelude.len() + scan_commands.len() + area.len() + cut.len()
    } else {
        job.len()
    };
    println!("commands        {command_count}");
    println!();
    if flip_y {
        println!("mapping         y_su = ({page_h} - y_mm) * 20  (flip-y), x_su = x_mm * 20");
    } else {
        println!("mapping         y_su = y_mm * 20  (y-down, per reference), x_su = x_mm * 20");
    }
    if regmark {
        println!(
            "regmarks        inset {:.2} mm, arm {:.2} mm, stroke {:.2} mm",
            regmark_inset_mm, regmark_arm_mm, regmark_stroke_mm
        );
        println!(
            "regmarks        cut origin = ({:.2}, {:.2}) mm (top-left mark)",
            regmark_inset_mm, regmark_inset_mm
        );
        if let Some(scan) = &scan {
            println!(
                "regmarks        scan TB123,{},{},{},{}  (arm {}, stroke {})",
                scan.height_su,
                scan.width_su,
                scan.top_su,
                scan.left_su,
                scan.arm_su,
                scan.stroke_su
            );
        }
    }
    if offset_x_mm != 0.0 || offset_y_mm != 0.0 {
        println!(
            "calibration     x_su += {:.2} mm, y_su += {:.2} mm",
            offset_x_mm, offset_y_mm
        );
    }
    if regmark {
        println!("WARNING         the registration scan is UNVERIFIED on this firmware.");
        println!("WARNING         the printed marks must match the inset/arm/stroke above.");
    } else {
        println!("WARNING         page origin relative to the machine origin is UNVERIFIED.");
    }
    println!("WARNING         the y direction is not proven; try --flip-y if the cut is mirrored.");

    if dry_run {
        println!("\n# dry run — nothing was sent\n");
        if regmark {
            println!("# 1. machine + blade setup");
            print!("{}", prelude.hex_dump());
            println!("# 2. registration scan");
            let mut scan_job = Job::new();
            for cmd in &scan_commands {
                scan_job.push(cmd.clone());
            }
            print!("{}", scan_job.hex_dump());
            println!("# 3. cut area \\0,0 Z{area_h},{area_w}");
            print!("{}", area.hex_dump());
            println!("# 4. cut, relative to the top-left mark");
            print!("{}", cut.hex_dump());
        } else {
            print!("{}", job.hex_dump());
        }
        return ExitCode::SUCCESS;
    }

    if !yes {
        eprintln!(
            "\nrefusing to move the cutter without --yes.\n\
             Run with --dry-run to inspect the exact commands first."
        );
        return ExitCode::from(3);
    }

    let mut transport = match UsbTransport::open_cameo5() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("cannot open 0b4d:1140: {e}");
            return ExitCode::from(4);
        }
    };
    transport = transport.with_write_timeout_ms(30_000);
    let mut session = Session::new(transport);

    if let Err(e) = session.initialize() {
        eprintln!("init failed: {e}");
        return ExitCode::from(4);
    }
    match session.firmware() {
        Ok(fw) => println!("firmware        {fw}"),
        Err(e) => println!("firmware        unavailable ({e})"),
    }

    if let Err(e) = session.run(&prelude) {
        eprintln!("setup failed: {e}");
        return ExitCode::from(4);
    }
    let _ = session.wait_idle(30_000);
    if let Some(scan) = &scan {
        println!("scanning        registration marks (the carriage will move) ...");
        match session.scan_regmarks(scan, 45_000) {
            Ok(()) => println!("regmarks        found; origin moved to the top-left mark"),
            Err(e) => {
                eprintln!("registration scan failed: {e}");
                return ExitCode::from(5);
            }
        }
        let _ = session.wait_idle(45_000);
    }

    println!("cut area        \\0,0 Z{area_h},{area_w}");
    if let Err(e) = session.run(&area) {
        eprintln!("cut area failed: {e}");
        let _ = session.run(&polyline_epilogue());
        let _ = session.wait_idle(30_000);
        return ExitCode::from(4);
    }
    let _ = session.wait_idle(30_000);

    let path_jobs: Vec<Job> = polylines
        .iter()
        .filter(|(points, _)| points.len() >= 2)
        .map(|path| polyline_paths(std::slice::from_ref(path), mapping))
        .filter(|job| !job.is_empty())
        .collect();
    for (index, path_job) in path_jobs.iter().enumerate() {
        println!("cutting         path {}/{}", index + 1, path_jobs.len());
        if let Err(e) = session.run_paced(path_job) {
            eprintln!("path {} failed: {e}", index + 1);
            let _ = session.run(&polyline_epilogue());
            let _ = session.wait_idle(30_000);
            return ExitCode::from(4);
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
        let _ = session.wait_idle(60_000);
    }
    let _ = session.run(&polyline_epilogue());
    let _ = session.wait_idle(30_000);

    println!(
        "done            {} paths cut; blade lifted and returned to origin",
        path_jobs.len()
    );
    ExitCode::SUCCESS
}

fn status_name(s: Status) -> &'static str {
    match s {
        Status::Ready => "ready (0)",
        Status::Moving => "moving (1)",
        Status::Unloaded => "media not detected (2)",
        Status::Paused => "paused (3)",
        Status::Cancelled => "cancelled (4)",
        Status::Unknown(_) => "unknown",
    }
}
