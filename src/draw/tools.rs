//! Tools, per-tool options and object creation from a drag gesture. Presets (in
//! `settings::draw`) store a [`Style`] plus [`ToolOptions`] per tool.

use super::edit::next_step_value_as;
use super::model::*;
use serde::{Deserialize, Serialize};

/// Drawing tools offered by the editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tool {
    #[default]
    Select,
    Text,
    Callout,
    Line,
    Arrow,
    Curve,
    Polyline,
    Elbow,
    Rect,
    Ellipse,
    Polygon,
    Bracket,
    Highlighter,
    LineHighlighter,
    Magnifier,
    Step,
    Emoji,
    Sticker,
    Image,
    Cursor,
    Blur,
    Pixelate,
    Eraser,
    Fill,
    Pencil,
}

impl Tool {
    pub const ALL: [Tool; 25] = [
        Tool::Select,
        Tool::Text,
        Tool::Callout,
        Tool::Line,
        Tool::Arrow,
        Tool::Curve,
        Tool::Polyline,
        Tool::Elbow,
        Tool::Rect,
        Tool::Ellipse,
        Tool::Polygon,
        Tool::Bracket,
        Tool::Highlighter,
        Tool::LineHighlighter,
        Tool::Magnifier,
        Tool::Step,
        Tool::Emoji,
        Tool::Sticker,
        Tool::Image,
        Tool::Cursor,
        Tool::Blur,
        Tool::Pixelate,
        Tool::Eraser,
        Tool::Fill,
        Tool::Pencil,
    ];

    /// Stable id (settings keys, locale keys `draw-tool-<id>` / `draw-tip-<id>`).
    pub fn id(self) -> &'static str {
        match self {
            Tool::Select => "select",
            Tool::Text => "text",
            Tool::Callout => "callout",
            Tool::Line => "line",
            Tool::Arrow => "arrow",
            Tool::Curve => "curve",
            Tool::Polyline => "polyline",
            Tool::Elbow => "elbow",
            Tool::Rect => "rect",
            Tool::Ellipse => "ellipse",
            Tool::Polygon => "polygon",
            Tool::Bracket => "bracket",
            Tool::Highlighter => "highlighter",
            Tool::LineHighlighter => "line-highlighter",
            Tool::Magnifier => "magnifier",
            Tool::Step => "step",
            Tool::Emoji => "emoji",
            Tool::Sticker => "sticker",
            Tool::Image => "image",
            Tool::Cursor => "cursor",
            Tool::Blur => "blur",
            Tool::Pixelate => "pixelate",
            Tool::Eraser => "eraser",
            Tool::Fill => "fill",
            Tool::Pencil => "pencil",
        }
    }

    pub fn from_id(s: &str) -> Option<Tool> {
        Tool::ALL.into_iter().find(|t| t.id() == s)
    }

    /// The tool that edits an existing object (used to remember "last style per tool").
    pub fn of(obj: &Object) -> Tool {
        match &obj.kind {
            Kind::Text(t) if t.tail.is_some() => Tool::Callout,
            Kind::Text(_) => Tool::Text,
            Kind::Line(l) => match l.kind {
                LineKind::Quad | LineKind::Cubic | LineKind::Smooth => Tool::Curve,
                LineKind::Polyline => Tool::Polyline,
                LineKind::ElbowH | LineKind::ElbowV => Tool::Elbow,
                LineKind::Straight if l.start != ArrowHead::None || l.end != ArrowHead::None => Tool::Arrow,
                LineKind::Straight => Tool::Line,
            },
            Kind::Shape(s) => match s.kind {
                ShapeKind::Rect => Tool::Rect,
                ShapeKind::Ellipse => Tool::Ellipse,
                ShapeKind::Polygon => Tool::Polygon,
                ShapeKind::CurlyBracket | ShapeKind::SquareBracket => Tool::Bracket,
            },
            Kind::Highlight(h) if h.shape == HighlightShape::Line => Tool::LineHighlighter,
            Kind::Highlight(_) => Tool::Highlighter,
            Kind::Magnifier(_) => Tool::Magnifier,
            Kind::Step(_) => Tool::Step,
            Kind::Emoji(_) => Tool::Emoji,
            Kind::Sticker(_) => Tool::Sticker,
            Kind::Image(_) => Tool::Image,
            Kind::Cursor(_) => Tool::Cursor,
            Kind::Blur(b) if b.mode == BlurMode::Pixelate => Tool::Pixelate,
            Kind::Blur(_) => Tool::Blur,
            Kind::Eraser(_) => Tool::Eraser,
            Kind::Fill(_) => Tool::Fill,
            Kind::Pencil(_) => Tool::Pencil,
        }
    }

    /// Tools whose objects are drawn freehand (the UI appends points while dragging).
    pub fn is_freehand(self) -> bool {
        matches!(self, Tool::Pencil | Tool::Eraser | Tool::LineHighlighter)
    }

    /// Tools whose objects are built click by click (the UI appends points, double-click ends).
    pub fn is_multi_point(self) -> bool {
        matches!(self, Tool::Polyline | Tool::Polygon)
    }
}

/// Text layout mode stored in tool options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowMode {
    #[default]
    Straight,
    Arc,
    Path,
}

/// Non-geometry options of every tool (each tool reads the fields it needs).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolOptions {
    pub line_kind: LineKind,
    pub head_start: ArrowHead,
    pub head_end: ArrowHead,
    pub head_size: f32,
    pub decor: LineDecor,
    pub color2: Color,
    pub text_box: TextBox,
    pub text_flow: FlowMode,
    pub wrap: bool,
    pub padding: f32,
    pub radius: f32,
    pub tail_width: f32,
    pub shape: ShapeKind,
    pub facing: Side,
    pub highlight: HighlightShape,
    pub indent: f32,
    pub blur_outside: f32,
    pub dim_outside: f32,
    pub magnifier: MagnifierShape,
    pub connector: bool,
    pub zoom: f32,
    pub step_shape: StepShape,
    pub step_letters: bool,
    pub step_size: f32,
    pub emoji: String,
    pub sticker: StickerShape,
    pub cursor: CursorShape,
    pub blur_ellipse: bool,
    pub blur_strength: f32,
    pub eraser_radius: f32,
    pub eraser_background: Option<Color>,
    pub fill_tolerance: u8,
    pub fill_transparent: bool,
}

impl Default for ToolOptions {
    fn default() -> Self {
        Self {
            line_kind: LineKind::Straight,
            head_start: ArrowHead::None,
            head_end: ArrowHead::Triangle,
            head_size: 1.0,
            decor: LineDecor::Plain,
            color2: Color::BLUE,
            text_box: TextBox::None,
            text_flow: FlowMode::Straight,
            wrap: true,
            padding: 8.0,
            radius: 10.0,
            tail_width: 22.0,
            shape: ShapeKind::Rect,
            facing: Side::Left,
            highlight: HighlightShape::Rect,
            indent: 0.0,
            blur_outside: 0.0,
            dim_outside: 0.0,
            magnifier: MagnifierShape::Circle,
            connector: true,
            zoom: 2.0,
            step_shape: StepShape::Circle,
            step_letters: false,
            step_size: 32.0,
            emoji: "\u{1F600}".into(),
            sticker: StickerShape::Arrow,
            cursor: CursorShape::Arrow,
            blur_ellipse: false,
            blur_strength: 8.0,
            eraser_radius: 10.0,
            eraser_background: None,
            fill_tolerance: 32,
            fill_transparent: false,
        }
    }
}

impl ToolOptions {
    /// Defaults for a tool.
    pub fn for_tool(tool: Tool) -> Self {
        let mut o = Self::default();
        match tool {
            Tool::Line => o.head_end = ArrowHead::None,
            Tool::Curve => {
                o.line_kind = LineKind::Quad;
                o.head_end = ArrowHead::None;
            }
            Tool::Polyline => {
                o.line_kind = LineKind::Polyline;
                o.head_end = ArrowHead::None;
            }
            Tool::Elbow => o.line_kind = LineKind::ElbowH,
            Tool::Callout => {
                o.text_box = TextBox::Rounded;
                o.padding = 10.0;
            }
            Tool::Rect => o.radius = 0.0,
            Tool::Highlighter => o.radius = 6.0,
            Tool::Ellipse => o.shape = ShapeKind::Ellipse,
            Tool::Polygon => o.shape = ShapeKind::Polygon,
            Tool::Bracket => o.shape = ShapeKind::CurlyBracket,
            Tool::LineHighlighter => o.highlight = HighlightShape::Line,
            Tool::Pixelate => o.blur_strength = 10.0,
            _ => {}
        }
        o
    }
}

/// Default style for a tool.
pub fn default_style(tool: Tool) -> Style {
    let mut s = Style::default();
    let soft = Some(Shadow { dx: 1.5, dy: 2.5, blur: 3.0, color: Color::rgba(0, 0, 0, 80) });
    match tool {
        Tool::Text => {
            s.font.color = Color::RED;
            s.font.size = 18.0;
            s.font.bold = true;
            s.width = 0.0;
        }
        Tool::Callout => {
            s.fill = Color::WHITE;
            s.color = Color::BLUE;
            s.width = 2.0;
            s.font.size = 14.0;
            s.shadow = soft;
        }
        Tool::Arrow => s.shadow = soft,
        Tool::Line | Tool::Curve | Tool::Polyline | Tool::Elbow => {}
        Tool::Rect | Tool::Ellipse | Tool::Polygon => s.width = 3.0,
        Tool::Bracket => {
            s.width = 3.0;
            s.fill = Color::TRANSPARENT;
        }
        Tool::Highlighter => {
            s.fill = Color::YELLOW;
            s.width = 0.0;
        }
        Tool::LineHighlighter => {
            s.fill = Color::YELLOW;
            s.width = 18.0;
        }
        Tool::Magnifier => {
            s.color = Color::WHITE;
            s.width = 4.0;
            s.shadow = Some(Shadow { dx: 2.0, dy: 3.0, blur: 6.0, color: Color::rgba(0, 0, 0, 110) });
        }
        Tool::Step => {
            s.fill = Color::RED;
            s.color = Color::WHITE;
            s.width = 2.0;
            s.font.color = Color::WHITE;
            s.font.bold = true;
            s.shadow = soft;
        }
        Tool::Sticker => {
            s.fill = Color::BLUE;
            s.shadow = soft;
        }
        Tool::Cursor => {
            s.fill = Color::ORANGE;
            s.shadow = Some(Shadow { dx: 1.0, dy: 2.0, blur: 2.0, color: Color::rgba(0, 0, 0, 90) });
        }
        Tool::Fill => s.fill = Color::RED,
        Tool::Pencil => s.width = 2.0,
        _ => {}
    }
    s
}

/// Create an object for `tool` from a drag from `a` to `b` (a click when they coincide).
/// `objects` is used for step auto-numbering. Returns `None` for `Select` and `Image`
/// (use [`create_picture`]).
pub fn create(
    tool: Tool,
    id: ObjectId,
    style: &Style,
    opts: &ToolOptions,
    a: Pt,
    b: Pt,
    objects: &[Object],
) -> Option<Object> {
    let click = a.dist(b) < 3.0;
    let rect_or = |w: f32, h: f32| if click { RectF::new(a.x, a.y, w, h) } else { RectF::from_points(a, b) };
    let centered_or = |w: f32, h: f32| if click { RectF::from_center(a, w, h) } else { RectF::from_points(a, b) };
    let b = if click { a + Pt::new(120.0, 0.0) } else { b };
    let kind = match tool {
        Tool::Select | Tool::Image => return None,
        Tool::Text | Tool::Callout => {
            let rect = rect_or(220.0, 40.0);
            let flow = match opts.text_flow {
                FlowMode::Straight => TextFlow::Straight,
                FlowMode::Arc => TextFlow::Arc { bend: 0.5 },
                FlowMode::Path => TextFlow::Path { points: vec![a, b], smooth: true },
            };
            let callout = tool == Tool::Callout;
            Kind::Text(Text {
                rect,
                text: String::new(),
                flow,
                wrap: opts.wrap,
                boxed: if callout && opts.text_box == TextBox::None { TextBox::Rounded } else { opts.text_box },
                padding: opts.padding,
                radius: opts.radius,
                tail: callout.then(|| Tail {
                    tip: Pt::new(rect.x + rect.w * 0.2, rect.bottom() + 48.0),
                    width: opts.tail_width,
                }),
            })
        }
        Tool::Line | Tool::Arrow | Tool::Curve | Tool::Polyline | Tool::Elbow => {
            let kind = match tool {
                Tool::Line | Tool::Arrow => match opts.line_kind {
                    LineKind::Quad | LineKind::Cubic | LineKind::Smooth => LineKind::Straight,
                    k => k,
                },
                Tool::Curve => match opts.line_kind {
                    LineKind::Cubic => LineKind::Cubic,
                    LineKind::Smooth => LineKind::Smooth,
                    _ => LineKind::Quad,
                },
                Tool::Polyline => LineKind::Polyline,
                _ => match opts.line_kind {
                    LineKind::ElbowV => LineKind::ElbowV,
                    _ => LineKind::ElbowH,
                },
            };
            let bulge = (b - a).perp() * 0.25;
            let points = match kind {
                LineKind::Quad => vec![a, a.lerp(b, 0.5) - bulge, b],
                LineKind::Cubic => vec![a, a.lerp(b, 0.33) - bulge, a.lerp(b, 0.67) + bulge, b],
                _ => vec![a, b],
            };
            let end = if tool == Tool::Arrow && opts.head_end == ArrowHead::None && opts.head_start == ArrowHead::None {
                ArrowHead::Triangle
            } else if tool == Tool::Line {
                ArrowHead::None
            } else {
                opts.head_end
            };
            let start = if tool == Tool::Line { ArrowHead::None } else { opts.head_start };
            Kind::Line(Line {
                kind,
                points,
                start,
                end,
                head_size: opts.head_size,
                decor: opts.decor,
                color2: opts.color2,
            })
        }
        Tool::Rect | Tool::Ellipse | Tool::Polygon | Tool::Bracket => {
            let rect = rect_or(120.0, 80.0);
            let kind = match tool {
                Tool::Rect => ShapeKind::Rect,
                Tool::Ellipse => ShapeKind::Ellipse,
                Tool::Polygon => ShapeKind::Polygon,
                _ => match opts.shape {
                    ShapeKind::SquareBracket => ShapeKind::SquareBracket,
                    _ => ShapeKind::CurlyBracket,
                },
            };
            let points = if kind == ShapeKind::Polygon {
                vec![Pt::new(rect.center().x, rect.y), rect.br(), Pt::new(rect.x, rect.bottom())]
            } else {
                Vec::new()
            };
            let radius = if kind == ShapeKind::Rect { opts.radius.max(0.0) } else { 0.0 };
            Kind::Shape(Shape { kind, rect, radius, points, facing: opts.facing })
        }
        Tool::Highlighter | Tool::LineHighlighter => {
            let line = tool == Tool::LineHighlighter;
            Kind::Highlight(Highlight {
                shape: if line {
                    HighlightShape::Line
                } else if opts.highlight == HighlightShape::Line {
                    HighlightShape::Rect
                } else {
                    opts.highlight
                },
                rect: rect_or(160.0, 28.0),
                points: if line { vec![a, b] } else { Vec::new() },
                radius: opts.radius.max(0.0),
                indent: opts.indent,
                blur_outside: opts.blur_outside,
                dim_outside: opts.dim_outside,
            })
        }
        Tool::Magnifier => {
            let source = centered_or(60.0, 60.0);
            let z = if opts.zoom.is_finite() { opts.zoom.clamp(1.0, 16.0) } else { 2.0 };
            let (w, h) = (source.w * z, source.h * z);
            let target = RectF::new(source.right() + 30.0, source.center().y - h / 2.0, w, h);
            Kind::Magnifier(Magnifier { source, target, shape: opts.magnifier, connector: opts.connector })
        }
        Tool::Step => Kind::Step(Step {
            center: if click { a } else { a.lerp(b, 0.5) },
            size: opts.step_size.clamp(8.0, 400.0),
            value: next_step_value_as(objects, opts.step_letters),
            shape: opts.step_shape,
            angle: 135.0,
        }),
        Tool::Emoji => Kind::Emoji(Emoji { frame: Frame::new(centered_or(48.0, 48.0)), text: opts.emoji.clone() }),
        Tool::Sticker => {
            let (w, h) = super::render::sticker_size(opts.sticker);
            Kind::Sticker(Sticker { frame: Frame::new(centered_or(w * 1.2, h * 1.2)), shape: opts.sticker })
        }
        Tool::Cursor => {
            let (w, h) = super::render::cursor_size(opts.cursor);
            let s = 32.0 / h;
            let rect = if click { RectF::new(a.x, a.y, w * s, h * s) } else { RectF::from_points(a, b) };
            Kind::Cursor(Cursor { frame: Frame::new(rect), shape: opts.cursor })
        }
        Tool::Blur | Tool::Pixelate => Kind::Blur(Blur {
            rect: rect_or(120.0, 40.0),
            ellipse: opts.blur_ellipse,
            mode: if tool == Tool::Pixelate { BlurMode::Pixelate } else { BlurMode::Blur },
            strength: opts.blur_strength,
        }),
        Tool::Eraser => Kind::Eraser(Eraser {
            points: if click { vec![a] } else { vec![a, b] },
            radius: opts.eraser_radius,
            background: opts.eraser_background,
        }),
        Tool::Fill => Kind::Fill(Fill { at: a, tolerance: opts.fill_tolerance, transparent: opts.fill_transparent }),
        Tool::Pencil => Kind::Pencil(Pencil { points: if click { vec![a] } else { vec![a, b] } }),
    };
    Some(Object::new(id, kind, style.clone()))
}

/// An inserted image at `at` (top-left), at its natural size, or scaled down to fit `max_w`.
pub fn create_picture(id: ObjectId, png: Vec<u8>, size: (u32, u32), at: Pt, max_w: Option<f32>) -> Object {
    let (w, h) = (size.0.max(1) as f32, size.1.max(1) as f32);
    let s = max_w.filter(|m| *m > 0.0 && w > *m).map(|m| m / w).unwrap_or(1.0);
    let frame = Frame::new(RectF::new(at.x, at.y, w * s, h * s));
    Object::new(id, Kind::Image(Picture { frame, png: ImageData(png.into()) }), Style::default())
}

/// The tool options an object currently uses (starting from `base` for unrelated fields).
pub fn options_of(obj: &Object, base: &ToolOptions) -> ToolOptions {
    let mut o = base.clone();
    match &obj.kind {
        Kind::Text(t) => {
            o.text_box = t.boxed;
            o.wrap = t.wrap;
            o.padding = t.padding;
            o.radius = t.radius;
            o.text_flow = match t.flow {
                TextFlow::Straight => FlowMode::Straight,
                TextFlow::Arc { .. } => FlowMode::Arc,
                TextFlow::Path { .. } => FlowMode::Path,
            };
            if let Some(tail) = t.tail {
                o.tail_width = tail.width;
            }
        }
        Kind::Line(l) => {
            o.line_kind = l.kind;
            o.head_start = l.start;
            o.head_end = l.end;
            o.head_size = l.head_size;
            o.decor = l.decor;
            o.color2 = l.color2;
        }
        Kind::Shape(s) => {
            o.shape = s.kind;
            o.radius = s.radius;
            o.facing = s.facing;
        }
        Kind::Highlight(h) => {
            o.highlight = h.shape;
            o.indent = h.indent;
            o.blur_outside = h.blur_outside;
            o.dim_outside = h.dim_outside;
            o.radius = h.radius;
        }
        Kind::Magnifier(m) => {
            o.magnifier = m.shape;
            o.connector = m.connector;
            if m.source.w > 0.0 {
                o.zoom = m.target.w / m.source.w;
            }
        }
        Kind::Step(s) => {
            o.step_shape = s.shape;
            o.step_size = s.size;
            o.step_letters = matches!(s.value, StepValue::Letter(_));
        }
        Kind::Emoji(e) => o.emoji = e.text.clone(),
        Kind::Sticker(s) => o.sticker = s.shape,
        Kind::Cursor(c) => o.cursor = c.shape,
        Kind::Blur(b) => {
            o.blur_ellipse = b.ellipse;
            o.blur_strength = b.strength;
        }
        Kind::Eraser(e) => {
            o.eraser_radius = e.radius;
            o.eraser_background = e.background;
        }
        Kind::Fill(f) => {
            o.fill_tolerance = f.tolerance;
            o.fill_transparent = f.transparent;
        }
        Kind::Image(_) | Kind::Pencil(_) => {}
    }
    o
}

/// Apply the non-geometry options relevant to this object's kind (e.g. a preset).
pub fn apply_options(obj: &mut Object, o: &ToolOptions) {
    match &mut obj.kind {
        Kind::Text(t) => {
            t.boxed = o.text_box;
            t.wrap = o.wrap;
            t.padding = o.padding;
            t.radius = o.radius;
            if let Some(tail) = &mut t.tail {
                tail.width = o.tail_width;
            }
        }
        Kind::Line(l) => {
            l.start = o.head_start;
            l.end = o.head_end;
            l.head_size = o.head_size;
            l.decor = o.decor;
            l.color2 = o.color2;
        }
        Kind::Shape(s) => {
            if s.kind == ShapeKind::Rect {
                s.radius = o.radius;
            }
            s.facing = o.facing;
        }
        Kind::Highlight(h) => {
            h.indent = o.indent;
            h.blur_outside = o.blur_outside;
            h.dim_outside = o.dim_outside;
            h.radius = o.radius;
        }
        Kind::Magnifier(m) => {
            m.shape = o.magnifier;
            m.connector = o.connector;
        }
        Kind::Step(s) => {
            s.shape = o.step_shape;
            s.size = o.step_size.clamp(8.0, 400.0);
        }
        Kind::Sticker(s) => s.shape = o.sticker,
        Kind::Cursor(c) => c.shape = o.cursor,
        Kind::Blur(b) => {
            b.ellipse = o.blur_ellipse;
            b.strength = o.blur_strength;
        }
        Kind::Eraser(e) => {
            e.radius = o.eraser_radius;
            e.background = o.eraser_background;
        }
        Kind::Fill(f) => {
            f.tolerance = o.fill_tolerance;
            f.transparent = o.fill_transparent;
        }
        Kind::Emoji(_) | Kind::Image(_) | Kind::Pencil(_) => {}
    }
}
