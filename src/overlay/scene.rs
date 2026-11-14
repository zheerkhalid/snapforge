//! What the overlay shows for a given [`State`], as a flat list of primitives.
//!
//! The same description drives painting and invalidation: every primitive has a
//! footprint (rect + content key), and after an input only the footprints that changed
//! (plus the area whose brightness changed) are invalidated.

use super::logic::{Handle, Phase, State};
use super::{loupe, OverlayMode};
use crate::common::win::scale;
use crate::common::{Point, Rect};
use std::collections::HashSet;
use std::hash::{Hash, Hasher};

/// GDI COLORREF (0x00BBGGRR).
pub const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}

pub const ACCENT: u32 = rgb(0x4F, 0x6B, 0xFF);
pub const PANEL: u32 = rgb(0x1C, 0x1E, 0x25);
pub const PANEL_BORDER: u32 = rgb(0x3A, 0x3F, 0x4D);
pub const TEXT: u32 = rgb(0xFF, 0xFF, 0xFF);
pub const TEXT_DIM: u32 = rgb(0xB4, 0xB9, 0xC6);
pub const WHITE: u32 = rgb(0xFF, 0xFF, 0xFF);
/// Brightness of the dimmed background (`c * DIM / 256`).
pub const DIM: u32 = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Font {
    pub px: i32,
    pub bold: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prim {
    Fill {
        r: Rect,
        color: u32,
    },
    Panel {
        r: Rect,
        color: u32,
        border: u32,
        radius: i32,
    },
    Text {
        r: Rect,
        text: String,
        color: u32,
        font: Font,
    },
    Line {
        a: Point,
        b: Point,
        color: u32,
        width: i32,
    },
    /// Zoomed pixels around `center` (virtual coordinates).
    Loupe {
        r: Rect,
        center: Point,
        cell: i32,
    },
}

impl Prim {
    pub fn bounds(&self) -> Rect {
        match self {
            Prim::Fill { r, .. } | Prim::Panel { r, .. } | Prim::Text { r, .. } | Prim::Loupe { r, .. } => *r,
            Prim::Line { a, b, width, .. } => {
                let w = width / 2 + 1;
                Rect::from_ltrb(a.x.min(b.x) - w, a.y.min(b.y) - w, a.x.max(b.x) + w + 1, a.y.max(b.y) + w + 1)
            }
        }
    }

    /// Content hash (rect excluded) so a changed label in the same place is still dirty.
    fn content_key(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        match self {
            Prim::Fill { color, .. } => (0u8, color).hash(&mut h),
            Prim::Panel { color, border, radius, .. } => (1u8, color, border, radius).hash(&mut h),
            Prim::Text { text, color, font, .. } => (2u8, text, color, font).hash(&mut h),
            Prim::Line { a, b, color, width } => (3u8, a.x, a.y, b.x, b.y, color, width).hash(&mut h),
            Prim::Loupe { center, cell, .. } => (4u8, center.x, center.y, cell).hash(&mut h),
        }
        h.finish()
    }
}

/// Hashable footprint entry: (x, y, w, h, content key).
pub type FpKey = (i32, i32, i32, i32, u64);

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Scene {
    /// Undimmed areas (shown from the original frozen image).
    pub bright: Vec<Rect>,
    /// Drawn in order on top.
    pub prims: Vec<Prim>,
}

impl Scene {
    pub fn footprint(&self) -> HashSet<FpKey> {
        self.prims
            .iter()
            .map(|p| {
                let r = p.bounds();
                (r.x, r.y, r.w, r.h, p.content_key())
            })
            .collect()
    }
}

/// Text measurement and pixel access the layout needs from the renderer.
pub trait Env {
    fn text_size(&mut self, text: &str, font: Font) -> (i32, i32);
    /// Frozen-screen pixel (BGRA) at a virtual-screen point.
    fn pixel(&self, p: Point) -> Option<[u8; 4]>;
}

pub fn size_text(r: &Rect) -> String {
    format!("{} \u{00D7} {}", r.w, r.h)
}

struct Builder<'a> {
    sc: Scene,
    env: &'a mut dyn Env,
    dpi: u32,
    mon: Rect,
}

impl Builder<'_> {
    fn s(&self, v: i32) -> i32 {
        scale(v, self.dpi)
    }

    /// Border thickness: 1 px up to 150 %, 2 px from 175 %.
    fn bw(&self) -> i32 {
        ((self.dpi as i32 + 40) / 96).max(1)
    }

    fn font(&self, px: i32, bold: bool) -> Font {
        Font { px: self.s(px), bold }
    }

    fn fill(&mut self, r: Rect, color: u32) {
        if !r.is_empty() {
            self.sc.prims.push(Prim::Fill { r, color });
        }
    }

    /// Border drawn just outside `r`, so the selected pixels stay fully visible.
    fn frame(&mut self, r: Rect, color: u32, bw: i32) {
        self.fill(Rect::new(r.x - bw, r.y - bw, r.w + 2 * bw, bw), color);
        self.fill(Rect::new(r.x - bw, r.bottom(), r.w + 2 * bw, bw), color);
        self.fill(Rect::new(r.x - bw, r.y, bw, r.h), color);
        self.fill(Rect::new(r.right(), r.y, bw, r.h), color);
    }

    fn handles(&mut self, r: Rect, active: Option<Point>, corners_only: bool) {
        let hs = self.s(7) | 1;
        let bw = self.bw();
        let small = r.w < hs * 3 || r.h < hs * 3;
        for h in Handle::ALL {
            let is_corner = matches!(h, Handle::TopLeft | Handle::TopRight | Handle::BottomRight | Handle::BottomLeft);
            if !is_corner && (small || corners_only) {
                continue;
            }
            let p = h.pos(&r);
            let sq = Rect::new(p.x - hs / 2, p.y - hs / 2, hs, hs);
            // The corner being dragged is filled with the accent colour.
            let hot = active.is_some_and(|a| (a.x - p.x).abs() <= 1 && (a.y - p.y).abs() <= 1);
            self.fill(sq, ACCENT);
            self.fill(sq.inflate(-bw), if hot { ACCENT } else { WHITE });
        }
    }

    fn panel(&mut self, r: Rect) {
        self.sc.prims.push(Prim::Panel { r, color: PANEL, border: PANEL_BORDER, radius: self.s(8) });
    }

    fn text(&mut self, r: Rect, text: String, color: u32, font: Font) {
        self.sc.prims.push(Prim::Text { r, text, color, font });
    }

    /// Size label above the selection's top-left corner (inside it when there is no room).
    fn label(&mut self, near: Rect, text: String) {
        let font = self.font(12, true);
        let (tw, th) = self.env.text_size(&text, font);
        let (px, py) = (self.s(7), self.s(3));
        let (w, h) = (tw + 2 * px, th + 2 * py);
        let gap = self.s(6);
        let mut r = Rect::new(near.x, near.y - h - gap, w, h);
        if r.y < self.mon.y {
            r = Rect::new(near.x + gap, near.y + gap, w, h);
        }
        let r = r.clamp_inside(&self.mon);
        self.panel(r);
        self.text(Rect::new(r.x + px, r.y + py, tw, th), text, TEXT, font);
    }

    fn hint(&mut self, key: &str, cursor: Point) {
        let text = crate::i18n::t(key);
        let font = self.font(13, false);
        let (tw, th) = self.env.text_size(&text, font);
        let (px, py) = (self.s(14), self.s(7));
        let margin = self.s(16);
        let w = (tw + 2 * px).min(self.mon.w - 2 * margin).max(1);
        let h = th + 2 * py;
        let x = self.mon.x + (self.mon.w - w) / 2;
        let mut r = Rect::new(x, self.mon.y + margin, w, h);
        // Get out of the way when the cursor is near it.
        if r.inflate(self.s(24)).contains(cursor) {
            r.y = self.mon.bottom() - margin - h;
        }
        self.panel(r);
        self.text(Rect::new(r.x + px, r.y + py, w - 2 * px, th), text, TEXT_DIM, font);
    }

    fn crosshair(&mut self, c: Point, bounds: Rect) {
        self.fill(Rect::new(bounds.x, c.y, bounds.w, 1), ACCENT);
        self.fill(Rect::new(c.x, bounds.y, 1, bounds.h), ACCENT);
    }

    fn loupe(&mut self, c: Point, sel: Option<Rect>) {
        let cell = loupe::cell_px(self.dpi);
        let side = loupe::CELLS * cell;
        let font = self.font(12, false);
        let pad = self.s(6);
        let border = self.bw();
        let px = self.env.pixel(c).unwrap_or([0, 0, 0, 255]);
        let (b, g, r) = (px[0], px[1], px[2]);
        let mut lines =
            vec![format!("{}, {}", c.x, c.y), format!("#{r:02X}{g:02X}{b:02X}"), format!("RGB {r}, {g}, {b}")];
        if let Some(s) = sel {
            lines.push(size_text(&s));
        }
        let line_h = self.env.text_size("Ag", font).1 + self.s(1);
        let text_h = lines.len() as i32 * line_h + 2 * pad;
        let w = side + 2 * border;
        let h = side + 2 * border + text_h;
        let gap = self.s(22);
        let outer = loupe::place(c, (w, h), gap, &self.mon);
        self.sc.prims.push(Prim::Panel { r: outer, color: PANEL, border: ACCENT, radius: 0 });
        self.sc.prims.push(Prim::Loupe {
            r: Rect::new(outer.x + border, outer.y + border, side, side),
            center: c,
            cell,
        });
        let tx = outer.x + border + pad;
        let mut ty = outer.y + border + side + pad;
        let tw = side - 2 * pad;
        for (i, line) in lines.into_iter().enumerate() {
            let mut x = tx;
            if i == 1 {
                // Colour swatch before the hex value.
                let sw = line_h - self.s(4);
                let sq = Rect::new(tx, ty + (line_h - sw) / 2, sw, sw);
                self.fill(sq, WHITE);
                self.fill(sq.inflate(-1), rgb(r, g, b));
                x += sw + self.s(6);
            }
            self.text(Rect::new(x, ty, tw - (x - tx), line_h), line, if i == 1 { TEXT } else { TEXT_DIM }, font);
            ty += line_h;
        }
    }
}

/// Lay out everything the overlay shows for `st`.
pub fn build(st: &State, env: &mut dyn Env) -> Scene {
    let m = st.monitor_at(st.cursor);
    let mut b = Builder { sc: Scene::default(), env, dpi: m.dpi.max(96), mon: m.rect };
    let bw = b.bw();
    let mut show_loupe = st.cfg.magnifier;
    let mut show_cross = st.cfg.crosshair;
    let hint;

    match st.mode {
        OverlayMode::Region | OverlayMode::ScrollPick => {
            let region = st.mode == OverlayMode::Region;
            hint = if region { "hint-region" } else { "hint-scroll" };
            match &st.phase {
                Phase::Dragging { .. } => {
                    if let Some(r) = st.selection_rect() {
                        b.sc.bright.push(r);
                        b.frame(r, ACCENT, bw);
                        b.handles(r, st.moving_corner().map(|c| Point::new(c.x + 1, c.y + 1)), false);
                        b.label(r, size_text(&r));
                    }
                }
                Phase::Adjust { rect, drag } => {
                    let r = *rect;
                    b.sc.bright.push(r);
                    b.frame(r, ACCENT, bw);
                    b.handles(r, None, false);
                    b.label(r, size_text(&r));
                    show_cross = false;
                    show_loupe &= drag.is_some();
                }
                _ => {
                    if let Some(h) = &st.hover {
                        let r = h.current().1.intersect(&st.bounds);
                        b.sc.bright.push(r);
                        b.frame(r, ACCENT, bw * 2);
                        b.label(r, hover_label(&r, h.level, h.chain.len()));
                    }
                    if !region {
                        show_cross = false;
                        show_loupe = false;
                    }
                }
            }
            let hint = if matches!(st.phase, Phase::Adjust { .. }) { "hint-adjust" } else { hint };
            finish(&mut b, st, show_cross, show_loupe, hint)
        }
        OverlayMode::Freehand => {
            if let Phase::Drawing { points } = &st.phase {
                let w = b.s(2).max(2);
                for seg in points.windows(2) {
                    b.sc.prims.push(Prim::Line { a: seg[0], b: seg[1], color: ACCENT, width: w });
                }
            }
            finish(&mut b, st, show_cross, show_loupe, "hint-freehand")
        }
        OverlayMode::WindowObject => {
            for (_, r) in &st.multi {
                let r = r.intersect(&st.bounds);
                b.sc.bright.push(r);
                b.frame(r, ACCENT, bw * 2);
                b.handles(r, None, true);
            }
            if let Some(h) = &st.hover {
                let r = h.current().1.intersect(&st.bounds);
                b.sc.bright.push(r);
                b.frame(r, ACCENT, bw * 2);
                b.label(r, hover_label(&r, h.level, h.chain.len()));
            }
            finish(&mut b, st, false, false, "hint-window")
        }
        OverlayMode::Fixed => {
            let r = st.fixed_rect();
            b.sc.bright.push(r);
            b.frame(r, ACCENT, bw);
            b.handles(r, None, true);
            b.label(r, size_text(&r));
            finish(&mut b, st, false, false, "hint-fixed")
        }
    }
}

fn hover_label(r: &Rect, level: usize, levels: usize) -> String {
    if levels > 1 {
        format!("{}   {}/{}", size_text(r), level + 1, levels)
    } else {
        size_text(r)
    }
}

fn finish(b: &mut Builder, st: &State, cross: bool, loupe: bool, hint: &str) -> Scene {
    if cross {
        b.crosshair(st.cursor, st.bounds);
    }
    b.hint(hint, st.cursor);
    if loupe {
        b.loupe(st.cursor, st.selection_rect());
    }
    std::mem::take(&mut b.sc)
}

#[cfg(test)]
mod tests {
    use super::super::logic::{Config, Input, Mods, Monitor};
    use super::*;

    struct FakeEnv;
    impl Env for FakeEnv {
        fn text_size(&mut self, text: &str, font: Font) -> (i32, i32) {
            (text.chars().count() as i32 * font.px / 2, font.px + 2)
        }
        fn pixel(&self, _: Point) -> Option<[u8; 4]> {
            Some([1, 2, 3, 255])
        }
    }

    fn state(mode: OverlayMode) -> State {
        let bounds = Rect::new(0, 0, 1920, 1080);
        State::new(
            mode,
            bounds,
            vec![(1, Rect::new(100, 100, 400, 300))],
            vec![Monitor { rect: bounds, dpi: 96 }],
            Config::default(),
            Point::new(800, 600),
            Box::new(|_, _, _, _| Vec::new()),
        )
    }

    #[test]
    fn small_move_dirties_little() {
        let mut st = state(OverlayMode::Region);
        st.handle(Input::LDown(Point::new(800, 600)), Mods::default());
        st.handle(Input::Move(Point::new(900, 700)), Mods::default());
        let a = build(&st, &mut FakeEnv);
        st.handle(Input::Move(Point::new(901, 700)), Mods::default());
        let b = build(&st, &mut FakeEnv);
        let (fa, fb) = (a.footprint(), b.footprint());
        let dirty: Vec<_> = fa.symmetric_difference(&fb).collect();
        // The hint bar and the unchanged edges stay clean.
        assert!(!dirty.is_empty());
        assert!(dirty.len() < fa.len() + fb.len());
        let area: i64 = dirty.iter().map(|k| k.2 as i64 * k.3 as i64).sum();
        assert!(area < 1920 * 1080 / 8, "dirty area {area}");
    }

    #[test]
    fn every_mode_builds_on_screen() {
        for mode in [
            OverlayMode::Region,
            OverlayMode::Freehand,
            OverlayMode::WindowObject,
            OverlayMode::Fixed,
            OverlayMode::ScrollPick,
        ] {
            let st = state(mode);
            let sc = build(&st, &mut FakeEnv);
            assert!(!sc.prims.is_empty(), "{mode:?}");
            for p in &sc.prims {
                if matches!(p, Prim::Panel { .. } | Prim::Loupe { .. }) {
                    let r = p.bounds();
                    assert_eq!(r.intersect(&st.bounds), r, "{mode:?} {p:?}");
                }
            }
        }
    }
}
