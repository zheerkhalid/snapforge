//! Tool size: which value is a tool's "size" (pen width, eraser radius, stamp size, …),
//! how the slider, the `[` / `]` keys and the wheel change it, and the on-canvas
//! feedback (the brush ring at the pointer and the brief size label).
//!
//! The mapping and stepping are pure and unit-tested; the painters are shared with the
//! clone stamp.

use crate::draw::Tool;
use crate::i18n::tf;
use crate::settings::draw::ToolStyle;
use eframe::egui::{self, vec2, Color32, Pos2, Sense, Stroke};
use std::time::{Duration, Instant};

/// The value that is a tool's size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeField {
    /// `style.width`: pen / stroke width.
    Width,
    /// `options.eraser_radius` (half the tip).
    EraserRadius,
    /// `options.step_size`: diameter of a step stamp.
    StepSize,
    /// `style.font.size`, in points.
    FontSize,
    /// `options.blur_strength`.
    Strength,
}

/// What the pointer shows on the canvas for a tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tip {
    /// Nothing at the pointer (only the size label while resizing).
    None,
    /// The round brush tip at the pointer, at its real size.
    Brush,
    /// The outline of the stamp that a click places.
    Stamp,
    /// A dot as thick as the stroke, next to the pointer.
    Dot,
}

/// Size of one tool: field, limits (the same as the value field's) and pointer feedback.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SizeSpec {
    pub field: SizeField,
    pub min: f32,
    pub max: f32,
    pub tip: Tip,
}

/// Slider curve: `value = min + (max - min) * t^GAMMA`, so the small sizes that are used
/// most get most of the track.
pub const SLIDER_GAMMA: f32 = 2.0;
/// Growth per `]` press / wheel notch (at least one whole unit).
pub const STEP_FACTOR: f32 = 1.15;
/// Below this on-screen diameter (points) the system crosshair stays visible, because
/// the ring alone would be too small to aim with.
pub const TINY_TIP: f32 = 12.0;
/// How long the size label stays after the last resize.
const FLASH: Duration = Duration::from_millis(900);

/// The size of a tool, if it has one.
pub fn spec(tool: Tool) -> Option<SizeSpec> {
    use Tool::*;
    let s = |field, min, max, tip| Some(SizeSpec { field, min, max, tip });
    match tool {
        Pencil | LineHighlighter => s(SizeField::Width, 1.0, 100.0, Tip::Brush),
        Eraser => s(SizeField::EraserRadius, 1.0, 500.0, Tip::Brush),
        Line | Arrow | Curve | Polyline | Elbow | Rect | Ellipse | Polygon | Bracket => {
            s(SizeField::Width, 0.0, 100.0, Tip::Dot)
        }
        Step => s(SizeField::StepSize, 8.0, 400.0, Tip::Stamp),
        Text | Callout => s(SizeField::FontSize, 1.0, 200.0, Tip::None),
        Blur | Pixelate => s(SizeField::Strength, 1.0, 80.0, Tip::None),
        Select | Highlighter | Magnifier | Emoji | Sticker | Image | Cursor | Fill => None,
    }
}

impl SizeSpec {
    /// Fractional values make sense (thin pens); the others are whole numbers.
    pub fn decimals(&self) -> bool {
        self.field == SizeField::Width
    }

    /// Smallest size the keys and the wheel go down to (the slider and the value field
    /// may go to `min`, e.g. width 0 = no border).
    pub fn gesture_min(&self) -> f32 {
        self.min.max(1.0)
    }

    /// Diameter of the tip in image pixels.
    pub fn diameter(&self, value: f32) -> f32 {
        match self.field {
            SizeField::EraserRadius => value * 2.0,
            _ => value,
        }
    }

    /// Round to what the value field shows: halves below 10 for widths, whole numbers
    /// otherwise; clamped to the limits.
    pub fn snap(&self, v: f32) -> f32 {
        if !v.is_finite() {
            return self.min;
        }
        let v = if self.decimals() && v < 10.0 { (v * 2.0).round() / 2.0 } else { v.round() };
        v.clamp(self.min, self.max)
    }

    /// Slider position (0..=1) of a value.
    pub fn slider_pos(&self, v: f32) -> f32 {
        let span = (self.max - self.min).max(f32::EPSILON);
        ((v - self.min) / span).clamp(0.0, 1.0).powf(1.0 / SLIDER_GAMMA)
    }

    /// Value at a slider position (0..=1).
    pub fn value_at(&self, t: f32) -> f32 {
        let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 0.0 };
        self.snap(self.min + (self.max - self.min) * t.powf(SLIDER_GAMMA))
    }

    /// `steps` presses of `]` (positive) or `[` (negative): about 15 % each, at least one
    /// whole unit, on whole numbers.
    pub fn step(&self, v: f32, steps: i32) -> f32 {
        let (lo, hi) = (self.gesture_min(), self.max);
        let mut v = if v.is_finite() { v } else { lo };
        for _ in 0..steps.unsigned_abs().min(64) {
            v = if steps > 0 {
                (v * STEP_FACTOR).round().max(v.floor() + 1.0)
            } else {
                (v / STEP_FACTOR).round().min(v.ceil() - 1.0)
            };
            v = v.clamp(lo, hi);
        }
        v.clamp(lo.min(v), hi)
    }

    /// `"12 px"` / `"18 pt"`.
    pub fn label(&self, v: f32) -> String {
        let n = format_value(v);
        let key = if self.field == SizeField::FontSize { "draw-size-pt" } else { "draw-size-px" };
        tf(key, &[("value", &n)])
    }
}

/// `12`, `2.5`.
pub fn format_value(v: f32) -> String {
    let r = (v * 10.0).round() / 10.0;
    if (r - r.round()).abs() < 0.05 {
        format!("{}", r.round() as i64)
    } else {
        format!("{r:.1}")
    }
}

/// On-screen diameter (points) of a tip `diameter` image pixels wide at canvas scale
/// `scale` (points per image pixel).
pub fn screen_diameter(diameter: f32, scale: f32) -> f32 {
    if diameter.is_finite() && scale.is_finite() {
        (diameter * scale).max(0.0)
    } else {
        0.0
    }
}

/// Hide the system cursor: the ring is big enough to aim with.
pub fn hides_cursor(screen_diameter: f32) -> bool {
    screen_diameter >= TINY_TIP
}

pub fn get(ts: &ToolStyle, f: SizeField) -> f32 {
    match f {
        SizeField::Width => ts.style.width,
        SizeField::EraserRadius => ts.options.eraser_radius,
        SizeField::StepSize => ts.options.step_size,
        SizeField::FontSize => ts.style.font.size,
        SizeField::Strength => ts.options.blur_strength,
    }
}

pub fn set(ts: &mut ToolStyle, f: SizeField, v: f32) {
    match f {
        SizeField::Width => ts.style.width = v,
        SizeField::EraserRadius => ts.options.eraser_radius = v,
        SizeField::StepSize => ts.options.step_size = v,
        SizeField::FontSize => ts.style.font.size = v,
        SizeField::Strength => ts.options.blur_strength = v,
    }
}

// ---------------------------------------------------------------------------------- feedback

/// The size label shown near the pointer for a moment after a resize.
#[derive(Debug, Clone)]
pub struct Flash {
    text: String,
    until: Instant,
}

impl Flash {
    pub fn new(text: String) -> Self {
        Self { text, until: Instant::now() + FLASH }
    }

    /// The label while it is still due; asks for a repaint when it expires.
    pub fn text(&self, ctx: &egui::Context) -> Option<&str> {
        let left = self.until.checked_duration_since(Instant::now())?;
        ctx.request_repaint_after(left + Duration::from_millis(20));
        Some(&self.text)
    }
}

const RING_DARK: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 150);

/// A ring readable on any background: a white line between two dark ones.
pub fn paint_ring(painter: &egui::Painter, c: Pos2, radius: f32) {
    let r = radius.max(1.0);
    painter.circle_stroke(c, r, Stroke::new(3.0_f32, RING_DARK));
    painter.circle_stroke(c, r, Stroke::new(1.0_f32, Color32::WHITE));
}

/// The same for a square tip.
pub fn paint_square(painter: &egui::Painter, c: Pos2, half: f32, rounding: f32) {
    let r = egui::Rect::from_center_size(c, vec2(half.max(1.0) * 2.0, half.max(1.0) * 2.0));
    painter.rect_stroke(r, rounding, Stroke::new(3.0_f32, RING_DARK), egui::StrokeKind::Middle);
    painter.rect_stroke(r, rounding, Stroke::new(1.0_f32, Color32::WHITE), egui::StrokeKind::Middle);
}

/// The exact centre of a ring whose system cursor is hidden.
pub fn paint_center(painter: &egui::Painter, c: Pos2) {
    painter.circle_filled(c, 2.0, RING_DARK);
    painter.circle_filled(c, 1.0, Color32::WHITE);
}

/// The size label: a dark pill next to a tip of `radius` points around `c`, kept inside
/// the painter's clip rectangle.
pub fn paint_label(painter: &egui::Painter, c: Pos2, radius: f32, text: &str) {
    let font = egui::FontId::proportional(12.5);
    let galley = painter.layout_no_wrap(text.to_owned(), font, Color32::WHITE);
    let size = galley.size() + vec2(14.0, 8.0);
    let clip = painter.clip_rect().shrink(4.0);
    let pos = label_pos([c.x, c.y], radius, [size.x, size.y], [clip.min.x, clip.min.y, clip.max.x, clip.max.y]);
    let rect = egui::Rect::from_min_size(egui::pos2(pos[0], pos[1]), size);
    painter.rect_filled(rect, 6.0, Color32::from_rgba_unmultiplied(24, 26, 32, 235));
    painter.rect_stroke(rect, 6.0, Stroke::new(1.0_f32, Color32::from_white_alpha(70)), egui::StrokeKind::Inside);
    painter.galley(rect.min + vec2(7.0, 4.0), galley, Color32::WHITE);
}

/// Top-left of a label of `size` beside a tip of `radius` around `c`: to the right of the
/// tip, on the left if that leaves the view, and over the tip when it fills the view.
/// `clip` is `[left, top, right, bottom]`.
pub fn label_pos(c: [f32; 2], radius: f32, size: [f32; 2], clip: [f32; 4]) -> [f32; 2] {
    let gap = 12.0;
    let r = radius.max(0.0);
    let mut x = c[0] + r + gap;
    if x + size[0] > clip[2] {
        x = c[0] - r - gap - size[0];
    }
    if x < clip[0] {
        // The tip is wider than the view: put the label just beside its centre.
        x = c[0] + gap;
    }
    let y = c[1] - size[1] / 2.0;
    let fit = |v: f32, lo: f32, hi: f32| if hi < lo { lo } else { v.clamp(lo, hi) };
    [fit(x, clip[0], clip[2] - size[0]), fit(y, clip[1], clip[3] - size[1])]
}

// ---------------------------------------------------------------------------------- widgets

const SLIDER_W: f32 = 112.0;
const SLIDER_H: f32 = 22.0;
const KNOB: f32 = 6.5;

/// The size slider: a wedge-shaped track (thin to thick) with a knob. Drag or click to
/// set the value; returns the response (`changed()` when the value moved).
pub fn slider(ui: &mut egui::Ui, value: &mut f32, spec: &SizeSpec) -> egui::Response {
    let (rect, mut resp) = ui.allocate_exact_size(vec2(SLIDER_W, SLIDER_H), Sense::click_and_drag());
    let (x0, x1) = (rect.left() + KNOB, rect.right() - KNOB);
    if resp.dragged() || resp.clicked() || resp.drag_started() {
        if let Some(p) = resp.interact_pointer_pos() {
            let v = spec.value_at((p.x - x0) / (x1 - x0));
            if v != *value {
                *value = v;
                resp.mark_changed();
            }
        }
    }
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let v = ui.visuals();
        let hot = resp.hovered() || resp.dragged();
        let t = spec.slider_pos(*value);
        let x = x0 + (x1 - x0) * t;
        let y = rect.center().y;
        let (thin, thick) = (1.0, 5.5);
        let wedge = |from: f32, to: f32, color: Color32| {
            if to <= from {
                return;
            }
            let h = |x: f32| thin + (thick - thin) * ((x - x0) / (x1 - x0)).clamp(0.0, 1.0);
            let pts = vec![
                egui::pos2(from, y - h(from)),
                egui::pos2(to, y - h(to)),
                egui::pos2(to, y + h(to)),
                egui::pos2(from, y + h(from)),
            ];
            p.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
        };
        let rail = v.widgets.noninteractive.bg_stroke.color;
        wedge(x0, x1, if v.dark_mode { rail.gamma_multiply(1.6) } else { rail });
        wedge(x0, x, super::super::style::ACCENT);
        let knob = if hot { KNOB + 0.5 } else { KNOB };
        p.circle_filled(egui::pos2(x, y), knob, Color32::WHITE);
        p.circle_stroke(
            egui::pos2(x, y),
            knob,
            Stroke::new(if hot { 2.0_f32 } else { 1.5 }, super::super::style::ACCENT),
        );
    }
    resp
}

/// A dot as large as the tip (1 image pixel = 1 point), next to the slider. Tips larger
/// than the box are shown as the largest dot that fits inside an accent ring ("more than
/// fits here").
pub fn preview_dot(ui: &mut egui::Ui, diameter: f32, color: Color32) -> egui::Response {
    let side = 24.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(side, side), Sense::hover());
    if ui.is_rect_visible(rect) {
        let p = ui.painter_at(rect);
        let v = ui.visuals();
        p.rect_filled(rect, 5.0, if v.dark_mode { Color32::from_gray(58) } else { Color32::from_gray(236) });
        let fits = side / 2.0 - 2.0;
        let r = (diameter / 2.0).clamp(0.5, fits);
        let c = rect.center();
        // Keep the dot visible on the box whatever its colour.
        let lum = (color.r() as u32 * 3 + color.g() as u32 * 6 + color.b() as u32) / 10;
        let edge = if lum > 150 { Color32::from_black_alpha(110) } else { Color32::from_white_alpha(120) };
        p.circle_filled(c, r, color);
        if diameter / 2.0 <= fits {
            p.circle_stroke(c, r + 0.5, Stroke::new(1.0_f32, edge));
        } else {
            p.circle_stroke(c, r + 1.0, Stroke::new(1.5_f32, super::super::style::ACCENT));
        }
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pencil() -> SizeSpec {
        spec(Tool::Pencil).expect("pencil has a size")
    }

    #[test]
    fn every_brush_tool_has_a_size_and_others_do_not() {
        for t in [Tool::Pencil, Tool::LineHighlighter, Tool::Eraser] {
            assert_eq!(spec(t).map(|s| s.tip), Some(Tip::Brush), "{t:?}");
        }
        for t in [Tool::Line, Tool::Arrow, Tool::Curve, Tool::Polyline, Tool::Elbow, Tool::Rect, Tool::Ellipse] {
            assert_eq!(spec(t).map(|s| (s.field, s.tip)), Some((SizeField::Width, Tip::Dot)), "{t:?}");
        }
        assert_eq!(spec(Tool::Step).map(|s| s.tip), Some(Tip::Stamp));
        assert_eq!(spec(Tool::Text).map(|s| s.field), Some(SizeField::FontSize));
        assert_eq!(spec(Tool::Pixelate).map(|s| s.field), Some(SizeField::Strength));
        for t in [Tool::Select, Tool::Fill, Tool::Emoji, Tool::Image] {
            assert!(spec(t).is_none(), "{t:?}");
        }
        for t in Tool::ALL {
            if let Some(s) = spec(t) {
                assert!(s.min < s.max && s.gesture_min() >= 1.0, "{t:?}");
            }
        }
    }

    #[test]
    fn slider_ends_and_roundtrip() {
        for t in Tool::ALL {
            let Some(s) = spec(t) else { continue };
            assert_eq!(s.value_at(0.0), s.min, "{t:?}");
            assert_eq!(s.value_at(1.0), s.max, "{t:?}");
            assert_eq!(s.value_at(-3.0), s.min);
            assert_eq!(s.value_at(7.0), s.max);
            assert_eq!(s.value_at(f32::NAN), s.min);
            assert_eq!(s.slider_pos(s.min - 5.0), 0.0);
            assert_eq!(s.slider_pos(s.max + 5.0), 1.0);
            // Every value the slider can produce maps back to itself.
            let mut prev = s.min;
            for i in 0..=400 {
                let v = s.value_at(i as f32 / 400.0);
                assert!(v >= prev, "{t:?}: slider is not monotonic at {i}");
                assert_eq!(s.value_at(s.slider_pos(v)), v, "{t:?}: {v} does not round-trip");
                prev = v;
            }
        }
    }

    #[test]
    fn slider_gives_small_sizes_most_of_the_track() {
        let s = pencil();
        // Half of the track covers the first quarter of the range.
        assert_eq!(s.value_at(0.5), 26.0);
        assert!(s.value_at(0.2) <= 5.0);
        // One point of a 100 pt track near the thin end moves less than one pixel.
        assert!(s.value_at(0.21) - s.value_at(0.20) <= 0.5);
        // The eraser reaches large radii without typing.
        let e = spec(Tool::Eraser).expect("eraser");
        assert_eq!(e.value_at(1.0), 500.0);
        assert!(e.value_at(0.3) < 50.0);
    }

    #[test]
    fn snap_rounds_like_the_value_field() {
        let s = pencil();
        assert_eq!(s.snap(2.3), 2.5);
        assert_eq!(s.snap(2.2), 2.0);
        assert_eq!(s.snap(12.4), 12.0);
        assert_eq!(s.snap(0.1), 1.0);
        assert_eq!(s.snap(1e9), 100.0);
        assert_eq!(s.snap(f32::NAN), 1.0);
        let e = spec(Tool::Eraser).expect("eraser");
        assert_eq!(e.snap(2.4), 2.0, "whole numbers only");
        assert_eq!(spec(Tool::Rect).expect("rect").snap(0.2), 0.0, "a border can be switched off");
    }

    #[test]
    fn steps_grow_and_shrink_and_stop_at_the_limits() {
        let s = pencil();
        assert_eq!(s.step(1.0, 1), 2.0);
        assert_eq!(s.step(2.0, 1), 3.0);
        assert_eq!(s.step(2.5, 1), 3.0);
        assert_eq!(s.step(2.5, -1), 2.0);
        assert_eq!(s.step(20.0, 1), 23.0);
        assert_eq!(s.step(20.0, -1), 17.0);
        assert_eq!(s.step(1.0, -1), 1.0);
        assert_eq!(s.step(100.0, 1), 100.0);
        assert_eq!(s.step(99.0, 5), 100.0);
        assert_eq!(s.step(10.0, 0), 10.0);
        assert_eq!(s.step(f32::NAN, 1), 2.0);
        // Always moves until the limit, in both directions.
        let (mut v, mut n) = (1.0, 0);
        while v < 100.0 {
            let next = s.step(v, 1);
            assert!(next > v);
            v = next;
            n += 1;
        }
        assert!(n < 40, "1 → 100 px took {n} presses");
        while v > 1.0 {
            let next = s.step(v, -1);
            assert!(next < v);
            v = next;
        }
        // Keys never switch a shape's stroke off, even though the slider can.
        let r = spec(Tool::Rect).expect("rect");
        assert_eq!(r.step(1.0, -1), 1.0);
        assert_eq!(r.step(0.0, 1), 1.0);
        assert_eq!(spec(Tool::Step).expect("step").step(8.0, -1), 8.0);
    }

    #[test]
    fn screen_size_follows_zoom() {
        let s = pencil();
        assert_eq!(screen_diameter(s.diameter(12.0), 1.0), 12.0);
        assert_eq!(screen_diameter(s.diameter(12.0), 4.0), 48.0);
        assert_eq!(screen_diameter(s.diameter(12.0), 0.25), 3.0);
        // The eraser stores a radius.
        let e = spec(Tool::Eraser).expect("eraser");
        assert_eq!(screen_diameter(e.diameter(10.0), 2.0), 40.0);
        assert_eq!(screen_diameter(f32::NAN, 1.0), 0.0);
        assert_eq!(screen_diameter(-4.0, 1.0), 0.0);
        // Tiny tips keep the system crosshair.
        assert!(!hides_cursor(screen_diameter(2.0, 1.0)));
        assert!(hides_cursor(screen_diameter(2.0, 8.0)));
        assert!(hides_cursor(screen_diameter(20.0, 1.0)));
    }

    #[test]
    fn get_and_set_reach_the_right_field() {
        let mut ts = ToolStyle::for_tool(Tool::Eraser);
        for f in
            [SizeField::Width, SizeField::EraserRadius, SizeField::StepSize, SizeField::FontSize, SizeField::Strength]
        {
            set(&mut ts, f, 41.0);
            assert_eq!(get(&ts, f), 41.0);
            set(&mut ts, f, 7.0);
        }
        assert_eq!(ts.options.eraser_radius, 7.0);
        assert_eq!(ts.style.width, 7.0);
    }

    #[test]
    fn labels() {
        assert_eq!(format_value(12.0), "12");
        assert_eq!(format_value(2.5), "2.5");
        assert_eq!(format_value(11.96), "12");
        assert_eq!(pencil().label(12.0), "12 px");
        assert_eq!(spec(Tool::Text).expect("text").label(18.0), "18 pt");
    }

    #[test]
    fn label_stays_in_view() {
        let clip = [0.0, 0.0, 400.0, 300.0];
        let size = [50.0, 22.0];
        // Normally right of the tip, vertically centred.
        assert_eq!(label_pos([100.0, 100.0], 10.0, size, clip), [122.0, 89.0]);
        // Near the right edge: on the left.
        assert_eq!(label_pos([380.0, 100.0], 10.0, size, clip), [308.0, 89.0]);
        // A tip larger than the view: beside its centre, inside the view.
        let p = label_pos([200.0, 150.0], 900.0, size, clip);
        assert_eq!(p, [212.0, 139.0]);
        // Never outside, even in a view smaller than the label.
        let p = label_pos([5.0, 2.0], 3.0, size, [0.0, 0.0, 30.0, 10.0]);
        assert_eq!(p, [0.0, 0.0]);
    }
}
