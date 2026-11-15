//! Annotation object model. Everything here is plain data with serde derives; geometry is in
//! image pixels (f32, origin at the top-left of the base image, y down).

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::sync::Arc;

/// Stable object identifier, unique within a document. See [`super::next_id`].
pub type ObjectId = u64;

// ---------------------------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------------------------

/// A point in image pixels.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Pt {
    pub x: f32,
    pub y: f32,
}

impl Pt {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
    pub fn len(self) -> f32 {
        self.x.hypot(self.y)
    }
    pub fn dist(self, o: Pt) -> f32 {
        (self - o).len()
    }
    pub fn dot(self, o: Pt) -> f32 {
        self.x * o.x + self.y * o.y
    }
    pub fn cross(self, o: Pt) -> f32 {
        self.x * o.y - self.y * o.x
    }
    /// Unit vector (or zero for a zero vector).
    pub fn norm(self) -> Pt {
        let l = self.len();
        if l > 1e-6 {
            Pt::new(self.x / l, self.y / l)
        } else {
            Pt::default()
        }
    }
    /// Perpendicular (rotated +90° in y-down coordinates: right-hand side of the direction).
    pub fn perp(self) -> Pt {
        Pt::new(-self.y, self.x)
    }
    pub fn lerp(self, o: Pt, t: f32) -> Pt {
        Pt::new(self.x + (o.x - self.x) * t, self.y + (o.y - self.y) * t)
    }
    /// Rotate by `deg` degrees (clockwise on screen) around `c`.
    pub fn rotate_around(self, c: Pt, deg: f32) -> Pt {
        if deg == 0.0 {
            return self;
        }
        let (s, co) = deg.to_radians().sin_cos();
        let d = self - c;
        Pt::new(c.x + d.x * co - d.y * s, c.y + d.x * s + d.y * co)
    }
    /// Angle of this vector in degrees (0 = +x, 90 = +y/down).
    pub fn angle_deg(self) -> f32 {
        self.y.atan2(self.x).to_degrees()
    }
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
}

impl std::ops::Add for Pt {
    type Output = Pt;
    fn add(self, o: Pt) -> Pt {
        Pt::new(self.x + o.x, self.y + o.y)
    }
}
impl std::ops::Sub for Pt {
    type Output = Pt;
    fn sub(self, o: Pt) -> Pt {
        Pt::new(self.x - o.x, self.y - o.y)
    }
}
impl std::ops::Mul<f32> for Pt {
    type Output = Pt;
    fn mul(self, k: f32) -> Pt {
        Pt::new(self.x * k, self.y * k)
    }
}
impl std::ops::Neg for Pt {
    type Output = Pt;
    fn neg(self) -> Pt {
        Pt::new(-self.x, -self.y)
    }
}
impl std::ops::AddAssign for Pt {
    fn add_assign(&mut self, o: Pt) {
        self.x += o.x;
        self.y += o.y;
    }
}

/// Axis-aligned rectangle in image pixels. Constructors normalize so `w`/`h` are never negative.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl RectF {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    /// Normalized rectangle spanning two corners.
    pub fn from_points(a: Pt, b: Pt) -> Self {
        Self::from_ltrb(a.x.min(b.x), a.y.min(b.y), a.x.max(b.x), a.y.max(b.y))
    }
    pub fn from_ltrb(l: f32, t: f32, r: f32, b: f32) -> Self {
        Self::new(l.min(r), t.min(b), (r - l).abs(), (b - t).abs())
    }
    pub fn from_center(c: Pt, w: f32, h: f32) -> Self {
        Self::new(c.x - w / 2.0, c.y - h / 2.0, w.abs(), h.abs())
    }
    /// Bounding box of points (empty default rect for no points).
    pub fn bounding(points: &[Pt]) -> Self {
        let mut it = points.iter();
        let Some(first) = it.next() else { return Self::default() };
        let (mut l, mut t, mut r, mut b) = (first.x, first.y, first.x, first.y);
        for p in it {
            l = l.min(p.x);
            t = t.min(p.y);
            r = r.max(p.x);
            b = b.max(p.y);
        }
        Self::from_ltrb(l, t, r, b)
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn center(&self) -> Pt {
        Pt::new(self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
    pub fn tl(&self) -> Pt {
        Pt::new(self.x, self.y)
    }
    pub fn br(&self) -> Pt {
        Pt::new(self.right(), self.bottom())
    }
    pub fn contains(&self, p: Pt) -> bool {
        p.x >= self.x && p.y >= self.y && p.x <= self.right() && p.y <= self.bottom()
    }
    pub fn inflate(&self, d: f32) -> RectF {
        RectF::new(self.x - d, self.y - d, (self.w + 2.0 * d).max(0.0), (self.h + 2.0 * d).max(0.0))
    }
    pub fn translate(&self, d: Pt) -> RectF {
        RectF::new(self.x + d.x, self.y + d.y, self.w, self.h)
    }
    pub fn union(&self, o: &RectF) -> RectF {
        RectF::from_ltrb(self.x.min(o.x), self.y.min(o.y), self.right().max(o.right()), self.bottom().max(o.bottom()))
    }
    pub fn intersects(&self, o: &RectF) -> bool {
        self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }
    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }
    /// Corners in order top-left, top-right, bottom-right, bottom-left.
    pub fn corners(&self) -> [Pt; 4] {
        [self.tl(), Pt::new(self.right(), self.y), self.br(), Pt::new(self.x, self.bottom())]
    }
    /// Map a point from this rect to `to` (affine, per axis). Degenerate axes map to the target origin.
    pub fn map_to(&self, p: Pt, to: &RectF) -> Pt {
        let sx = if self.w.abs() > 1e-6 { to.w / self.w } else { 1.0 };
        let sy = if self.h.abs() > 1e-6 { to.h / self.h } else { 1.0 };
        Pt::new(to.x + (p.x - self.x) * sx, to.y + (p.y - self.y) * sy)
    }
    pub fn is_finite(&self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.w.is_finite() && self.h.is_finite()
    }
}

// ---------------------------------------------------------------------------------------------
// Color
// ---------------------------------------------------------------------------------------------

/// Straight-alpha sRGB color. Serialized as `"#RRGGBBAA"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const TRANSPARENT: Color = Color::rgba(0, 0, 0, 0);
    pub const BLACK: Color = Color::rgb(0, 0, 0);
    pub const WHITE: Color = Color::rgb(255, 255, 255);
    pub const RED: Color = Color::rgb(0xE5, 0x39, 0x35);
    pub const ORANGE: Color = Color::rgb(0xFB, 0x8C, 0x00);
    pub const YELLOW: Color = Color::rgb(0xFF, 0xEB, 0x3B);
    pub const GREEN: Color = Color::rgb(0x43, 0xA0, 0x47);
    pub const BLUE: Color = Color::rgb(0x1E, 0x88, 0xE5);
    pub const PURPLE: Color = Color::rgb(0x8E, 0x24, 0xAA);

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
    pub fn with_alpha(self, a: u8) -> Self {
        Self { a, ..self }
    }
    /// Multiply alpha by `k` (0..=1).
    pub fn fade(self, k: f32) -> Self {
        Self { a: (self.a as f32 * k.clamp(0.0, 1.0)).round() as u8, ..self }
    }
    pub fn is_visible(self) -> bool {
        self.a > 0
    }
    /// Mix toward `o` by `t` (0 = self, 1 = o), alpha included.
    pub fn mix(self, o: Color, t: f32) -> Color {
        let t = t.clamp(0.0, 1.0);
        let m = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Color::rgba(m(self.r, o.r), m(self.g, o.g), m(self.b, o.b), m(self.a, o.a))
    }
    pub fn lighten(self, t: f32) -> Color {
        self.mix(Color::WHITE.with_alpha(self.a), t)
    }
    pub fn darken(self, t: f32) -> Color {
        self.mix(Color::BLACK.with_alpha(self.a), t)
    }
    /// Relative luminance in 0..=1 (fast approximation, ignores alpha).
    pub fn luma(self) -> f32 {
        (0.2126 * self.r as f32 + 0.7152 * self.g as f32 + 0.0722 * self.b as f32) / 255.0
    }
    /// Black or white, whichever reads better on this color.
    pub fn contrast(self) -> Color {
        if self.luma() > 0.6 {
            Color::rgb(0x1A, 0x1A, 0x1A)
        } else {
            Color::WHITE
        }
    }
    pub fn to_hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}{:02X}", self.r, self.g, self.b, self.a)
    }
    /// Parse `#RGB`, `#RRGGBB` or `#RRGGBBAA` (the `#` is optional).
    pub fn from_hex(s: &str) -> Option<Color> {
        let h = s.trim().trim_start_matches('#');
        let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
        match h.len() {
            3 => {
                let n = |i: usize| u8::from_str_radix(h.get(i..i + 1)?, 16).ok().map(|v| v * 17);
                Some(Color::rgb(n(0)?, n(1)?, n(2)?))
            }
            6 => Some(Color::rgb(byte(0)?, byte(2)?, byte(4)?)),
            8 => Some(Color::rgba(byte(0)?, byte(2)?, byte(4)?, byte(6)?)),
            _ => None,
        }
    }
    pub fn to_skia(self) -> tiny_skia::Color {
        tiny_skia::Color::from_rgba8(self.r, self.g, self.b, self.a)
    }
}

impl Serialize for Color {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Color::from_hex(&s).ok_or_else(|| serde::de::Error::custom(format!("invalid color \"{s}\"")))
    }
}

// ---------------------------------------------------------------------------------------------
// Style (shared by every object; each kind documents which fields it uses)
// ---------------------------------------------------------------------------------------------

/// Visual attributes common to all objects. The convention is:
/// `color` + `width` = stroke / line / border (width 0 = no border), `fill` = interior
/// (alpha 0 = none), `font.color` = text, `opacity` = whole-object opacity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Style {
    pub color: Color,
    pub fill: Color,
    pub width: f32,
    pub opacity: f32,
    pub dash: Dash,
    pub shadow: Option<Shadow>,
    /// Outline effect around lines and text (drawn behind them).
    pub outline: Option<Outline>,
    pub font: Font,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            color: Color::RED,
            fill: Color::TRANSPARENT,
            width: 4.0,
            opacity: 1.0,
            dash: Dash::Solid,
            shadow: None,
            outline: None,
            font: Font::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Shadow {
    pub dx: f32,
    pub dy: f32,
    /// Blur radius in pixels (0 = hard shadow).
    pub blur: f32,
    pub color: Color,
}

impl Default for Shadow {
    fn default() -> Self {
        Self { dx: 3.0, dy: 4.0, blur: 6.0, color: Color::rgba(0, 0, 0, 110) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Outline {
    pub color: Color,
    /// Thickness of the outline outside the line or glyph, in pixels.
    pub width: f32,
}

impl Default for Outline {
    fn default() -> Self {
        Self { color: Color::WHITE, width: 2.0 }
    }
}

#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dash {
    #[default]
    Solid,
    Dash,
    Dot,
    DashDot,
    LongDash,
}

impl Dash {
    pub const ALL: [Dash; 5] = [Dash::Solid, Dash::Dash, Dash::Dot, Dash::DashDot, Dash::LongDash];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Font {
    pub family: String,
    /// Size in points at 96 DPI (1 pt = 4/3 px). Clamped to 1..=200 when rendering.
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub align: TextAlign,
    pub color: Color,
}

impl Font {
    pub const MAX_SIZE: f32 = 200.0;
    /// Em size in pixels.
    pub fn px(&self) -> f32 {
        self.size.clamp(1.0, Self::MAX_SIZE) * 96.0 / 72.0
    }
}

impl Default for Font {
    fn default() -> Self {
        Self {
            family: "Segoe UI".into(),
            size: 16.0,
            bold: false,
            italic: false,
            underline: false,
            align: TextAlign::Left,
            color: Color::rgb(0x1A, 0x1A, 0x1A),
        }
    }
}

/// Optional visibility window for video annotations (milliseconds from the start).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Timing {
    pub start_ms: u64,
    pub end_ms: u64,
}

impl Timing {
    pub fn contains(&self, t: u64) -> bool {
        t >= self.start_ms && t < self.end_ms.max(self.start_ms)
    }
}

// ---------------------------------------------------------------------------------------------
// Object
// ---------------------------------------------------------------------------------------------

/// One annotation. Z-order is the position in the object list (later = on top).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Object {
    pub id: ObjectId,
    pub kind: Kind,
    #[serde(default)]
    pub style: Style,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing: Option<Timing>,
}

impl Object {
    pub fn new(id: ObjectId, kind: Kind, style: Style) -> Self {
        Self { id, kind, style, timing: None }
    }
    /// True for raster tools that modify the base image (blur, eraser, fill, pencil).
    pub fn is_raster(&self) -> bool {
        self.kind.is_raster()
    }
    /// Visible at `time_ms` (objects without timing are always visible).
    pub fn visible_at(&self, time_ms: Option<u64>) -> bool {
        match (self.timing, time_ms) {
            (Some(t), Some(now)) => t.contains(now),
            _ => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Kind {
    Text(Text),
    Line(Line),
    Shape(Shape),
    Highlight(Highlight),
    Magnifier(Magnifier),
    Step(Step),
    Emoji(Emoji),
    Sticker(Sticker),
    Image(Picture),
    Cursor(Cursor),
    Blur(Blur),
    Eraser(Eraser),
    Fill(Fill),
    Pencil(Pencil),
}

impl Kind {
    pub fn is_raster(&self) -> bool {
        matches!(self, Kind::Blur(_) | Kind::Eraser(_) | Kind::Fill(_) | Kind::Pencil(_))
    }
    /// Short stable name ("text", "line", ...), same as the serde tag.
    pub fn name(&self) -> &'static str {
        match self {
            Kind::Text(_) => "text",
            Kind::Line(_) => "line",
            Kind::Shape(_) => "shape",
            Kind::Highlight(_) => "highlight",
            Kind::Magnifier(_) => "magnifier",
            Kind::Step(_) => "step",
            Kind::Emoji(_) => "emoji",
            Kind::Sticker(_) => "sticker",
            Kind::Image(_) => "image",
            Kind::Cursor(_) => "cursor",
            Kind::Blur(_) => "blur",
            Kind::Eraser(_) => "eraser",
            Kind::Fill(_) => "fill",
            Kind::Pencil(_) => "pencil",
        }
    }
}

/// A rectangle with rotation (degrees, clockwise, around the centre) and mirroring.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Frame {
    pub rect: RectF,
    pub rotation: f32,
    pub flip_h: bool,
    pub flip_v: bool,
}

impl Frame {
    pub fn new(rect: RectF) -> Self {
        Self { rect, ..Default::default() }
    }
}

// ----- Text -----

/// Text object. Uses `font` for the glyphs, `outline` for a glyph outline, `shadow`,
/// and for the box: `fill` (background) and `color` + `width` (border).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Text {
    /// Box. With `wrap`, text wraps at the box width and the height grows to fit;
    /// otherwise the box grows in both directions to fit (the rect is a minimum size).
    pub rect: RectF,
    pub text: String,
    pub flow: TextFlow,
    pub wrap: bool,
    pub boxed: TextBox,
    /// Inner padding between the box and the text.
    pub padding: f32,
    /// Corner radius for [`TextBox::Rounded`].
    pub radius: f32,
    /// Callout pointer.
    pub tail: Option<Tail>,
}

impl Default for Text {
    fn default() -> Self {
        Self {
            rect: RectF::new(0.0, 0.0, 200.0, 40.0),
            text: String::new(),
            flow: TextFlow::Straight,
            wrap: true,
            boxed: TextBox::None,
            padding: 8.0,
            radius: 10.0,
            tail: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum TextFlow {
    #[default]
    Straight,
    /// Single line along a circular arc across the box. `bend` in -1..=1: the arc height as a
    /// fraction of half the box width (positive bulges upward, 1 = semicircle).
    Arc { bend: f32 },
    /// Single line along a polyline (Catmull-Rom smoothed when `smooth`). The box is unused.
    Path { points: Vec<Pt>, smooth: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextBox {
    #[default]
    None,
    Rect,
    Rounded,
    Oval,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Tail {
    pub tip: Pt,
    /// Width of the pointer where it meets the box.
    pub width: f32,
}

// ----- Lines -----

/// Lines and arrows. Uses `color`, `width`, `dash`, `outline`, `shadow`, `opacity`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Line {
    pub kind: LineKind,
    /// Meaning depends on `kind`: Straight/Elbow `[a, b]`, Quad `[a, c, b]`,
    /// Cubic `[a, c1, c2, b]`, Polyline/Smooth `[p0, p1, ...]`.
    pub points: Vec<Pt>,
    pub start: ArrowHead,
    pub end: ArrowHead,
    /// Arrowhead size multiplier (1 = default, scales with the line width).
    pub head_size: f32,
    pub decor: LineDecor,
    /// Secondary color: gradient end, neon core, railroad ties.
    pub color2: Color,
}

impl Default for Line {
    fn default() -> Self {
        Self {
            kind: LineKind::Straight,
            points: vec![Pt::default(), Pt::new(100.0, 0.0)],
            start: ArrowHead::None,
            end: ArrowHead::None,
            head_size: 1.0,
            decor: LineDecor::Plain,
            color2: Color::BLUE,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineKind {
    #[default]
    Straight,
    Quad,
    Cubic,
    Polyline,
    /// Polyline through the points, smoothed with Catmull-Rom curves.
    Smooth,
    /// L-line: horizontal first, then vertical.
    ElbowH,
    /// L-line: vertical first, then horizontal.
    ElbowV,
}

impl LineKind {
    pub const ALL: [LineKind; 7] = [
        LineKind::Straight,
        LineKind::Quad,
        LineKind::Cubic,
        LineKind::Polyline,
        LineKind::Smooth,
        LineKind::ElbowH,
        LineKind::ElbowV,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArrowHead {
    #[default]
    None,
    /// Filled triangle.
    Triangle,
    /// Open V (two strokes).
    Open,
    /// Triangle with a notched back.
    Stealth,
    Diamond,
    Circle,
    /// Perpendicular bar (dimension lines).
    Bar,
    /// Swept, curved barbs.
    Fancy,
    /// Bevelled head with a lit and a shaded half.
    ThreeD,
}

impl ArrowHead {
    pub const ALL: [ArrowHead; 9] = [
        ArrowHead::None,
        ArrowHead::Triangle,
        ArrowHead::Open,
        ArrowHead::Stealth,
        ArrowHead::Diamond,
        ArrowHead::Circle,
        ArrowHead::Bar,
        ArrowHead::Fancy,
        ArrowHead::ThreeD,
    ];
}

/// Decorative line styles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineDecor {
    #[default]
    Plain,
    /// Two parallel rails.
    Double,
    /// Color fades from `color` to `color2` along the line.
    Gradient,
    /// Glowing tube with a bright core.
    Neon,
    /// Grainy, rough chalk stroke.
    Chalk,
    /// Row of shiny beads.
    Beads,
    /// Thick at the start, thin at the end.
    Tapered,
    Wavy,
    Zigzag,
    /// Two rails with cross ties.
    Railroad,
    /// Calligraphic brush with varying width.
    Brush,
}

impl LineDecor {
    pub const ALL: [LineDecor; 11] = [
        LineDecor::Plain,
        LineDecor::Double,
        LineDecor::Gradient,
        LineDecor::Neon,
        LineDecor::Chalk,
        LineDecor::Beads,
        LineDecor::Tapered,
        LineDecor::Wavy,
        LineDecor::Zigzag,
        LineDecor::Railroad,
        LineDecor::Brush,
    ];
}

// ----- Shapes -----

/// Rectangle, ellipse, polygon or bracket. Uses `fill`, `color` + `width` (border), `dash`,
/// `shadow`, `opacity`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Shape {
    pub kind: ShapeKind,
    pub rect: RectF,
    /// Corner radius for rectangles.
    pub radius: f32,
    /// Polygon vertices (Polygon only; the rect is derived from them).
    pub points: Vec<Pt>,
    /// Which way a bracket's point faces (`Left` = "{").
    pub facing: Side,
}

impl Default for Shape {
    fn default() -> Self {
        Self {
            kind: ShapeKind::Rect,
            rect: RectF::new(0.0, 0.0, 100.0, 60.0),
            radius: 0.0,
            points: Vec::new(),
            facing: Side::Left,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShapeKind {
    #[default]
    Rect,
    Ellipse,
    Polygon,
    CurlyBracket,
    SquareBracket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    #[default]
    Left,
    Right,
    Top,
    Bottom,
}

// ----- Highlighter -----

/// Highlighter. `fill` is the highlight color (multiplied onto what is below), `color` +
/// `width` an optional border (for `Line`, `width` is the pen thickness).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Highlight {
    pub shape: HighlightShape,
    pub rect: RectF,
    /// Pen path (Line only).
    pub points: Vec<Pt>,
    pub radius: f32,
    /// Inner padding between the border and the highlighted area.
    pub indent: f32,
    /// Blur radius applied to everything outside the shape (0 = off).
    pub blur_outside: f32,
    /// Darken everything outside the shape (0..=1, 0 = off).
    pub dim_outside: f32,
}

impl Default for Highlight {
    fn default() -> Self {
        Self {
            shape: HighlightShape::Rect,
            rect: RectF::new(0.0, 0.0, 100.0, 30.0),
            points: Vec::new(),
            radius: 6.0,
            indent: 0.0,
            blur_outside: 0.0,
            dim_outside: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HighlightShape {
    #[default]
    Rect,
    Rounded,
    Oval,
    Line,
}

// ----- Magnifier -----

/// The `source` area of the base image drawn magnified into `target`. Uses `color` + `width`
/// for the border and connector, `shadow`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Magnifier {
    pub source: RectF,
    pub target: RectF,
    pub shape: MagnifierShape,
    pub connector: bool,
}

impl Default for Magnifier {
    fn default() -> Self {
        Self {
            source: RectF::new(0.0, 0.0, 40.0, 40.0),
            target: RectF::new(60.0, 0.0, 120.0, 120.0),
            shape: MagnifierShape::Circle,
            connector: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MagnifierShape {
    #[default]
    Circle,
    Rect,
    Rounded,
}

// ----- Step stamp -----

/// Numbered/lettered step marker. `fill` = badge color, `color` + `width` = ring,
/// `font` = label (its size is ignored; the label is fitted to the badge).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Step {
    pub center: Pt,
    /// Badge diameter in pixels.
    pub size: f32,
    pub value: StepValue,
    pub shape: StepShape,
    /// Teardrop pointer direction in degrees (0 = right, 90 = down).
    pub angle: f32,
}

impl Default for Step {
    fn default() -> Self {
        Self { center: Pt::default(), size: 32.0, value: StepValue::Number(1), shape: StepShape::Circle, angle: 135.0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepValue {
    /// 0..=999.
    Number(u32),
    /// 0 = A, 25 = Z, 26 = AA, ...
    Letter(u32),
}

impl Default for StepValue {
    fn default() -> Self {
        StepValue::Number(1)
    }
}

impl StepValue {
    pub const MAX_NUMBER: u32 = 999;

    pub fn label(&self) -> String {
        match *self {
            StepValue::Number(n) => n.min(Self::MAX_NUMBER).to_string(),
            StepValue::Letter(i) => {
                // Bijective base-26: A..Z, AA..AZ, BA...
                let mut n = i as u64 + 1;
                let mut s = Vec::new();
                while n > 0 {
                    n -= 1;
                    s.push(b'A' + (n % 26) as u8);
                    n /= 26;
                }
                s.reverse();
                String::from_utf8_lossy(&s).into_owned()
            }
        }
    }
    pub fn next(&self) -> StepValue {
        match *self {
            StepValue::Number(n) => StepValue::Number((n + 1).min(Self::MAX_NUMBER)),
            StepValue::Letter(i) => StepValue::Letter(i.saturating_add(1)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepShape {
    #[default]
    Circle,
    Square,
    Teardrop,
}

// ----- Emoji, stickers, images, cursors -----

/// A color emoji (Segoe UI Emoji) fitted into the frame.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Emoji {
    pub frame: Frame,
    pub text: String,
}

/// Built-in vector art: 3D block arrows. `fill` is the base color.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Sticker {
    pub frame: Frame,
    pub shape: StickerShape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StickerShape {
    #[default]
    Arrow,
    DoubleArrow,
    CurvedArrow,
    UTurnArrow,
    CornerArrow,
    CircularArrow,
}

impl StickerShape {
    pub const ALL: [StickerShape; 6] = [
        StickerShape::Arrow,
        StickerShape::DoubleArrow,
        StickerShape::CurvedArrow,
        StickerShape::UTurnArrow,
        StickerShape::CornerArrow,
        StickerShape::CircularArrow,
    ];
}

/// Encoded image bytes (PNG), shared cheaply between undo snapshots. Serialized as base64.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct ImageData(pub Arc<[u8]>);

impl std::fmt::Debug for ImageData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ImageData({} bytes)", self.0.len())
    }
}

impl Serialize for ImageData {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::b64::encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for ImageData {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        let bytes = super::b64::decode(&s).ok_or_else(|| serde::de::Error::custom("invalid base64 image data"))?;
        Ok(ImageData(bytes.into()))
    }
}

/// An inserted image (PNG bytes, decoded and cached on demand).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Picture {
    pub frame: Frame,
    pub png: ImageData,
}

/// Mouse-pointer icon drawn as vector art. `fill` is the click highlight color.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Cursor {
    pub frame: Frame,
    pub shape: CursorShape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorShape {
    #[default]
    Arrow,
    /// Arrow with click ripples at the tip.
    ArrowClick,
    /// Mouse with the left button highlighted.
    LeftClick,
    /// Mouse with the right button highlighted.
    RightClick,
    /// Pointing hand.
    Hand,
}

impl CursorShape {
    pub const ALL: [CursorShape; 5] = [
        CursorShape::Arrow,
        CursorShape::ArrowClick,
        CursorShape::LeftClick,
        CursorShape::RightClick,
        CursorShape::Hand,
    ];
}

// ----- Raster tools (modify the base image; still editable objects) -----

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Blur {
    pub rect: RectF,
    pub ellipse: bool,
    pub mode: BlurMode,
    /// Blur radius or pixel block size, in pixels.
    pub strength: f32,
}

impl Default for Blur {
    fn default() -> Self {
        Self { rect: RectF::new(0.0, 0.0, 80.0, 40.0), ellipse: false, mode: BlurMode::Blur, strength: 8.0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlurMode {
    #[default]
    Blur,
    Pixelate,
}

/// Circular-brush eraser. Erases to transparent, or paints `background` when set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Eraser {
    pub points: Vec<Pt>,
    pub radius: f32,
    pub background: Option<Color>,
}

impl Default for Eraser {
    fn default() -> Self {
        Self { points: Vec::new(), radius: 10.0, background: None }
    }
}

/// Flood fill at `at` with `style.fill` (or to transparent).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Fill {
    pub at: Pt,
    /// Maximum per-channel difference (0..=255) from the seed color.
    pub tolerance: u8,
    pub transparent: bool,
}

impl Default for Fill {
    fn default() -> Self {
        Self { at: Pt::default(), tolerance: 32, transparent: false }
    }
}

/// Freehand hard-pixel stroke with `color` and `width`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Pencil {
    pub points: Vec<Pt>,
}
