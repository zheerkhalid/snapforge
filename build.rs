//! Build script: renders the original SnapForge icon (vector, via tiny-skia) into a
//! multi-size .ico, then compiles the Win32 resources (icon, manifest, version info).

#![allow(clippy::field_reassign_with_default)]

use std::{env, fs, path::PathBuf};
use tiny_skia::*;

fn render_icon(size: u32) -> Vec<u8> {
    let mut pm = Pixmap::new(size, size).expect("pixmap");
    let s = size as f32 / 256.0;
    // Rounded square with an indigo -> violet gradient.
    let r = 56.0 * s;
    let (x0, y0, x1, y1) = (8.0 * s, 8.0 * s, 248.0 * s, 248.0 * s);
    let mut pb = PathBuilder::new();
    pb.move_to(x0 + r, y0);
    pb.line_to(x1 - r, y0);
    pb.quad_to(x1, y0, x1, y0 + r);
    pb.line_to(x1, y1 - r);
    pb.quad_to(x1, y1, x1 - r, y1);
    pb.line_to(x0 + r, y1);
    pb.quad_to(x0, y1, x0, y1 - r);
    pb.line_to(x0, y0 + r);
    pb.quad_to(x0, y0, x0 + r, y0);
    pb.close();
    let bg = pb.finish().expect("bg path");
    let mut paint = Paint::default();
    paint.anti_alias = true;
    paint.shader = LinearGradient::new(
        Point::from_xy(x0, y0),
        Point::from_xy(x1, y1),
        vec![
            GradientStop::new(0.0, Color::from_rgba8(0x4f, 0x6b, 0xff, 255)),
            GradientStop::new(1.0, Color::from_rgba8(0x9b, 0x3c, 0xf0, 255)),
        ],
        SpreadMode::Pad,
        Transform::identity(),
    )
    .expect("gradient");
    pm.fill_path(&bg, &paint, FillRule::Winding, Transform::identity(), None);

    // White capture-frame corner brackets.
    let mut white = Paint::default();
    white.anti_alias = true;
    white.set_color_rgba8(255, 255, 255, 255);
    let stroke = Stroke {
        width: (22.0 * s).max(1.6),
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    let (a, b, l) = (62.0 * s, 194.0 * s, 46.0 * s);
    for (cx, cy, dx, dy) in [(a, a, 1.0, 1.0), (b, a, -1.0, 1.0), (a, b, 1.0, -1.0), (b, b, -1.0, -1.0)] {
        let mut p = PathBuilder::new();
        p.move_to(cx, cy + dy * l);
        p.line_to(cx, cy);
        p.line_to(cx + dx * l, cy);
        if let Some(path) = p.finish() {
            pm.stroke_path(&path, &white, &stroke, Transform::identity(), None);
        }
    }
    // Amber "forge spark" in the centre.
    let mut spark = Paint::default();
    spark.anti_alias = true;
    spark.set_color_rgba8(0xff, 0xb8, 0x2e, 255);
    let c = 128.0 * s;
    let (ro, ri) = (40.0 * s, 13.0 * s);
    let mut p = PathBuilder::new();
    for i in 0..8 {
        let ang = std::f32::consts::FRAC_PI_4 * i as f32 - std::f32::consts::FRAC_PI_2;
        let rad = if i % 2 == 0 { ro } else { ri };
        let (px, py) = (c + rad * ang.cos(), c + rad * ang.sin());
        if i == 0 {
            p.move_to(px, py)
        } else {
            p.line_to(px, py)
        }
    }
    p.close();
    if let Some(path) = p.finish() {
        pm.fill_path(&path, &spark, FillRule::Winding, Transform::identity(), None);
    }
    pm.encode_png().expect("png")
}

fn write_ico(path: &PathBuf, sizes: &[u32]) {
    let pngs: Vec<Vec<u8>> = sizes.iter().map(|&s| render_icon(s)).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (i, &s) in sizes.iter().enumerate() {
        let b = if s >= 256 { 0 } else { s as u8 };
        out.extend_from_slice(&[b, b, 0, 0, 1, 0, 32, 0]);
        out.extend_from_slice(&(pngs[i].len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += pngs[i].len() as u32;
    }
    for p in &pngs {
        out.extend_from_slice(p);
    }
    fs::write(path, out).expect("write ico");
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=res/snapforge.manifest.in");
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let ico = out.join("snapforge.ico");
    write_ico(&ico, &[16, 20, 24, 32, 40, 48, 64, 256]);
    // Also keep a 256px PNG copy for docs (only written when it changed).
    let docs_png = PathBuf::from("docs/assets/snapforge-icon.png");
    let png = render_icon(256);
    if fs::read(&docs_png).ok().as_deref() != Some(&png[..]) {
        let _ = fs::create_dir_all("docs/assets");
        let _ = fs::write(&docs_png, &png);
    }

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let ver = env::var("CARGO_PKG_VERSION").expect("version");
    let parts: Vec<u16> = ver.split(['.', '-']).take(3).map(|p| p.parse().unwrap_or(0)).collect();
    let (ma, mi, pa) = (parts[0], parts.get(1).copied().unwrap_or(0), parts.get(2).copied().unwrap_or(0));
    let ver4 = format!("{ma}.{mi}.{pa}.0");

    let manifest = fs::read_to_string("res/snapforge.manifest.in").expect("manifest").replace("@VERSION4@", &ver4);
    let manifest_path = out.join("snapforge.manifest");
    fs::write(&manifest_path, manifest).expect("write manifest");

    let esc = |p: &PathBuf| p.display().to_string().replace('\\', "\\\\");
    let rc = format!(
        r#"#pragma code_page(65001)
1 ICON "{ico}"
1 24 "{manifest}"
1 VERSIONINFO
FILEVERSION {ma},{mi},{pa},0
PRODUCTVERSION {ma},{mi},{pa},0
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "CompanyName", "Zheer Khalid (zheer.dev)"
      VALUE "FileDescription", "SnapForge screen capture"
      VALUE "FileVersion", "{ver}"
      VALUE "InternalName", "SnapForge"
      VALUE "LegalCopyright", "Copyright (c) 2026 Zheer Khalid. MIT License."
      VALUE "OriginalFilename", "SnapForge.exe"
      VALUE "ProductName", "SnapForge"
      VALUE "ProductVersion", "{ver}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        ico = esc(&ico),
        manifest = esc(&manifest_path),
    );
    let rc_path = out.join("snapforge.rc");
    fs::write(&rc_path, rc).expect("write rc");
    embed_resource::compile(&rc_path, embed_resource::NONE);
    println!("cargo:rustc-link-arg-bins=/MANIFEST:NO");
}
