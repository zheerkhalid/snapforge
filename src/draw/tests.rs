//! Integration tests for the draw module: showcase, golden images, editing, history, serialization.

use super::*;
use crate::common::Image;

// ---------------------------------------------------------------------------------------------
// Scene building helpers
// ---------------------------------------------------------------------------------------------

pub(super) struct Scene {
    pub objects: Vec<Object>,
}

impl Scene {
    pub fn new() -> Self {
        Self { objects: Vec::new() }
    }

    pub fn add(&mut self, kind: Kind, style: Style) -> ObjectId {
        let id = next_id(&self.objects);
        self.objects.push(Object::new(id, kind, style));
        id
    }

    pub fn style(&mut self, id: ObjectId) -> &mut Style {
        let i = self.objects.iter().position(|o| o.id == id).unwrap_or(0);
        &mut self.objects[i].style
    }

    #[allow(clippy::too_many_arguments)]
    pub fn text(&mut self, x: f32, y: f32, w: f32, s: &str, size: f32, color: Color, bold: bool) -> ObjectId {
        let mut st = Style::default();
        st.font.size = size;
        st.font.color = color;
        st.font.bold = bold;
        st.width = 0.0;
        let t = Text { rect: RectF::new(x, y, w, 10.0), text: s.into(), padding: 0.0, ..Default::default() };
        self.add(Kind::Text(t), st)
    }

    pub fn label(&mut self, x: f32, y: f32, s: &str) -> ObjectId {
        self.text(x, y, 400.0, s, 9.0, Color::rgb(0x5B, 0x63, 0x70), false)
    }

    pub fn heading(&mut self, x: f32, y: f32, s: &str) -> ObjectId {
        self.text(x, y, 600.0, s, 12.0, Color::rgb(0x22, 0x2A, 0x35), true)
    }

    pub fn line(&mut self, kind: LineKind, pts: &[Pt], color: Color, w: f32) -> ObjectId {
        let st = Style { color, width: w, ..Style::default() };
        self.add(Kind::Line(Line { kind, points: pts.to_vec(), ..Default::default() }), st)
    }

    pub fn line_mut(&mut self, id: ObjectId) -> &mut Line {
        let i = self.objects.iter().position(|o| o.id == id).unwrap_or(0);
        match &mut self.objects[i].kind {
            Kind::Line(l) => l,
            _ => panic!("not a line"),
        }
    }
}

pub(super) fn p(x: f32, y: f32) -> Pt {
    Pt::new(x, y)
}

pub(super) fn rect(x: f32, y: f32, w: f32, h: f32) -> RectF {
    RectF::new(x, y, w, h)
}

/// A screenshot-like base: soft background, white cards with text, a small chart.
pub(super) fn sample_base(w: u32, h: u32) -> Image {
    let bg = Image::filled(w, h, [0xF6, 0xF3, 0xF1, 0xFF]);
    let mut s = Scene::new();
    let card = |s: &mut Scene, r: RectF| {
        let st = Style {
            fill: Color::WHITE,
            color: Color::rgb(0xDD, 0xE1, 0xE7),
            width: 1.0,
            shadow: Some(Shadow { dx: 0.0, dy: 2.0, blur: 4.0, color: Color::rgba(20, 30, 50, 30) }),
            ..Style::default()
        };
        s.add(Kind::Shape(Shape { kind: ShapeKind::Rect, rect: r, radius: 8.0, ..Default::default() }), st);
    };
    // Document card used by highlighters, magnifier and raster tools.
    card(&mut s, rect(20.0, 690.0, 560.0, 230.0));
    let ink = Color::rgb(0x2B, 0x32, 0x3C);
    let lines = [
        "Quarterly report: revenue grew 24% year over year.",
        "Customer satisfaction reached an all-time high of 4.8/5.",
        "Support tickets dropped by a third after the redesign.",
        "Next: ship the mobile app and expand to three regions.",
        "Contact: team@example.com  ·  Build 2026.11.14",
    ];
    for (i, l) in lines.iter().enumerate() {
        s.text(40.0, 708.0 + i as f32 * 40.0, 520.0, l, 13.0, ink, i == 0);
    }
    // A chart card for the magnifier.
    card(&mut s, rect(600.0, 690.0, 300.0, 230.0));
    let bars = [70.0, 120.0, 95.0, 160.0, 135.0, 185.0];
    for (i, h) in bars.iter().enumerate() {
        let st = Style {
            fill: Color::rgb(0x42, 0x85, 0xF4).mix(Color::rgb(0x8E, 0x24, 0xAA), i as f32 / 6.0),
            width: 0.0,
            ..Style::default()
        };
        let r = rect(625.0 + i as f32 * 44.0, 900.0 - h, 30.0, *h);
        s.add(Kind::Shape(Shape { kind: ShapeKind::Rect, rect: r, radius: 4.0, ..Default::default() }), st);
    }
    s.text(620.0, 700.0, 260.0, "Monthly active users", 11.0, ink, true);
    // Raster tools card.
    card(&mut s, rect(1180.0, 940.0, 400.0, 220.0));
    for (i, l) in ["Password: hunter2-secret", "Account 4921 7788 0012", "Signed: J. Doe"].iter().enumerate() {
        s.text(1200.0, 1000.0 + i as f32 * 30.0, 360.0, l, 12.0, ink, false);
    }
    let st = Style {
        fill: Color::rgb(0xFF, 0xFF, 0xFF),
        color: Color::rgb(0x33, 0x33, 0x33),
        width: 2.0,
        ..Style::default()
    };
    s.add(
        Kind::Shape(Shape { kind: ShapeKind::Rect, rect: rect(1470.0, 1085.0, 90.0, 55.0), ..Default::default() }),
        st,
    );
    render(&bg, &s.objects, &RenderOptions::default())
}

/// Every object type and style, laid out on a 1600x1180 canvas.
pub(super) fn showcase() -> (Image, Vec<Object>) {
    let base = sample_base(1600, 1180);
    let mut s = Scene::new();

    // ----- Decorative line styles -----
    s.heading(20.0, 8.0, "Line styles");
    let decors = LineDecor::ALL;
    let palette = [
        Color::RED,
        Color::BLUE,
        Color::rgb(0xFF, 0x57, 0x22),
        Color::rgb(0x00, 0xE5, 0xFF),
        Color::rgb(0x37, 0x47, 0x4F),
        Color::PURPLE,
        Color::GREEN,
        Color::rgb(0x00, 0x89, 0x7B),
        Color::ORANGE,
        Color::rgb(0x6D, 0x4C, 0x41),
        Color::rgb(0x1A, 0x23, 0x7E),
    ];
    for (i, d) in decors.iter().enumerate() {
        let x = 20.0 + i as f32 * 143.0;
        let id = s.line(LineKind::Quad, &[p(x + 10.0, 105.0), p(x + 60.0, 25.0), p(x + 125.0, 95.0)], palette[i], 6.0);
        let l = s.line_mut(id);
        l.decor = *d;
        l.color2 = Color::rgb(0xFF, 0xC1, 0x07);
        s.label(x + 10.0, 112.0, &format!("{d:?}"));
    }

    // ----- Arrowheads and dashes -----
    s.heading(20.0, 140.0, "Arrowheads");
    let heads = &ArrowHead::ALL[1..];
    for (i, h) in heads.iter().enumerate() {
        let x = 20.0 + i as f32 * 112.0;
        let id = s.line(LineKind::Straight, &[p(x + 8.0, 225.0), p(x + 95.0, 175.0)], Color::RED, 4.0);
        s.line_mut(id).end = *h;
        s.label(x + 8.0, 232.0, &format!("{h:?}"));
    }
    s.heading(930.0, 140.0, "Dash styles");
    for (i, d) in Dash::ALL.iter().enumerate() {
        let y = 172.0 + i as f32 * 15.0;
        let id = s.line(LineKind::Straight, &[p(930.0, y), p(1130.0, y)], Color::rgb(0x26, 0x32, 0x38), 3.0);
        s.style(id).dash = *d;
        s.label(1140.0, y - 7.0, &format!("{d:?}"));
    }
    // Double-headed gradient arrow and outlined arrow.
    let id = s.line(LineKind::Straight, &[p(1250.0, 230.0), p(1570.0, 170.0)], Color::rgb(0xE9, 0x1E, 0x63), 7.0);
    {
        let l = s.line_mut(id);
        l.start = ArrowHead::Triangle;
        l.end = ArrowHead::Triangle;
        l.decor = LineDecor::Gradient;
        l.color2 = Color::rgb(0x3F, 0x51, 0xB5);
    }
    s.style(id).shadow = Some(Shadow::default());
    s.label(1250.0, 240.0, "Gradient, both ends, shadow");

    // ----- Line geometry -----
    s.heading(20.0, 262.0, "Curves, polylines, elbows, outline effect");
    let outline = Some(Outline { color: Color::WHITE, width: 2.5 });
    let ids = [
        s.line(
            LineKind::Polyline,
            &[p(25.0, 380.0), p(70.0, 300.0), p(115.0, 370.0), p(160.0, 300.0)],
            Color::BLUE,
            5.0,
        ),
        s.line(
            LineKind::Smooth,
            &[p(190.0, 380.0), p(235.0, 300.0), p(280.0, 370.0), p(330.0, 300.0)],
            Color::GREEN,
            5.0,
        ),
        s.line(
            LineKind::Cubic,
            &[p(360.0, 380.0), p(380.0, 270.0), p(470.0, 410.0), p(500.0, 300.0)],
            Color::PURPLE,
            5.0,
        ),
        s.line(LineKind::ElbowH, &[p(530.0, 300.0), p(640.0, 385.0)], Color::ORANGE, 5.0),
        s.line(LineKind::ElbowV, &[p(670.0, 300.0), p(780.0, 385.0)], Color::RED, 5.0),
    ];
    for (i, id) in ids.iter().enumerate() {
        s.style(*id).outline = outline;
        s.style(*id).shadow = Some(Shadow { dx: 1.5, dy: 2.5, blur: 3.0, color: Color::rgba(0, 0, 0, 90) });
        s.line_mut(*id).end =
            [ArrowHead::Stealth, ArrowHead::Fancy, ArrowHead::ThreeD, ArrowHead::Triangle, ArrowHead::Open][i];
    }
    s.label(25.0, 392.0, "Polyline · Smooth · Cubic · Elbow H · Elbow V (white outline + shadow)");

    // ----- Shapes -----
    s.heading(830.0, 262.0, "Shapes");
    let shadow = Some(Shadow { dx: 2.0, dy: 3.0, blur: 5.0, color: Color::rgba(0, 0, 0, 70) });
    let shape =
        |kind: ShapeKind, r: RectF, radius: f32| Kind::Shape(Shape { kind, rect: r, radius, ..Default::default() });
    s.add(
        shape(ShapeKind::Rect, rect(835.0, 295.0, 110.0, 80.0), 0.0),
        Style { color: Color::RED, width: 3.0, ..Style::default() },
    );
    s.add(
        shape(ShapeKind::Rect, rect(965.0, 295.0, 110.0, 80.0), 14.0),
        Style { color: Color::BLUE, fill: Color::rgb(0xBB, 0xDE, 0xFB), width: 3.0, shadow, ..Style::default() },
    );
    s.add(
        shape(ShapeKind::Ellipse, rect(1095.0, 295.0, 110.0, 80.0), 0.0),
        Style {
            color: Color::GREEN,
            fill: Color::GREEN.with_alpha(90),
            width: 3.0,
            dash: Dash::Dash,
            ..Style::default()
        },
    );
    let star: Vec<Pt> = (0..10)
        .map(|i| {
            let a = (i as f32 * 36.0 - 90.0).to_radians();
            let r = if i % 2 == 0 { 44.0 } else { 19.0 };
            p(1275.0 + r * a.cos(), 337.0 + r * a.sin())
        })
        .collect();
    s.add(
        Kind::Shape(Shape { kind: ShapeKind::Polygon, points: star, ..Default::default() }),
        Style {
            color: Color::rgb(0xF5, 0x7F, 0x17),
            fill: Color::rgb(0xFF, 0xD5, 0x4F),
            width: 2.5,
            shadow,
            ..Style::default()
        },
    );
    s.add(
        Kind::Shape(Shape {
            kind: ShapeKind::CurlyBracket,
            rect: rect(1340.0, 292.0, 34.0, 92.0),
            facing: Side::Left,
            ..Default::default()
        }),
        Style { color: Color::PURPLE, fill: Color::PURPLE.with_alpha(40), width: 3.0, ..Style::default() },
    );
    s.add(
        Kind::Shape(Shape {
            kind: ShapeKind::CurlyBracket,
            rect: rect(1390.0, 292.0, 90.0, 30.0),
            facing: Side::Top,
            ..Default::default()
        }),
        Style { color: Color::rgb(0x26, 0x32, 0x38), width: 3.0, ..Style::default() },
    );
    s.add(
        Kind::Shape(Shape {
            kind: ShapeKind::SquareBracket,
            rect: rect(1500.0, 292.0, 28.0, 92.0),
            facing: Side::Right,
            ..Default::default()
        }),
        Style { color: Color::RED, fill: Color::RED.with_alpha(30), width: 3.0, opacity: 0.8, ..Style::default() },
    );
    s.label(835.0, 392.0, "Rect · Rounded + shadow · Ellipse dashed · Polygon · Brackets");

    // ----- Text -----
    s.heading(20.0, 418.0, "Text");
    let t1 = s.text(25.0, 450.0, 220.0, "Bold red label", 20.0, Color::RED, true);
    let _ = t1;
    let t2 = s.text(25.0, 495.0, 240.0, "Outlined text", 22.0, Color::rgb(0x1A, 0x23, 0x7E), true);
    s.style(t2).outline = Some(Outline { color: Color::rgb(0xFF, 0xEB, 0x3B), width: 3.0 });
    let t3 = s.text(25.0, 545.0, 240.0, "Soft shadow", 22.0, Color::WHITE, true);
    s.style(t3).shadow = Some(Shadow { dx: 2.0, dy: 2.0, blur: 3.0, color: Color::rgba(0, 0, 0, 200) });
    let t4 = s.text(25.0, 595.0, 260.0, "Italic underlined", 16.0, Color::rgb(0x00, 0x69, 0x5C), false);
    s.style(t4).font.italic = true;
    s.style(t4).font.underline = true;

    let boxed = |s: &mut Scene, r: RectF, txt: &str, b: TextBox, fill: Color, border: Color, tail: Option<Tail>| {
        let mut st = Style { fill, color: border, width: 2.0, ..Style::default() };
        st.font.size = 13.0;
        st.font.align = TextAlign::Center;
        st.shadow = Some(Shadow { dx: 1.5, dy: 2.5, blur: 4.0, color: Color::rgba(0, 0, 0, 70) });
        let t = Text { rect: r, text: txt.into(), boxed: b, tail, padding: 10.0, ..Default::default() };
        s.add(Kind::Text(t), st)
    };
    boxed(
        &mut s,
        rect(300.0, 440.0, 150.0, 40.0),
        "Rectangle box",
        TextBox::Rect,
        Color::rgb(0xFF, 0xF8, 0xE1),
        Color::ORANGE,
        None,
    );
    boxed(
        &mut s,
        rect(300.0, 500.0, 150.0, 40.0),
        "Rounded box",
        TextBox::Rounded,
        Color::rgb(0xE3, 0xF2, 0xFD),
        Color::BLUE,
        None,
    );
    boxed(
        &mut s,
        rect(300.0, 560.0, 150.0, 50.0),
        "Oval box",
        TextBox::Oval,
        Color::rgb(0xF3, 0xE5, 0xF5),
        Color::PURPLE,
        None,
    );
    boxed(
        &mut s,
        rect(480.0, 440.0, 200.0, 60.0),
        "Callout with a tail that wraps onto two lines",
        TextBox::Rounded,
        Color::WHITE,
        Color::BLUE,
        Some(Tail { tip: p(520.0, 600.0), width: 24.0 }),
    );
    // Arc and path text.
    let mut st = Style::default();
    st.font.size = 18.0;
    st.font.bold = true;
    st.font.color = Color::rgb(0xD8, 0x1B, 0x60);
    st.font.align = TextAlign::Center;
    s.add(
        Kind::Text(Text {
            rect: rect(720.0, 440.0, 230.0, 110.0),
            text: "Text on a curved arc".into(),
            flow: TextFlow::Arc { bend: 0.6 },
            ..Default::default()
        }),
        st.clone(),
    );
    st.font.color = Color::rgb(0x00, 0x79, 0x6B);
    st.font.size = 15.0;
    st.font.align = TextAlign::Left;
    s.add(
        Kind::Text(Text {
            rect: rect(980.0, 440.0, 10.0, 10.0),
            text: "follows any path you draw ~ smoothly".into(),
            flow: TextFlow::Path {
                points: vec![p(980.0, 600.0), p(1060.0, 470.0), p(1150.0, 590.0), p(1240.0, 470.0)],
                smooth: true,
            },
            ..Default::default()
        }),
        st,
    );
    let mut st = Style::default();
    st.font.size = 12.0;
    st.font.color = Color::rgb(0x26, 0x32, 0x38);
    st.font.align = TextAlign::Center;
    st.fill = Color::rgb(0xFA, 0xFA, 0xFA);
    st.color = Color::rgb(0x90, 0xA4, 0xAE);
    st.width = 1.0;
    st.dash = Dash::Dash;
    s.add(
        Kind::Text(Text {
            rect: rect(1290.0, 440.0, 280.0, 40.0),
            text:
                "Auto-expanding box: this paragraph wraps at the box width and the box grows taller to fit every line."
                    .into(),
            boxed: TextBox::Rect,
            ..Default::default()
        }),
        st,
    );

    // ----- Highlighters, magnifier (over the document card) -----
    s.heading(20.0, 660.0, "Highlighters, magnifier");
    let hl = |shape: HighlightShape, r: RectF, fill: Color, border: f32, indent: f32| {
        (
            Kind::Highlight(Highlight { shape, rect: r, indent, ..Default::default() }),
            Style { fill, width: border, color: fill.darken(0.3), ..Style::default() },
        )
    };
    let (k, st) = hl(HighlightShape::Rect, rect(36.0, 704.0, 300.0, 30.0), Color::YELLOW, 0.0, 0.0);
    s.add(k, st);
    let (k, st) = hl(HighlightShape::Rounded, rect(36.0, 744.0, 420.0, 30.0), Color::rgb(0xA5, 0xF3, 0xA8), 2.0, 3.0);
    s.add(k, st);
    let (k, st) = hl(HighlightShape::Oval, rect(30.0, 780.0, 230.0, 40.0), Color::rgb(0xFF, 0xB3, 0xD0), 0.0, 0.0);
    s.add(k, st);
    s.add(
        Kind::Highlight(Highlight {
            shape: HighlightShape::Line,
            points: vec![p(40.0, 838.0), p(330.0, 838.0)],
            ..Default::default()
        }),
        Style { fill: Color::rgb(0x80, 0xDE, 0xEA), width: 16.0, ..Style::default() },
    );
    s.add(
        Kind::Magnifier(Magnifier {
            source: rect(765.0, 720.0, 60.0, 60.0),
            target: rect(930.0, 680.0, 150.0, 150.0),
            shape: MagnifierShape::Circle,
            connector: true,
        }),
        default_style(Tool::Magnifier),
    );
    let mut mst = default_style(Tool::Magnifier);
    mst.color = Color::rgb(0x26, 0x32, 0x38);
    mst.width = 3.0;
    s.add(
        Kind::Magnifier(Magnifier {
            source: rect(40.0, 860.0, 120.0, 30.0),
            target: rect(400.0, 850.0, 170.0, 60.0),
            shape: MagnifierShape::Rounded,
            connector: true,
        }),
        mst,
    );

    // ----- Steps -----
    s.heading(1110.0, 660.0, "Step stamps");
    let step = |s: &mut Scene, c: Pt, v: StepValue, shape: StepShape, fill: Color, size: f32| {
        let mut st = default_style(Tool::Step);
        st.fill = fill;
        s.add(Kind::Step(Step { center: c, size, value: v, shape, angle: 135.0 }), st);
    };
    step(&mut s, p(1130.0, 710.0), StepValue::Number(1), StepShape::Circle, Color::RED, 34.0);
    step(&mut s, p(1180.0, 710.0), StepValue::Number(2), StepShape::Circle, Color::RED, 34.0);
    step(&mut s, p(1230.0, 710.0), StepValue::Number(42), StepShape::Circle, Color::BLUE, 34.0);
    step(&mut s, p(1285.0, 710.0), StepValue::Number(999), StepShape::Circle, Color::GREEN, 40.0);
    step(&mut s, p(1340.0, 710.0), StepValue::Letter(0), StepShape::Square, Color::PURPLE, 34.0);
    step(&mut s, p(1390.0, 710.0), StepValue::Letter(27), StepShape::Square, Color::ORANGE, 34.0);
    step(&mut s, p(1450.0, 715.0), StepValue::Number(7), StepShape::Teardrop, Color::rgb(0x00, 0x89, 0x7B), 34.0);
    step(&mut s, p(1520.0, 715.0), StepValue::Letter(25), StepShape::Teardrop, Color::rgb(0x26, 0x32, 0x38), 34.0);

    // ----- Emoji -----
    s.heading(1110.0, 760.0, "Emoji");
    for (i, e) in
        ["\u{1F600}", "\u{1F44D}", "\u{2764}\u{FE0F}", "\u{1F525}", "\u{2705}", "\u{26A0}\u{FE0F}"].iter().enumerate()
    {
        let r = rect(1112.0 + i as f32 * 60.0, 790.0, 48.0, 48.0);
        s.add(Kind::Emoji(Emoji { frame: Frame::new(r), text: (*e).into() }), Style::default());
    }

    // ----- 3D arrows -----
    s.heading(20.0, 935.0, "3D arrows");
    let colors = [Color::BLUE, Color::RED, Color::GREEN, Color::ORANGE, Color::PURPLE, Color::rgb(0x00, 0xAC, 0xC1)];
    for (i, sh) in StickerShape::ALL.iter().enumerate() {
        let (w, h) = render::sticker_size(*sh);
        let sc = 0.85;
        let r = rect(25.0 + i as f32 * 115.0, 965.0, w * sc, h * sc);
        let mut st = default_style(Tool::Sticker);
        st.fill = colors[i];
        let mut f = Frame::new(r);
        if i == 0 {
            f.rotation = -20.0;
        }
        s.add(Kind::Sticker(Sticker { frame: f, shape: *sh }), st);
    }

    // ----- Cursors -----
    s.heading(720.0, 935.0, "Pointers");
    for (i, c) in CursorShape::ALL.iter().enumerate() {
        let (w, h) = render::cursor_size(*c);
        let k = 48.0 / h;
        let r = rect(725.0 + i as f32 * 62.0, 965.0, w * k, h * k);
        s.add(Kind::Cursor(Cursor { frame: Frame::new(r), shape: *c }), default_style(Tool::Cursor));
    }

    // ----- Inserted image -----
    s.heading(720.0, 1035.0, "Image (rotated, flipped)");
    let mut thumb = Image::new(64, 40);
    for y in 0..40u32 {
        for x in 0..64u32 {
            let r = (x * 4) as u8;
            let g = (y * 6) as u8;
            thumb.set_pixel(x, y, [200, g, r, 255]);
        }
    }
    let png = crate::output::save::encode(&thumb, crate::settings::ImageFormat::Png, 100).unwrap_or_default();
    let mut pic = tools::create_picture(0, png.clone(), (64, 40), p(735.0, 1075.0), None);
    pic.id = next_id(&s.objects);
    if let Kind::Image(ref mut im) = pic.kind {
        im.frame.rect = rect(735.0, 1075.0, 120.0, 75.0);
        im.frame.rotation = 8.0;
    }
    pic.style.shadow = Some(Shadow::default());
    s.objects.push(pic);
    let mut pic2 = tools::create_picture(0, png, (64, 40), p(880.0, 1075.0), None);
    pic2.id = next_id(&s.objects);
    if let Kind::Image(ref mut im) = pic2.kind {
        im.frame.rect = rect(880.0, 1075.0, 96.0, 60.0);
        im.frame.flip_h = true;
    }
    s.objects.push(pic2);

    // ----- Raster tools -----
    s.heading(1180.0, 910.0, "Raster: blur, pixelate, eraser, fill, pencil");
    s.add(
        Kind::Blur(Blur {
            rect: rect(1275.0, 995.0, 150.0, 26.0),
            ellipse: false,
            mode: BlurMode::Blur,
            strength: 4.0,
        }),
        Style::default(),
    );
    s.add(
        Kind::Blur(Blur {
            rect: rect(1270.0, 1025.0, 170.0, 26.0),
            ellipse: false,
            mode: BlurMode::Pixelate,
            strength: 7.0,
        }),
        Style::default(),
    );
    s.add(
        Kind::Eraser(Eraser {
            points: vec![p(1200.0, 1120.0), p(1260.0, 1140.0), p(1320.0, 1115.0)],
            radius: 9.0,
            background: Some(Color::rgb(0xFF, 0xF1, 0x76)),
        }),
        Style::default(),
    );
    s.add(
        Kind::Fill(Fill { at: p(1500.0, 1110.0), tolerance: 30, transparent: false }),
        Style { fill: Color::rgb(0x81, 0xC7, 0x84), ..Style::default() },
    );
    s.add(
        Kind::Pencil(Pencil {
            points: vec![p(1350.0, 1140.0), p(1370.0, 1120.0), p(1390.0, 1145.0), p(1410.0, 1118.0), p(1440.0, 1142.0)],
        }),
        Style { color: Color::RED, width: 3.0, ..Style::default() },
    );
    s.label(1200.0, 1150.0, "eraser → background · pencil (hard pixels) · fill");

    (base, s.objects)
}

#[test]
fn showcase_renders() {
    let (base, objects) = showcase();
    let out = render(&base, &objects, &RenderOptions::default());
    assert_eq!((out.width, out.height), (base.width, base.height));
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
    let png = crate::output::save::encode(&out, crate::settings::ImageFormat::Png, 100).expect("encode");
    std::fs::create_dir_all(&dir).ok();
    std::fs::write(dir.join("draw-showcase.png"), png).expect("write showcase");
}
