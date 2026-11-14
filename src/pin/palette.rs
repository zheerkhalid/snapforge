//! Draw Mode palette: layout, hit-testing and tiny-skia rendering (vector icons).
//! The palette window itself lives in `window.rs`.

use super::annot::{sk_color, Style, Tool, COLORS};
use crate::common::{Point, Rect};
use tiny_skia::{FillRule, LineCap, LineJoin, Paint, PathBuilder, PixmapMut, Rect as SkRect, Stroke, Transform};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    /// Drag handle: moves the pin together with the palette.
    Grip,
    Tool(Tool),
    Color(usize),
    /// Opens the system color dialog.
    CustomColor,
    /// Cycles S → M → L.
    Size,
    Undo,
    /// Leave Draw Mode.
    Done,
}

pub struct Layout {
    pub items: Vec<(Item, Rect)>,
    pub w: i32,
    pub h: i32,
}

impl Layout {
    pub fn hit(&self, p: Point) -> Option<Item> {
        self.items.iter().find(|(_, r)| r.contains(p)).map(|(i, _)| *i)
    }
}

fn s(v: i32, dpi: u32) -> i32 {
    crate::common::win::scale(v, dpi)
}

pub fn layout(dpi: u32) -> Layout {
    let pad = s(4, dpi);
    let btn = s(28, dpi);
    let sw = s(22, dpi);
    let gap = s(9, dpi);
    let mut x = pad;
    let mut items = Vec::new();
    let mut push = |item: Item, w: i32, x: &mut i32| {
        items.push((item, Rect::new(*x, pad, w, btn)));
        *x += w;
    };
    push(Item::Grip, s(16, dpi), &mut x);
    for t in [Tool::Pen, Tool::Text, Tool::Step] {
        push(Item::Tool(t), btn, &mut x);
    }
    x += gap;
    for i in 0..COLORS.len() {
        push(Item::Color(i), sw, &mut x);
    }
    push(Item::CustomColor, sw, &mut x);
    x += gap;
    push(Item::Size, btn, &mut x);
    push(Item::Undo, btn, &mut x);
    push(Item::Done, btn, &mut x);
    Layout { items, w: x + pad, h: btn + 2 * pad }
}

pub struct View {
    pub tool: Tool,
    pub style: Style,
    pub hover: Option<Item>,
    pub can_undo: bool,
    pub accent: u32,
    pub dpi: u32,
}

const BG: u32 = 0x26_26_28;
const HOVER: u32 = 0x3A_3A_3E;
const FG: u32 = 0xF2_F2_F2;
const FG_DIM: u32 = 0x77_77_7A;

fn paint(rgb: u32, a: u8) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(sk_color(rgb, a));
    p.anti_alias = true;
    p
}

fn rrect(r: Rect, rad: f32) -> Option<tiny_skia::Path> {
    let (x, y, w, h) = (r.x as f32, r.y as f32, r.w as f32, r.h as f32);
    let rad = rad.min(w / 2.0).min(h / 2.0);
    let mut pb = PathBuilder::new();
    pb.move_to(x + rad, y);
    pb.line_to(x + w - rad, y);
    pb.quad_to(x + w, y, x + w, y + rad);
    pb.line_to(x + w, y + h - rad);
    pb.quad_to(x + w, y + h, x + w - rad, y + h);
    pb.line_to(x + rad, y + h);
    pb.quad_to(x, y + h, x, y + h - rad);
    pb.line_to(x, y + rad);
    pb.quad_to(x, y, x + rad, y);
    pb.close();
    pb.finish()
}

fn line_stroke(w: f32) -> Stroke {
    Stroke { width: w, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() }
}

/// Draw an icon into a 24×24 design box mapped onto `r`.
fn icon(pm: &mut PixmapMut, item: Item, r: Rect, fg: u32, v: &View) {
    let k = (r.w.min(r.h) as f32) / 24.0 * 0.8;
    let ts =
        Transform::from_translate(r.x as f32 + r.w as f32 / 2.0 - 12.0 * k, r.y as f32 + r.h as f32 / 2.0 - 12.0 * k)
            .pre_scale(k, k);
    let fgp = paint(fg, 255);
    let st = line_stroke(2.0);
    match item {
        Item::Grip => {
            let kk = r.w as f32 / 16.0;
            for row in 0..3 {
                for col in 0..2 {
                    let cx = r.x as f32 + (5.5 + col as f32 * 5.0) * kk;
                    let cy = r.y as f32 + r.h as f32 / 2.0 + (row as f32 - 1.0) * 5.0 * kk;
                    if let Some(c) = PathBuilder::from_circle(cx, cy, 1.3 * kk) {
                        pm.fill_path(&c, &paint(FG_DIM, 255), FillRule::Winding, Transform::identity(), None);
                    }
                }
            }
        }
        Item::Tool(Tool::Pen) => {
            // Pencil body from bottom-left to top-right.
            let mut pb = PathBuilder::new();
            pb.move_to(4.0, 20.0);
            pb.line_to(5.0, 15.5);
            pb.line_to(16.5, 4.0);
            pb.line_to(20.0, 7.5);
            pb.line_to(8.5, 19.0);
            pb.close();
            if let Some(p) = pb.finish() {
                pm.stroke_path(&p, &fgp, &st, ts, None);
            }
            let mut pb = PathBuilder::new();
            pb.move_to(13.5, 7.0);
            pb.line_to(17.0, 10.5);
            if let Some(p) = pb.finish() {
                pm.stroke_path(&p, &fgp, &st, ts, None);
            }
        }
        Item::Tool(Tool::Text) => {
            let mut pb = PathBuilder::new();
            pb.move_to(5.0, 6.0);
            pb.line_to(19.0, 6.0);
            pb.move_to(12.0, 6.0);
            pb.line_to(12.0, 19.0);
            pb.move_to(9.0, 19.0);
            pb.line_to(15.0, 19.0);
            if let Some(p) = pb.finish() {
                pm.stroke_path(&p, &fgp, &line_stroke(2.4), ts, None);
            }
        }
        Item::Tool(Tool::Step) => {
            if let Some(c) = PathBuilder::from_circle(12.0, 12.0, 8.5) {
                pm.stroke_path(&c, &fgp, &st, ts, None);
            }
            // A "1".
            let mut pb = PathBuilder::new();
            pb.move_to(10.0, 9.5);
            pb.line_to(12.5, 7.5);
            pb.line_to(12.5, 16.5);
            if let Some(p) = pb.finish() {
                pm.stroke_path(&p, &fgp, &line_stroke(2.0), ts, None);
            }
        }
        Item::Size => {
            // Dot sized like the current preset, plus a faint ring for the largest.
            if let Some(c) = PathBuilder::from_circle(12.0, 12.0, 8.0) {
                pm.stroke_path(&c, &paint(FG_DIM, 255), &line_stroke(1.2), ts, None);
            }
            let rad = [2.5, 4.5, 7.0][(v.style.size as usize).min(2)];
            if let Some(c) = PathBuilder::from_circle(12.0, 12.0, rad) {
                pm.fill_path(&c, &fgp, FillRule::Winding, ts, None);
            }
        }
        Item::Undo => {
            let mut pb = PathBuilder::new();
            pb.move_to(8.0, 9.0);
            pb.line_to(15.0, 9.0);
            pb.cubic_to(21.0, 9.0, 21.0, 19.0, 15.0, 19.0);
            pb.line_to(9.0, 19.0);
            pb.move_to(11.5, 5.0);
            pb.line_to(7.5, 9.0);
            pb.line_to(11.5, 13.0);
            if let Some(p) = pb.finish() {
                pm.stroke_path(&p, &fgp, &st, ts, None);
            }
        }
        Item::Done => {
            let mut pb = PathBuilder::new();
            pb.move_to(5.0, 12.5);
            pb.line_to(10.0, 17.5);
            pb.line_to(19.5, 7.0);
            if let Some(p) = pb.finish() {
                pm.stroke_path(&p, &fgp, &line_stroke(2.4), ts, None);
            }
        }
        Item::Color(_) | Item::CustomColor => {}
    }
}

/// Render the palette into `buf` (`w`×`h` BGRA).
pub fn render(buf: &mut [u8], w: i32, h: i32, l: &Layout, v: &View) {
    let Some(mut pm) = PixmapMut::from_bytes(buf, w as u32, h as u32) else { return };
    if let Some(r) = SkRect::from_xywh(0.0, 0.0, w as f32, h as f32) {
        pm.fill_rect(r, &paint(BG, 255), Transform::identity(), None);
    }
    let rad = s(5, v.dpi) as f32;
    for &(item, r) in &l.items {
        let selected = matches!(item, Item::Tool(t) if t == v.tool);
        let hovered = v.hover == Some(item) && item != Item::Grip;
        match item {
            Item::Color(_) | Item::CustomColor => {
                let rgb = if let Item::Color(i) = item { COLORS[i] } else { v.style.color };
                let d = (r.w.min(r.h) as f32 * 0.62).round();
                let cx = r.x as f32 + r.w as f32 / 2.0;
                let cy = r.y as f32 + r.h as f32 / 2.0;
                let chosen = match item {
                    Item::Color(_) => rgb == v.style.color,
                    _ => !COLORS.contains(&v.style.color),
                };
                if chosen || hovered {
                    if let Some(c) = PathBuilder::from_circle(cx, cy, d / 2.0 + s(3, v.dpi) as f32) {
                        let ring = if chosen { 0xFF_FF_FF } else { FG_DIM };
                        pm.stroke_path(&c, &paint(ring, 255), &line_stroke(1.6), Transform::identity(), None);
                    }
                }
                if item == Item::CustomColor {
                    // Rainbow-ish quadrants mark "custom", the centre shows the current color.
                    let quads = [0xE5_39_35, 0xFD_D8_35, 0x43_A0_47, 0x1E_88_E5];
                    for (q, &qc) in quads.iter().enumerate() {
                        let a0 = q as f32 * std::f32::consts::FRAC_PI_2;
                        let mut pb = PathBuilder::new();
                        pb.move_to(cx, cy);
                        let steps = 8;
                        for st in 0..=steps {
                            let a = a0 + st as f32 / steps as f32 * std::f32::consts::FRAC_PI_2;
                            pb.line_to(cx + a.cos() * d / 2.0, cy + a.sin() * d / 2.0);
                        }
                        pb.close();
                        if let Some(p) = pb.finish() {
                            pm.fill_path(&p, &paint(qc, 255), FillRule::Winding, Transform::identity(), None);
                        }
                    }
                    if let Some(c) = PathBuilder::from_circle(cx, cy, d / 4.0) {
                        pm.fill_path(&c, &paint(rgb, 255), FillRule::Winding, Transform::identity(), None);
                    }
                } else if let Some(c) = PathBuilder::from_circle(cx, cy, d / 2.0) {
                    pm.fill_path(&c, &paint(rgb, 255), FillRule::Winding, Transform::identity(), None);
                    if rgb == 0x21_21_21 {
                        pm.stroke_path(&c, &paint(FG_DIM, 255), &line_stroke(1.0), Transform::identity(), None);
                    }
                }
            }
            _ => {
                let inner = r.inflate(-s(2, v.dpi));
                if selected || hovered {
                    if let Some(p) = rrect(inner, rad) {
                        let bg = if selected { v.accent } else { HOVER };
                        pm.fill_path(&p, &paint(bg, 255), FillRule::Winding, Transform::identity(), None);
                    }
                }
                let fg = if item == Item::Undo && !v.can_undo { FG_DIM } else { FG };
                icon(&mut pm, item, inner, fg, v);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_hits_every_item_once() {
        let l = layout(96);
        assert!(l.w > 200 && l.h >= 36);
        for &(item, r) in &l.items {
            assert_eq!(l.hit(r.center()), Some(item));
        }
        assert_eq!(l.hit(Point::new(-1, -1)), None);
        // Scales with DPI.
        assert!(layout(192).w > l.w * 2 - 4);
    }
}
