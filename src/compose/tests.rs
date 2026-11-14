//! Tests for combine layout, PDF generation, split points and print planning.

use super::batch::{expand_template, page_areas, plan_print, DeviceGeom, TemplateCtx};
use super::combine::{combine_images, combine_layout, CombineAlign, CombineItem, CombineParams, LABEL_GAP};
use super::pdf::{
    images_to_pdf, images_to_pdf_bytes, plan_pages, PageOrientation, PageSize, PdfOptions, PdfQuality, PdfSource,
};
use super::split::{row_stats, split_image_rows, split_points};
use crate::common::{Image, Rect};
use crate::effects::{Color, Orientation};
use crate::settings::print::{MarginsMm, PrintFit};

/// A "document": white page with black text-like line blocks of `line_h` rows every `pitch`.
fn document(w: u32, h: u32, line_h: u32, pitch: u32) -> Image {
    let mut img = Image::filled(w, h, [255, 255, 255, 255]);
    for y in 0..h {
        if y % pitch < line_h {
            for x in (10..w - 10).step_by(3) {
                img.set_pixel(x, y, [0, 0, 0, 255]);
            }
        }
    }
    img
}

fn count_pages(pdf: &[u8]) -> usize {
    let s = String::from_utf8_lossy(pdf);
    s.matches("/Type /Page").count() - s.matches("/Type /Pages").count()
}

// ------------------------------------------------------------------ combine

#[test]
fn combine_layout_math() {
    let p = CombineParams { spacing: 10, margin: 5, align: CombineAlign::Center, ..Default::default() };
    let l = combine_layout(&[(100, 50), (40, 80)], &[(0, 0), (0, 0)], &p);
    assert_eq!((l.width, l.height), (5 + 100 + 10 + 40 + 5, 5 + 80 + 5));
    assert_eq!((l.cells[0].image_x, l.cells[0].image_y), (5, 5 + 15));
    assert_eq!((l.cells[1].image_x, l.cells[1].image_y), (115, 5));

    let end = combine_layout(&[(100, 50), (40, 80)], &[], &CombineParams { align: CombineAlign::End, ..p.clone() });
    assert_eq!(end.cells[0].image_y, 5 + 30);
    let start = combine_layout(&[(100, 50), (40, 80)], &[], &CombineParams { align: CombineAlign::Start, ..p.clone() });
    assert_eq!(start.cells[0].image_y, 5);

    let v = combine_layout(
        &[(100, 50), (40, 80)],
        &[(60, 12), (0, 0)],
        &CombineParams { direction: Orientation::Vertical, ..p.clone() },
    );
    // First cell: image 50 + gap + label 12.
    assert_eq!((v.width, v.height), (5 + 100 + 5, 5 + 50 + LABEL_GAP + 12 + 10 + 80 + 5));
    assert_eq!((v.cells[0].image_x, v.cells[0].image_y), (5, 5));
    assert_eq!((v.cells[0].label_x, v.cells[0].label_y), (5 + 20, 5 + 50 + LABEL_GAP as i32));
    assert_eq!((v.cells[1].image_x, v.cells[1].image_y), (5 + 30, 5 + 50 + LABEL_GAP as i32 + 12 + 10));
    // A label wider than its image widens the cell; the image centres in it.
    let wide = combine_layout(&[(20, 20)], &[(80, 10)], &CombineParams { margin: 0, ..p });
    assert_eq!(wide.width, 80);
    assert_eq!(wide.cells[0].image_x, 30);
}

#[test]
fn combine_renders() {
    let a = Image::filled(60, 40, [0, 0, 255, 255]);
    let b = Image::filled(30, 70, [255, 0, 0, 255]);
    let items = vec![CombineItem { image: a, label: "a.png".into() }, CombineItem { image: b, label: "b.png".into() }];
    let p = CombineParams { spacing: 8, margin: 4, background: Color::WHITE, ..Default::default() };
    let out = combine_images(&items, &p);
    assert_eq!((out.width, out.height), (4 + 60 + 8 + 30 + 4, 4 + 70 + 4));
    assert_eq!(out.pixel(4, 4 + 15), [0, 0, 255, 255]);
    assert_eq!(out.pixel(4 + 60 + 8, 4), [255, 0, 0, 255]);
    assert_eq!(out.pixel(0, 0), [255, 255, 255, 255]);
    let labelled = combine_images(&items, &CombineParams { labels: true, shadow: true, ..p.clone() });
    assert!(labelled.height > out.height && labelled.width > out.width);
    assert!(!labelled.has_transparency());
    let clear = combine_images(&items, &CombineParams { background: Color::TRANSPARENT, ..p });
    assert_eq!(clear.pixel(0, 0)[3], 0);
}

// ------------------------------------------------------------------ split points

#[test]
fn split_points_avoid_text_lines() {
    // Lines of 12 text rows every 20 rows: rows 0-11 text, 12-19 blank, and so on.
    let doc = document(200, 1000, 12, 20);
    let stats = row_stats(&doc);
    assert!(stats[5].1 > 100.0 && stats[15].1 == 0.0);
    // Without avoiding lines: exact page boundaries.
    assert_eq!(split_points(&stats, 300, 75, false), vec![300, 600, 900, 1000]);
    let cuts = split_points(&stats, 300, 75, true);
    assert_eq!(*cuts.last().unwrap(), 1000);
    let mut prev = 0;
    for &c in &cuts[..cuts.len() - 1] {
        assert!(c - prev <= 300, "chunk too tall: {prev}..{c}");
        assert!(c - prev >= 225, "cut moved too far: {prev}..{c}");
        // The row before the cut is blank and the cut never lands inside a text line.
        assert_eq!(stats[(c - 1) as usize].1, 0.0, "cut at {c} splits a line");
        prev = c;
    }
    // Text runs that never pause: falls back to the hard boundary.
    let dense = document(100, 500, 20, 20);
    assert_eq!(split_image_rows(&dense, 200, true), vec![200, 400, 500]);
    // Short images are never split.
    assert_eq!(split_image_rows(&dense, 600, true), vec![500]);
}

#[test]
fn split_prefers_white_rows() {
    // A grey uniform band near the boundary and a white one a bit higher.
    let mut stats = vec![(0.0f32, 900.0f32); 400];
    for s in &mut stats[180..185] {
        *s = (255.0, 0.0);
    }
    for s in &mut stats[195..199] {
        *s = (128.0, 0.0);
    }
    let cuts = split_points(&stats, 200, 50, true);
    assert_eq!(cuts[0], 185, "white run wins over a closer grey one");
}

// ------------------------------------------------------------------ PDF

#[test]
fn pdf_page_plans() {
    let o = PdfOptions { margin_mm: 10.0, ..Default::default() };
    // Small image: one page, natural size (0.75 pt/px), centred, portrait.
    let p =
        plan_pages(400, 300, &PdfOptions { orientation: PageOrientation::Portrait, ..o.clone() }, |_| unreachable!());
    assert_eq!(p.len(), 1);
    assert!((p[0].w - 300.0).abs() < 0.01 && (p[0].h - 225.0).abs() < 0.01);
    assert!(p[0].page_w < p[0].page_h);
    assert!((p[0].x - (p[0].page_w - 300.0) / 2.0).abs() < 0.01, "centred");
    let tall = plan_pages(300, 400, &o, |_| unreachable!());
    assert!(tall[0].page_w < tall[0].page_h, "auto: portrait for a tall image");
    // Wide image: landscape in auto mode.
    let wide = plan_pages(2400, 1000, &o, |_| unreachable!());
    assert!(wide[0].page_w > wide[0].page_h);
    let forced =
        plan_pages(2400, 1000, &PdfOptions { orientation: PageOrientation::Portrait, ..o.clone() }, |_| unreachable!());
    assert!(forced[0].page_w < forced[0].page_h);
    // Long image: split into pages that each fill at most the printable height.
    let long = plan_pages(800, 6000, &o, |max| {
        assert!(max > 0);
        (1..).map(|i| (i * max).min(6000)).take_while(|&c| c < 6000).chain([6000]).collect()
    });
    assert!(long.len() >= 5, "{}", long.len());
    assert_eq!(long[0].y0, 0);
    assert_eq!(long.last().unwrap().y1, 6000);
    for w in long.windows(2) {
        assert_eq!(w[0].y1, w[1].y0);
    }
    // Not splitting: one page, shrunk to fit.
    let one = plan_pages(800, 6000, &PdfOptions { split_long_images: false, ..o.clone() }, |_| unreachable!());
    assert_eq!(one.len(), 1);
    // Fit to image: page = image at 0.75 pt/px + margins.
    let fit =
        plan_pages(400, 200, &PdfOptions { page_size: PageSize::FitToImage, margin_mm: 0.0, ..o }, |_| unreachable!());
    assert_eq!((fit[0].page_w, fit[0].page_h), (300.0, 150.0));
}

#[test]
fn pdf_generation() {
    let shot = Image::filled(320, 200, [40, 120, 220, 255]);
    let doc = document(900, 4000, 14, 24);
    let sources = vec![PdfSource::Image(shot.clone()), PdfSource::Image(doc), PdfSource::Image(shot)];
    for quality in [PdfQuality::Jpeg, PdfQuality::Lossless] {
        let o = PdfOptions { quality, title: "Test".into(), ..Default::default() };
        let (bytes, pages) = images_to_pdf_bytes(&sources, &o).unwrap();
        assert!(bytes.starts_with(b"%PDF-"));
        assert!(bytes.ends_with(b"%%EOF") || bytes.ends_with(b"%%EOF\n"));
        assert!(pages > 3, "long image split: {pages}");
        assert_eq!(count_pages(&bytes), pages);
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains(&format!("/Count {pages}")));
        assert_eq!(s.matches("/Subtype /Image").count(), pages);
        let filter = if quality == PdfQuality::Jpeg { "/DCTDecode" } else { "/FlateDecode" };
        assert!(s.contains(filter), "{filter}");
        assert!(s.contains("/MediaBox [0 0 595.276 841.89]"));
    }
    // File output and loading from paths.
    let dir = std::env::temp_dir().join(format!("snapforge-pdf-test-{}", std::process::id()));
    let png = dir.join("in.png");
    crate::output::save::save_image_as(
        &Image::filled(50, 50, [0, 0, 0, 255]),
        &png,
        crate::settings::ImageFormat::Png,
        100,
    )
    .unwrap();
    let out = dir.join("out.pdf");
    let n = images_to_pdf(&[PdfSource::Path(png)], &PdfOptions::default(), &out).unwrap();
    assert_eq!(n, 1);
    assert_eq!(count_pages(&std::fs::read(&out).unwrap()), 1);
    assert!(images_to_pdf(&[], &PdfOptions::default(), &out).is_err());
    assert!(images_to_pdf(&[PdfSource::Path(dir.join("missing.png"))], &PdfOptions::default(), &out).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------------ batch print planning

fn letter_600dpi() -> DeviceGeom {
    // 8.5x11 in at 600 DPI with 1/4 in unprintable borders.
    DeviceGeom {
        printable_w: 4800,
        printable_h: 6300,
        paper_w: 5100,
        paper_h: 6600,
        offset_x: 150,
        offset_y: 150,
        dpi_x: 600,
        dpi_y: 600,
    }
}

#[test]
fn print_areas_and_plans() {
    let g = letter_600dpi();
    let m = MarginsMm { left: 25.4, top: 25.4, right: 25.4, bottom: 25.4 };
    let a = page_areas(&g, &m, false, false, 75);
    // 1 inch margins = 600 device px from the paper edge = 450 from the printable origin.
    assert_eq!(a.content, Rect::new(450, 450, 5100 - 1200, 6600 - 1200));
    let hf = page_areas(&g, &m, true, true, 75);
    assert_eq!(hf.header, Rect::new(450, 450, 3900, 75));
    assert_eq!(hf.content.y, 450 + 75 + 37);
    assert_eq!(hf.content.bottom(), 450 + 5400 - 75 - 37);
    assert_eq!(hf.footer.bottom(), 450 + 5400);

    let area = Rect::new(0, 0, 4000, 5000);
    // Fit page: one page, scaled and centred.
    let fp = plan_print(1000, 500, area, PrintFit::FitPage, 600, true, |_| unreachable!());
    assert_eq!(fp.len(), 1);
    assert_eq!(fp[0].dest, Rect::new(0, 1500, 4000, 2000));
    // Actual size: 96 DPI pixels at 600 DPI = 6.25x; 400x2000 px -> 2500x12500 -> 3 pages.
    let act = plan_print(400, 2000, area, PrintFit::ActualSize, 600, true, |max| {
        assert_eq!(max, 800);
        vec![800, 1600, 2000]
    });
    assert_eq!(act.len(), 3);
    assert_eq!(act[0].dest, Rect::new(750, 0, 2500, 5000));
    assert_eq!(act[2].dest.h, 2500);
    // Fit width with a real split that avoids text lines.
    let doc = document(1000, 3000, 12, 20);
    let fw = plan_print(1000, 3000, area, PrintFit::FitWidth, 600, true, |max| split_image_rows(&doc, max, true));
    assert!(fw.len() >= 3);
    for s in &fw[..fw.len() - 1] {
        assert!(s.y1 - s.y0 <= 1250);
        assert!(s.y1 % 20 >= 12 || s.y1 % 20 == 0, "cut at {} is inside a text line", s.y1);
    }
}

#[test]
fn templates() {
    let c = TemplateCtx {
        file: "shot.png".into(),
        path: "C:\\x\\shot.png".into(),
        page: 2,
        total: 7,
        date: "2026-11-14".into(),
        time: "09:30".into(),
    };
    assert_eq!(expand_template("Page {n} of {total}", &c), "Page 2 of 7");
    assert_eq!(expand_template("{file} · {date} {time}", &c), "shot.png · 2026-11-14 09:30");
    assert_eq!(expand_template("{path} p{page}", &c), "C:\\x\\shot.png p2");
    assert_eq!(expand_template("plain", &c), "plain");
}

/// Writes a combined PNG and a PDF: `cargo test write_compose_samples -- --ignored`.
#[test]
#[ignore]
fn write_compose_samples() {
    let dir = std::env::var_os("SNAPFORGE_SAMPLES")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join("effect-samples"));
    let shot = crate::effects::tests::sample_screenshot(480, 300);
    let small = crate::effects::quick_resize(&shot, 2);
    let items = vec![
        CombineItem { image: shot.clone(), label: "editor-window.png".into() },
        CombineItem { image: small.clone(), label: "a-much-longer-file-name-for-the-thumbnail.png".into() },
        CombineItem { image: crate::effects::grayscale(&shot), label: "grayscale.png".into() },
    ];
    let p = CombineParams {
        shadow: true,
        labels: true,
        margin: 24,
        spacing: 24,
        background: Color::rgb(241, 243, 246),
        ..Default::default()
    };
    let h = combine_images(&items, &p);
    let v =
        combine_images(&items, &CombineParams { direction: Orientation::Vertical, align: CombineAlign::Start, ..p });
    let fmt = crate::settings::ImageFormat::Png;
    crate::output::save::save_image_as(&h, &dir.join("40_combine_horizontal.png"), fmt, 100).unwrap();
    crate::output::save::save_image_as(&v, &dir.join("41_combine_vertical.png"), fmt, 100).unwrap();
    let doc = document(900, 4000, 14, 24);
    let n = images_to_pdf(
        &[PdfSource::Image(shot), PdfSource::Image(doc), PdfSource::Image(small)],
        &PdfOptions { quality: PdfQuality::Lossless, title: "SnapForge sample".into(), ..Default::default() },
        &dir.join("42_images.pdf"),
    )
    .unwrap();
    assert!(n >= 4);
}
