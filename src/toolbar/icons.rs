//! SnapForge's original toolbar icons: stroke-based vector art drawn with tiny-skia.
//!
//! All icons are designed on a 20 × 20 grid with a 1.75 stroke, round caps and joins,
//! and scaled to the exact target pixel size. `fg` follows the theme; the accent
//! (#4F6BFF) and amber (#FFB82E) are used sparingly to give each icon one focal point.

use crate::settings::{CaptureMode, Destination};
use tiny_skia::{
    BlendMode, Color, FillRule, LineCap, LineJoin, Paint, Path, PathBuilder, Pixmap, Stroke, StrokeDash, Transform,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Icon {
    Mode(CaptureMode),
    Dest(Destination),
    Delay,
    Menu,
    Minimize,
}

impl Icon {
    /// Every icon, for previews and tests.
    #[cfg(test)]
    pub fn all() -> Vec<Icon> {
        let mut v: Vec<Icon> = CaptureMode::ALL.iter().map(|&m| Icon::Mode(m)).collect();
        v.extend(Destination::ALL.iter().map(|&d| Icon::Dest(d)));
        v.extend([Icon::Delay, Icon::Menu, Icon::Minimize]);
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub fg: Color,
    pub accent: Color,
    pub amber: Color,
}

pub fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::from_rgba8(r, g, b, 255)
}

impl Palette {
    pub fn for_theme(dark: bool) -> Palette {
        if dark {
            Palette { fg: rgb(0xE8, 0xE9, 0xEE), accent: rgb(0x6E, 0x86, 0xFF), amber: rgb(0xFF, 0xB8, 0x2E) }
        } else {
            Palette { fg: rgb(0x23, 0x26, 0x2E), accent: rgb(0x4F, 0x6B, 0xFF), amber: rgb(0xF5, 0xA6, 0x14) }
        }
    }
}

const STROKE: f32 = 1.75;

/// Drawing helper in 20-unit icon space.
struct Pen<'a> {
    pm: &'a mut Pixmap,
    ts: Transform,
    pal: Palette,
}

fn paint(c: Color) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(c);
    p.anti_alias = true;
    p
}

fn stroke(width: f32) -> Stroke {
    Stroke { width, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Stroke::default() }
}

pub fn rrect(x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> Option<Path> {
    let mut pb = PathBuilder::new();
    let k = 0.4477; // 1 - 0.5523 (cubic circle approximation)
    pb.move_to(x0 + r, y0);
    pb.line_to(x1 - r, y0);
    pb.cubic_to(x1 - r * k, y0, x1, y0 + r * k, x1, y0 + r);
    pb.line_to(x1, y1 - r);
    pb.cubic_to(x1, y1 - r * k, x1 - r * k, y1, x1 - r, y1);
    pb.line_to(x0 + r, y1);
    pb.cubic_to(x0 + r * k, y1, x0, y1 - r * k, x0, y1 - r);
    pb.line_to(x0, y0 + r);
    pb.cubic_to(x0, y0 + r * k, x0 + r * k, y0, x0 + r, y0);
    pb.close();
    pb.finish()
}

fn poly(pts: &[(f32, f32)], close: bool) -> Option<Path> {
    let mut pb = PathBuilder::new();
    for (i, &(x, y)) in pts.iter().enumerate() {
        if i == 0 {
            pb.move_to(x, y);
        } else {
            pb.line_to(x, y);
        }
    }
    if close {
        pb.close();
    }
    pb.finish()
}

fn circle(cx: f32, cy: f32, r: f32) -> Option<Path> {
    PathBuilder::from_circle(cx, cy, r)
}

impl Pen<'_> {
    fn stroke_c(&mut self, path: Option<Path>, c: Color, width: f32) {
        if let Some(p) = path {
            self.pm.stroke_path(&p, &paint(c), &stroke(width), self.ts, None);
        }
    }
    fn line(&mut self, path: Option<Path>) {
        let c = self.pal.fg;
        self.stroke_c(path, c, STROKE);
    }
    fn accent_line(&mut self, path: Option<Path>) {
        let c = self.pal.accent;
        self.stroke_c(path, c, STROKE);
    }
    fn dashed(&mut self, path: Option<Path>, c: Color, on: f32, off: f32) {
        if let Some(p) = path {
            let mut s = stroke(STROKE);
            s.dash = StrokeDash::new(vec![on, off], 0.0);
            self.pm.stroke_path(&p, &paint(c), &s, self.ts, None);
        }
    }
    fn fill(&mut self, path: Option<Path>, c: Color) {
        if let Some(p) = path {
            self.pm.fill_path(&p, &paint(c), FillRule::Winding, self.ts, None);
        }
    }
    /// Erase to transparent (a "halo" that separates overlapping shapes).
    fn clear_stroke(&mut self, path: Option<Path>, width: f32) {
        if let Some(p) = path {
            let mut pt = paint(Color::BLACK);
            pt.blend_mode = BlendMode::Clear;
            self.pm.stroke_path(&p, &pt, &stroke(width), self.ts, None);
        }
    }
    fn clear_fill(&mut self, path: Option<Path>) {
        if let Some(p) = path {
            let mut pt = paint(Color::BLACK);
            pt.blend_mode = BlendMode::Clear;
            self.pm.fill_path(&p, &pt, FillRule::Winding, self.ts, None);
        }
    }
}

fn with_alpha(mut c: Color, a: f32) -> Color {
    c.set_alpha(a);
    c
}

/// Render `icon` at `size` × `size` pixels (premultiplied RGBA).
pub fn render(icon: Icon, size: u32, pal: Palette) -> Option<Pixmap> {
    let mut pm = Pixmap::new(size, size)?;
    let s = size as f32 / 20.0;
    let mut pen = Pen { pm: &mut pm, ts: Transform::from_scale(s, s), pal };
    draw(icon, &mut pen);
    Some(pm)
}

fn draw(icon: Icon, p: &mut Pen) {
    let pal = p.pal;
    match icon {
        Icon::Mode(CaptureMode::ActiveWindow) => {
            // Window whose title bar glows in the accent colour: "the active one".
            let mut pb = PathBuilder::new();
            pb.move_to(2.5, 7.5);
            pb.line_to(2.5, 5.75);
            pb.cubic_to(2.5, 4.5, 3.25, 3.5, 4.75, 3.5);
            pb.line_to(15.25, 3.5);
            pb.cubic_to(16.75, 3.5, 17.5, 4.5, 17.5, 5.75);
            pb.line_to(17.5, 7.5);
            pb.close();
            p.fill(pb.finish(), pal.accent);
            p.line(rrect(2.5, 3.5, 17.5, 16.5, 2.25));
            p.line(poly(&[(2.5, 7.5), (17.5, 7.5)], false));
        }
        Icon::Mode(CaptureMode::WindowObject) => {
            // A window being picked by the pointer.
            p.line(rrect(2.5, 3.5, 15.5, 14.0, 2.0));
            p.line(poly(&[(2.5, 6.75), (15.5, 6.75)], false));
            let arrow = || poly(&[(10.5, 9.0), (17.5, 13.25), (14.25, 14.25), (12.75, 17.5)], true);
            p.clear_stroke(arrow(), 4.0);
            p.clear_fill(arrow());
            p.fill(arrow(), pal.accent);
            p.stroke_c(arrow(), pal.accent, 1.5);
        }
        Icon::Mode(CaptureMode::Region) => {
            // Four selection corners around an accent area.
            let (a, b, l) = (2.75, 17.25, 4.25);
            for (cx, cy, dx, dy) in [(a, a, 1.0, 1.0), (b, a, -1.0, 1.0), (a, b, 1.0, -1.0), (b, b, -1.0, -1.0)] {
                p.line(poly(&[(cx, cy + dy * l), (cx, cy), (cx + dx * l, cy)], false));
            }
            p.fill(rrect(6.75, 6.75, 13.25, 13.25, 1.5), with_alpha(pal.accent, 0.35));
            p.stroke_c(rrect(6.75, 6.75, 13.25, 13.25, 1.5), pal.accent, 1.5);
        }
        Icon::Mode(CaptureMode::Freehand) => {
            // A dashed lasso with the pen tip at its end.
            let mut pb = PathBuilder::new();
            pb.move_to(13.5, 15.75);
            pb.cubic_to(12.0, 16.4, 10.5, 16.6, 9.0, 16.5);
            pb.cubic_to(4.5, 16.2, 2.5, 13.2, 2.75, 9.75);
            pb.cubic_to(3.0, 5.9, 6.4, 3.25, 10.25, 3.25);
            pb.cubic_to(14.5, 3.25, 17.4, 5.8, 17.25, 9.25);
            pb.cubic_to(17.15, 11.0, 16.4, 12.3, 15.25, 13.0);
            p.dashed(pb.finish(), pal.fg, 2.4, 2.6);
            p.fill(circle(15.0, 14.75, 2.0), pal.accent);
        }
        Icon::Mode(CaptureMode::FullScreen) => {
            // A monitor whose whole screen is selected.
            p.line(rrect(2.0, 3.25, 18.0, 14.0, 2.0));
            p.fill(rrect(4.75, 6.0, 15.25, 11.25, 0.75), pal.accent);
            p.line(poly(&[(10.0, 14.0), (10.0, 16.75)], false));
            p.line(poly(&[(6.5, 16.75), (13.5, 16.75)], false));
        }
        Icon::Mode(CaptureMode::Scrolling) => {
            // A long page with a scroll arrow.
            p.line(rrect(2.75, 2.5, 12.25, 17.5, 2.0));
            for (y, x1) in [(6.5, 9.25), (10.0, 9.25), (13.5, 7.75)] {
                p.line(poly(&[(5.75, y), (x1, y)], false));
            }
            p.accent_line(poly(&[(16.25, 3.5), (16.25, 16.25)], false));
            p.accent_line(poly(&[(14.0, 14.0), (16.25, 16.25), (18.5, 14.0)], false));
        }
        Icon::Mode(CaptureMode::FixedRegion) => {
            // A frame with a fixed width dimension above it.
            p.line(rrect(2.75, 7.0, 17.25, 17.0, 2.0));
            p.accent_line(poly(&[(2.75, 3.5), (17.25, 3.5)], false));
            p.accent_line(poly(&[(2.75, 1.75), (2.75, 5.0)], false));
            p.accent_line(poly(&[(17.25, 1.75), (17.25, 5.0)], false));
            p.fill(rrect(5.5, 9.75, 10.0, 14.25, 1.0), with_alpha(pal.accent, 0.35));
        }
        Icon::Dest(Destination::Editor) => {
            // A picture: mountains and an amber sun.
            p.line(rrect(2.5, 3.5, 17.5, 16.5, 2.25));
            p.line(poly(&[(5.5, 13.75), (8.75, 10.0), (11.5, 12.75), (13.0, 11.25), (14.75, 13.0)], false));
            p.fill(circle(13.0, 7.25, 1.6), pal.amber);
        }
        Icon::Dest(Destination::Draw) => {
            // A pencil with an accent tip.
            let body = poly(&[(13.25, 3.25), (16.75, 6.75), (7.25, 16.25), (3.25, 16.75), (3.75, 12.75)], true);
            p.fill(poly(&[(3.25, 16.75), (3.6, 13.9), (6.1, 16.4)], true), pal.accent);
            p.line(body);
            p.line(poly(&[(11.25, 5.25), (14.75, 8.75)], false));
        }
        Icon::Dest(Destination::Clipboard) => {
            p.line(rrect(3.75, 4.25, 16.25, 17.75, 2.0));
            p.clear_fill(rrect(6.75, 2.0, 13.25, 6.25, 1.25));
            p.fill(rrect(7.0, 2.25, 13.0, 6.0, 1.25), pal.accent);
            p.line(poly(&[(7.0, 10.0), (13.0, 10.0)], false));
            p.line(poly(&[(7.0, 13.5), (11.0, 13.5)], false));
        }
        Icon::Dest(Destination::File) => {
            // A disk: save to file.
            let mut pb = PathBuilder::new();
            pb.move_to(4.75, 3.0);
            pb.line_to(13.5, 3.0);
            pb.line_to(17.0, 6.5);
            pb.line_to(17.0, 15.25);
            pb.cubic_to(17.0, 16.25, 16.25, 17.0, 15.25, 17.0);
            pb.line_to(4.75, 17.0);
            pb.cubic_to(3.75, 17.0, 3.0, 16.25, 3.0, 15.25);
            pb.line_to(3.0, 4.75);
            pb.cubic_to(3.0, 3.75, 3.75, 3.0, 4.75, 3.0);
            pb.close();
            p.fill(rrect(6.25, 11.0, 13.75, 17.0, 0.75), with_alpha(pal.accent, 0.35));
            p.line(pb.finish());
            p.line(poly(&[(6.5, 3.25), (6.5, 6.5), (12.25, 6.5), (12.25, 3.25)], false));
            p.accent_line(poly(&[(6.25, 17.0), (6.25, 11.0), (13.75, 11.0), (13.75, 17.0)], false));
        }
        Icon::Dest(Destination::Printer) => {
            p.line(poly(&[(5.5, 7.25), (5.5, 2.75), (14.5, 2.75), (14.5, 7.25)], false));
            p.line(rrect(2.5, 7.25, 17.5, 14.25, 2.0));
            p.clear_fill(rrect(4.75, 10.5, 15.25, 18.25, 0.5));
            p.fill(rrect(5.5, 11.25, 14.5, 17.5, 0.5), with_alpha(pal.accent, 0.35));
            p.accent_line(rrect(5.5, 11.25, 14.5, 17.5, 0.5));
            p.fill(circle(14.75, 9.5, 0.9), pal.amber);
        }
        Icon::Dest(Destination::Email) => {
            p.line(rrect(2.5, 4.25, 17.5, 15.75, 2.25));
            p.accent_line(poly(&[(3.5, 5.75), (10.0, 10.75), (16.5, 5.75)], false));
        }
        Icon::Dest(d @ (Destination::Word | Destination::Excel | Destination::PowerPoint | Destination::OneNote)) => {
            office_doc(p, d)
        }
        Icon::Dest(Destination::Upload) => {
            // A cloud with an upward arrow.
            let mut pb = PathBuilder::new();
            pb.move_to(6.0, 15.25);
            pb.line_to(14.75, 15.25);
            pb.cubic_to(16.9, 15.25, 18.4, 13.6, 18.25, 11.6);
            pb.cubic_to(18.1, 9.7, 16.6, 8.5, 14.85, 8.6);
            pb.cubic_to(14.3, 6.0, 12.2, 4.25, 9.6, 4.25);
            pb.cubic_to(6.9, 4.25, 4.9, 6.25, 4.6, 8.9);
            pb.cubic_to(2.9, 9.3, 1.75, 10.75, 1.75, 12.3);
            pb.cubic_to(1.75, 14.0, 3.25, 15.25, 5.0, 15.25);
            pb.close();
            p.line(pb.finish());
            let shaft = || poly(&[(10.0, 18.25), (10.0, 9.75)], false);
            let head = || poly(&[(7.5, 12.25), (10.0, 9.75), (12.5, 12.25)], false);
            p.clear_stroke(shaft(), 5.0);
            p.clear_stroke(head(), 5.0);
            p.accent_line(shaft());
            p.accent_line(head());
        }
        Icon::Dest(Destination::Program) => {
            // A console window with a prompt.
            p.line(rrect(2.5, 3.5, 17.5, 16.5, 2.25));
            p.accent_line(poly(&[(6.0, 8.0), (8.75, 10.5), (6.0, 13.0)], false));
            p.line(poly(&[(10.75, 13.0), (14.0, 13.0)], false));
        }
        Icon::Delay => {
            // A stopwatch.
            p.line(circle(10.0, 11.0, 6.75));
            p.line(poly(&[(8.0, 2.0), (12.0, 2.0)], false));
            p.line(poly(&[(10.0, 2.0), (10.0, 4.25)], false));
            p.line(poly(&[(15.25, 4.75), (16.5, 6.0)], false));
            p.accent_line(poly(&[(10.0, 11.0), (12.75, 8.25)], false));
            p.fill(circle(10.0, 11.0, 1.25), pal.accent);
        }
        Icon::Menu => {
            for y in [5.25, 10.0, 14.75] {
                p.line(poly(&[(3.75, y), (16.25, y)], false));
            }
        }
        Icon::Minimize => {
            p.line(poly(&[(5.0, 10.0), (15.0, 10.0)], false));
        }
    }
}

/// Generic document with a lettered badge: W(ord), X (Excel), P(owerPoint), N (OneNote).
fn office_doc(p: &mut Pen, d: Destination) {
    let pal = p.pal;
    let mut pb = PathBuilder::new();
    pb.move_to(7.0, 2.5);
    pb.line_to(13.0, 2.5);
    pb.line_to(17.25, 6.75);
    pb.line_to(17.25, 15.5);
    pb.cubic_to(17.25, 16.6, 16.35, 17.5, 15.25, 17.5);
    pb.line_to(7.0, 17.5);
    pb.cubic_to(5.9, 17.5, 5.0, 16.6, 5.0, 15.5);
    pb.line_to(5.0, 4.5);
    pb.cubic_to(5.0, 3.4, 5.9, 2.5, 7.0, 2.5);
    pb.close();
    p.line(pb.finish());
    p.line(poly(&[(12.75, 2.75), (12.75, 7.0), (17.0, 7.0)], false));
    // Content hint on the page's right side.
    let thin = 1.25;
    match d {
        Destination::Excel => {
            p.stroke_c(poly(&[(12.25, 10.5), (15.0, 10.5)], false), pal.fg, thin);
            p.stroke_c(poly(&[(12.25, 13.5), (15.0, 13.5)], false), pal.fg, thin);
            p.stroke_c(poly(&[(13.6, 9.5), (13.6, 14.75)], false), pal.fg, thin);
        }
        Destination::PowerPoint => {
            p.fill(circle(13.75, 12.25, 1.75), pal.fg);
        }
        Destination::OneNote => {
            p.stroke_c(poly(&[(12.5, 10.25), (14.75, 10.25)], false), pal.fg, thin);
            p.stroke_c(poly(&[(12.5, 13.25), (14.75, 13.25)], false), pal.fg, thin);
        }
        _ => {
            for y in [10.0, 12.5, 15.0] {
                p.stroke_c(poly(&[(12.5, y), (14.75, y)], false), pal.fg, thin);
            }
        }
    }
    let badge_color = match d {
        Destination::Excel => rgb(0x1F, 0xA8, 0x6A),
        Destination::PowerPoint => rgb(0xEE, 0x6E, 0x3A),
        Destination::OneNote => rgb(0x8B, 0x5C, 0xF6),
        _ => pal.accent,
    };
    let badge = || rrect(1.5, 8.5, 10.75, 17.75, 2.25);
    p.clear_stroke(badge(), 2.0);
    p.fill(badge(), badge_color);
    let white = Color::WHITE;
    let letter = match d {
        Destination::Word => poly(&[(3.25, 10.75), (4.5, 15.25), (6.0, 11.75), (7.5, 15.25), (8.75, 10.75)], false),
        Destination::Excel => {
            let mut pb = PathBuilder::new();
            pb.move_to(3.75, 10.75);
            pb.line_to(8.25, 15.25);
            pb.move_to(8.25, 10.75);
            pb.line_to(3.75, 15.25);
            pb.finish()
        }
        Destination::PowerPoint => {
            let mut pb = PathBuilder::new();
            pb.move_to(4.5, 15.25);
            pb.line_to(4.5, 10.75);
            pb.line_to(6.5, 10.75);
            pb.cubic_to(8.4, 10.75, 8.4, 13.6, 6.5, 13.6);
            pb.line_to(4.5, 13.6);
            pb.finish()
        }
        _ => poly(&[(4.0, 15.25), (4.0, 10.75), (8.0, 15.25), (8.0, 10.75)], false),
    };
    p.stroke_c(letter, white, 1.4);
}

/// Convert a premultiplied RGBA pixmap into premultiplied BGRA bytes (for a DIB).
pub fn to_bgra(pm: &Pixmap) -> Vec<u8> {
    let mut out = pm.data().to_vec();
    for px in out.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_draws_something() {
        for dark in [false, true] {
            for icon in Icon::all() {
                for size in [16, 20, 28, 40] {
                    let pm = render(icon, size, Palette::for_theme(dark)).expect("pixmap");
                    let ink = pm.data().chunks_exact(4).filter(|p| p[3] > 0).count();
                    assert!(ink > (size * size / 20) as usize, "{icon:?} at {size}px is empty");
                }
            }
        }
    }
}
