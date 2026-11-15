//! Tests for serialization, `.sfcap`, hit-testing, handles, alignment, z-order, undo/redo,
//! step numbering, copy/paste, group style edits and tool creation.

use super::tests::{p, rect, showcase, Scene};
use super::*;
use crate::common::Image;
use std::sync::Arc;

fn shape(id: ObjectId, r: RectF, fill: Color) -> Object {
    Object::new(
        id,
        Kind::Shape(Shape { kind: ShapeKind::Rect, rect: r, ..Default::default() }),
        Style { fill, width: 2.0, ..Style::default() },
    )
}

fn ids(objects: &[Object]) -> Vec<ObjectId> {
    objects.iter().map(|o| o.id).collect()
}

fn approx(a: Pt, b: Pt) -> bool {
    a.dist(b) < 1e-3
}

fn approx_rect(a: RectF, b: RectF) -> bool {
    (a.x - b.x).abs() < 1e-3 && (a.y - b.y).abs() < 1e-3 && (a.w - b.w).abs() < 1e-3 && (a.h - b.h).abs() < 1e-3
}

// ----- Serialization -----

#[test]
fn json_roundtrip_every_object() {
    let (_, objects) = showcase();
    let json = sfcap::annotations_to_json(&objects).expect("json");
    assert!(json.contains("\"version\": 1"));
    let back = sfcap::annotations_from_json(&json).expect("parse");
    assert_eq!(back, objects);
    // Unknown fields are ignored; missing style falls back to defaults.
    let lenient = r#"{"version":1,"objects":[{"id":7,"kind":{"type":"step","center":{"x":1,"y":2},"future":true}}]}"#;
    let objs = sfcap::annotations_from_json(lenient).expect("lenient parse");
    assert_eq!(objs[0].id, 7);
    assert!(matches!(objs[0].kind, Kind::Step(Step { value: StepValue::Number(1), .. })));
    let newer = r#"{"version":99,"objects":[]}"#;
    assert!(sfcap::annotations_from_json(newer).is_err());
}

#[test]
fn sfcap_roundtrip_keeps_everything_editable() {
    let (base, objects) = showcase();
    let dir = std::env::temp_dir().join(format!("snapforge-sfcap-{}", std::process::id()));
    let path = dir.join("project.sfcap");
    let meta = Meta::new("region", "Showcase");
    save_sfcap(&path, &base, &objects, &meta).expect("save");
    let (img, back, m) = load_sfcap(&path).expect("load");
    assert_eq!(img, base);
    assert_eq!(back, objects);
    assert_eq!((m.width, m.height), (base.width, base.height));
    assert_eq!((m.title.as_str(), m.source.as_str()), ("Showcase", "region"));
    assert_eq!(m.app_version, env!("CARGO_PKG_VERSION"));
    assert!(sfcap::is_sfcap(&path));
    // No temp files left behind by the atomic write.
    let leftovers = std::fs::read_dir(&dir).expect("dir").count();
    assert_eq!(leftovers, 1);
    std::fs::remove_dir_all(&dir).ok();
    assert!(sfcap::from_bytes(b"not a zip").is_err());
}

#[test]
fn sfcap_with_transparency() {
    let mut base = Image::filled(8, 8, [10, 20, 30, 255]);
    base.set_pixel(1, 1, [0, 0, 0, 0]);
    let bytes = sfcap::to_bytes(&base, &[], &Meta::default()).expect("bytes");
    let (img, objs, _) = sfcap::from_bytes(&bytes).expect("load");
    assert_eq!(img, base);
    assert!(objs.is_empty());
}

// ----- Hit-testing -----

#[test]
fn hit_test_respects_fill_zorder_and_rotation() {
    let mut objects = vec![
        shape(1, rect(0.0, 0.0, 100.0, 100.0), Color::RED),
        shape(2, rect(50.0, 50.0, 100.0, 100.0), Color::TRANSPARENT),
    ];
    // Inside both: the unfilled top rect is only hit on its border, so the filled one wins.
    assert_eq!(hit_test(&objects, p(75.0, 75.0), 2.0), Some(1));
    assert_eq!(hit_test(&objects, p(51.0, 75.0), 2.0), Some(2));
    assert_eq!(hit_test(&objects, p(120.0, 120.0), 2.0), None);
    assert_eq!(hit_test(&objects, p(300.0, 300.0), 2.0), None);
    // Z-order: after bringing 1 to the front it is hit at the border of 2 too.
    bring_to_front(&mut objects, &[1]);
    assert_eq!(hit_test(&objects, p(51.0, 75.0), 2.0), Some(1));

    let mut f = Frame::new(rect(0.0, 0.0, 100.0, 20.0));
    f.rotation = 90.0;
    let emoji = Object::new(3, Kind::Emoji(Emoji { frame: f, text: "x".into() }), Style::default());
    assert!(edit::hits(&emoji, p(50.0, 50.0), 0.0));
    assert!(!edit::hits(&emoji, p(5.0, 10.0), 0.0), "rotated: the original corner is outside");

    let line = Object::new(
        4,
        Kind::Line(Line { points: vec![p(0.0, 0.0), p(100.0, 0.0)], end: ArrowHead::Triangle, ..Default::default() }),
        Style { width: 4.0, ..Style::default() },
    );
    assert!(edit::hits(&line, p(50.0, 3.0), 2.0));
    assert!(!edit::hits(&line, p(50.0, 9.0), 2.0));
    assert!(edit::hits(&line, p(98.0, 6.0), 1.0), "arrowhead is hittable");

    let found = edit::objects_in_rect(&objects, rect(-10.0, -10.0, 200.0, 200.0));
    assert_eq!(found.len(), 2);
}

#[test]
fn hit_test_text_and_steps() {
    let mut s = Scene::new();
    let t = s.text(10.0, 10.0, 200.0, "Hello", 16.0, Color::BLACK, false);
    let step = s.add(Kind::Step(Step { center: p(300.0, 300.0), size: 30.0, ..Default::default() }), Style::default());
    assert_eq!(hit_test(&s.objects, p(20.0, 20.0), 1.0), Some(t));
    assert_eq!(hit_test(&s.objects, p(310.0, 300.0), 1.0), Some(step));
    assert_eq!(hit_test(&s.objects, p(330.0, 330.0), 1.0), None);
}

// ----- Handles -----

#[test]
fn resize_handles_and_constraints() {
    let mut o = shape(1, rect(10.0, 10.0, 100.0, 50.0), Color::RED);
    let hs = handles(&o, 1.0);
    assert_eq!(hs.len(), 8);
    let se = hs.iter().find(|h| h.id == HandleId::Resize(Dir::SE)).expect("se");
    assert!(approx(se.pos, p(110.0, 60.0)));
    assert_eq!(handle_at(&o, p(109.0, 61.0), 4.0, 1.0), Some(HandleId::Resize(Dir::SE)));

    let snap = o.clone();
    drag_handle(&mut o, HandleId::Resize(Dir::SE), p(20.0, 10.0), &DragConstraints::default());
    assert!(approx_rect(frame(&o), rect(10.0, 10.0, 120.0, 60.0)));

    let mut o = snap.clone();
    drag_handle(&mut o, HandleId::Resize(Dir::W), p(-10.0, 99.0), &DragConstraints::default());
    assert!(approx_rect(frame(&o), rect(0.0, 10.0, 110.0, 50.0)), "edge handles move one axis");

    let mut o = snap.clone();
    let keep = DragConstraints { keep_aspect: true, ..Default::default() };
    drag_handle(&mut o, HandleId::Resize(Dir::SE), p(100.0, 0.0), &keep);
    assert!(approx_rect(frame(&o), rect(10.0, 10.0, 200.0, 100.0)));

    let mut o = snap.clone();
    let center = DragConstraints { from_center: true, ..Default::default() };
    drag_handle(&mut o, HandleId::Resize(Dir::E), p(10.0, 0.0), &center);
    assert!(approx_rect(frame(&o), rect(0.0, 10.0, 120.0, 50.0)));

    // Dragging past the opposite edge flips into a normalized rect.
    let mut o = snap;
    drag_handle(&mut o, HandleId::Resize(Dir::E), p(-150.0, 0.0), &DragConstraints::default());
    assert!(approx_rect(frame(&o), rect(-40.0, 10.0, 50.0, 50.0)));
}

#[test]
fn rotated_frame_resize_keeps_opposite_corner() {
    let mut f = Frame::new(rect(0.0, 0.0, 100.0, 50.0));
    f.rotation = 30.0;
    let mut o = Object::new(1, Kind::Sticker(Sticker { frame: f, shape: StickerShape::Arrow }), Style::default());
    let nw_before = handles(&o, 1.0).into_iter().find(|h| h.id == HandleId::Resize(Dir::NW)).expect("nw").pos;
    let se_before = handles(&o, 1.0).into_iter().find(|h| h.id == HandleId::Resize(Dir::SE)).expect("se").pos;
    drag_handle(&mut o, HandleId::Resize(Dir::SE), p(15.0, 25.0), &DragConstraints::default());
    let hs = handles(&o, 1.0);
    let nw_after = hs.iter().find(|h| h.id == HandleId::Resize(Dir::NW)).expect("nw").pos;
    let se_after = hs.iter().find(|h| h.id == HandleId::Resize(Dir::SE)).expect("se").pos;
    assert!(approx(nw_before, nw_after), "{nw_before:?} vs {nw_after:?}");
    assert!(approx(se_after, se_before + p(15.0, 25.0)), "{se_after:?}");

    // Rotation knob with 15° snapping.
    let knob = hs.iter().find(|h| h.id == HandleId::Rotate).expect("rotate").pos;
    let c = frame(&o).center();
    let target = c + p(100.0, 3.0); // roughly to the right: 90°
    drag_handle(&mut o, HandleId::Rotate, target - knob, &DragConstraints { snap_angle: true, ..Default::default() });
    let Kind::Sticker(s) = &o.kind else { panic!() };
    assert!((s.frame.rotation - 90.0).abs() < 1e-3, "{}", s.frame.rotation);
}

#[test]
fn vertex_tail_bend_and_source_handles() {
    let mut line = Object::new(
        1,
        Kind::Line(Line {
            kind: LineKind::Quad,
            points: vec![p(0.0, 0.0), p(50.0, -40.0), p(100.0, 0.0)],
            ..Default::default()
        }),
        Style::default(),
    );
    let hs = handles(&line, 1.0);
    assert_eq!(hs.len(), 3);
    assert_eq!(hs[1].kind, HandleKind::Control);
    drag_handle(
        &mut line,
        HandleId::Vertex(2),
        p(10.0, 3.0),
        &DragConstraints { snap_angle: true, ..Default::default() },
    );
    let Kind::Line(l) = &line.kind else { panic!() };
    // Snapped to 45° steps relative to the previous point (50,-40): (60,3) → 135°/90° direction.
    let d = l.points[2] - l.points[1];
    assert!((d.angle_deg() / 45.0 - (d.angle_deg() / 45.0).round()).abs() < 1e-3);

    let mut callout = Object::new(
        2,
        Kind::Text(Text {
            rect: rect(0.0, 0.0, 100.0, 40.0),
            boxed: TextBox::Rounded,
            tail: Some(Tail { tip: p(20.0, 100.0), width: 20.0 }),
            ..Default::default()
        }),
        Style::default(),
    );
    assert!(handles(&callout, 1.0).iter().any(|h| h.id == HandleId::TailTip));
    drag_handle(&mut callout, HandleId::TailTip, p(5.0, 5.0), &DragConstraints::default());
    let Kind::Text(t) = &callout.kind else { panic!() };
    assert_eq!(t.tail.map(|tl| tl.tip), Some(p(25.0, 105.0)));

    let mut arc = Object::new(
        3,
        Kind::Text(Text {
            rect: rect(0.0, 0.0, 200.0, 100.0),
            flow: TextFlow::Arc { bend: 0.0 },
            ..Default::default()
        }),
        Style::default(),
    );
    drag_handle(&mut arc, HandleId::Bend, p(0.0, -25.0), &DragConstraints::default());
    let Kind::Text(Text { flow: TextFlow::Arc { bend }, .. }) = &arc.kind else { panic!() };
    assert!((bend - 0.5).abs() < 1e-3, "{bend}");

    let mut mag = Object::new(4, Kind::Magnifier(Magnifier::default()), Style::default());
    drag_handle(&mut mag, HandleId::SourceMove, p(10.0, 10.0), &DragConstraints::default());
    drag_handle(&mut mag, HandleId::Source(Dir::SE), p(10.0, 10.0), &DragConstraints::default());
    let Kind::Magnifier(m) = &mag.kind else { panic!() };
    assert!(approx_rect(m.source, rect(10.0, 10.0, 50.0, 50.0)));

    let mut step =
        Object::new(5, Kind::Step(Step { shape: StepShape::Teardrop, ..Default::default() }), Style::default());
    let tip = handles(&step, 1.0).into_iter().find(|h| h.id == HandleId::Bend).expect("tip").pos;
    drag_handle(&mut step, HandleId::Bend, p(0.0, 48.0) - tip, &DragConstraints::default());
    let Kind::Step(s) = &step.kind else { panic!() };
    assert!((s.angle - 90.0).abs() < 1e-3);
}

#[test]
fn constrain_helpers() {
    assert!(approx(constrain_to_hv(p(0.0, 0.0), p(10.0, 3.0)), p(10.0, 0.0)));
    assert!(approx(constrain_to_hv(p(0.0, 0.0), p(2.0, -9.0)), p(0.0, -9.0)));
    let d = constrain_to_axis(p(0.0, 0.0), p(10.0, 9.0));
    assert!((d.x - d.y).abs() < 1e-3);
}

// ----- Move, align, z-order -----

#[test]
fn move_and_align() {
    let mut objects = vec![
        shape(1, rect(10.0, 10.0, 50.0, 20.0), Color::RED),
        shape(2, rect(30.0, 40.0, 80.0, 30.0), Color::RED),
        Object::new(
            3,
            Kind::Step(Step { center: p(200.0, 100.0), size: 20.0, ..Default::default() }),
            Style::default(),
        ),
    ];
    assert_eq!(move_objects(&mut objects, &[1, 2], p(5.0, 5.0)), 2);
    assert!(approx_rect(frame(&objects[0]), rect(15.0, 15.0, 50.0, 20.0)));

    let all = [1, 2, 3];
    assert!(align(&mut objects, &all, Align::Left));
    assert!(objects.iter().all(|o| (frame(o).x - 15.0).abs() < 1e-3));
    assert!(align(&mut objects, &all, Align::Bottom));
    let bottom = frame(&objects[2]).bottom();
    assert!(objects.iter().all(|o| (frame(o).bottom() - bottom).abs() < 1e-3));
    assert!(align(&mut objects, &[1, 2], Align::SameSize));
    assert!((frame(&objects[1]).w - 50.0).abs() < 1e-3 && (frame(&objects[1]).h - 20.0).abs() < 1e-3);
    assert!(!align(&mut objects, &[1], Align::Top), "needs two objects");
    let b = selection_bounds(&objects, &[1, 2]).expect("bounds");
    assert!(b.w > 0.0);
}

#[test]
fn z_order_ops() {
    let mut objects: Vec<Object> = (1..=5).map(|i| shape(i, rect(0.0, 0.0, 1.0, 1.0), Color::RED)).collect();
    assert!(bring_to_front(&mut objects, &[2, 4]));
    assert_eq!(ids(&objects), vec![1, 3, 5, 2, 4]);
    assert!(!bring_to_front(&mut objects, &[2, 4]));
    assert!(send_to_back(&mut objects, &[4]));
    assert_eq!(ids(&objects), vec![4, 1, 3, 5, 2]);
    assert!(bring_forward(&mut objects, &[4, 3]));
    assert_eq!(ids(&objects), vec![1, 4, 5, 3, 2]);
    assert!(send_backward(&mut objects, &[2]));
    assert_eq!(ids(&objects), vec![1, 4, 5, 2, 3]);
    let gone = delete(&mut objects, &[5, 1]);
    assert_eq!(ids(&gone), vec![1, 5]);
    assert_eq!(ids(&objects), vec![4, 2, 3]);
}

// ----- Undo / redo -----

#[test]
fn history_object_and_pixel_edits() {
    let mut doc = Document::new(Image::filled(4, 4, [1, 2, 3, 255]));
    let mut h = History::new();
    assert!(!h.is_dirty());

    // Add.
    let before = doc.objects.clone();
    doc.objects.push(shape(1, rect(0.0, 0.0, 10.0, 10.0), Color::RED));
    doc.objects.push(shape(2, rect(5.0, 5.0, 10.0, 10.0), Color::BLUE));
    assert!(h.record_objects(&before, &doc.objects));
    let s1 = doc.objects.clone();
    // Modify + reorder.
    let before = doc.objects.clone();
    move_objects(&mut doc.objects, &[1], p(3.0, 0.0));
    bring_to_front(&mut doc.objects, &[1]);
    assert!(h.record_objects(&before, &doc.objects));
    let s2 = doc.objects.clone();
    // Delete.
    let before = doc.objects.clone();
    delete(&mut doc.objects, &[2]);
    h.record_objects(&before, &doc.objects);
    let s3 = doc.objects.clone();
    // Pixel edit.
    let old = doc.base.clone();
    let new = Arc::new(Image::filled(2, 2, [9, 9, 9, 255]));
    doc.set_base(new.clone());
    h.record_base(old.clone(), new.clone());
    assert!(!h.record_objects(&doc.objects, &doc.objects.clone()), "no-op is not recorded");
    assert_eq!(h.undo_len(), 4);

    let rev = doc.base_rev;
    assert!(h.undo(&mut doc));
    assert_eq!(*doc.base, *old);
    assert_ne!(doc.base_rev, rev);
    assert!(h.undo(&mut doc));
    assert_eq!(doc.objects, s2);
    assert!(h.undo(&mut doc));
    assert_eq!(doc.objects, s1);
    assert!(h.undo(&mut doc));
    assert!(doc.objects.is_empty());
    assert!(!h.undo(&mut doc));
    assert!(!h.is_dirty());
    assert!(h.redo(&mut doc) && h.redo(&mut doc));
    assert_eq!(doc.objects, s2);
    assert!(h.redo(&mut doc));
    assert_eq!(doc.objects, s3);
    h.mark_saved();
    assert!(h.redo(&mut doc));
    assert_eq!(*doc.base, *new);
    assert!(h.is_dirty());
    assert!(!h.redo(&mut doc));

    // A new edit after undo clears redo.
    h.undo(&mut doc);
    assert!(!h.is_dirty());
    let before = doc.objects.clone();
    doc.objects.clear();
    h.record_objects(&before, &doc.objects);
    assert!(!h.can_redo());
    assert!(h.is_dirty());

    // Batch: undone as one step.
    let before_objs = doc.objects.clone();
    doc.objects.push(shape(9, rect(0.0, 0.0, 1.0, 1.0), Color::RED));
    let ob = doc.base.clone();
    doc.set_base(Arc::new(Image::filled(1, 1, [0, 0, 0, 255])));
    let batch = Edit::Batch(vec![
        Edit::objects(&before_objs, &doc.objects).expect("diff"),
        Edit::Base { before: ob.clone(), after: doc.base.clone() },
    ]);
    h.record(batch);
    h.undo(&mut doc);
    assert!(doc.objects.is_empty());
    assert_eq!(*doc.base, *ob);
}

#[test]
fn many_undos_are_unlimited() {
    let mut doc = Document::new(Image::new(1, 1));
    let mut h = History::new();
    for i in 0..500 {
        let before = doc.objects.clone();
        doc.objects.push(shape(i + 1, rect(0.0, 0.0, 1.0, 1.0), Color::RED));
        h.record_objects(&before, &doc.objects);
    }
    let mut n = 0;
    while h.undo(&mut doc) {
        n += 1;
    }
    assert_eq!(n, 500);
    assert!(doc.objects.is_empty());
}

// ----- Steps -----

#[test]
fn step_auto_increment() {
    let step = |id, v| Object::new(id, Kind::Step(Step { value: v, ..Default::default() }), Style::default());
    assert_eq!(next_step_value(&[]), StepValue::Number(1));
    let mut objs = vec![step(1, StepValue::Number(1)), step(2, StepValue::Number(5)), step(3, StepValue::Number(2))];
    assert_eq!(next_step_value(&objs), StepValue::Number(6));
    objs.push(step(4, StepValue::Letter(1)));
    assert_eq!(next_step_value(&objs), StepValue::Letter(2), "follows the most recent format");
    assert_eq!(next_step_value_as(&objs, false), StepValue::Number(6));
    assert_eq!(next_step_value_as(&[step(1, StepValue::Number(999))], false), StepValue::Number(999));
    assert_eq!(next_step_value_as(&[], true), StepValue::Letter(0));
    assert_eq!(StepValue::Letter(0).label(), "A");
    assert_eq!(StepValue::Letter(25).label(), "Z");
    assert_eq!(StepValue::Letter(26).label(), "AA");
    assert_eq!(StepValue::Letter(701).label(), "ZZ");
    assert_eq!(StepValue::Number(1234).label(), "999");

    // The Step tool numbers new stamps automatically.
    let st = default_style(Tool::Step);
    let opts = ToolOptions::for_tool(Tool::Step);
    let a = create(Tool::Step, 10, &st, &opts, p(5.0, 5.0), p(5.0, 5.0), &objs).expect("step");
    assert!(matches!(a.kind, Kind::Step(Step { value: StepValue::Number(6), .. })));
}

// ----- Clipboard, group style -----

#[test]
fn copy_paste_remaps_ids() {
    let mut objects =
        vec![shape(1, rect(0.0, 0.0, 10.0, 10.0), Color::RED), shape(7, rect(20.0, 0.0, 10.0, 10.0), Color::BLUE)];
    let json = serialize_selection(&objects, &[7, 1]);
    let new_ids = paste(&mut objects, &json, p(10.0, 10.0)).expect("paste");
    assert_eq!(new_ids, vec![8, 9]);
    assert_eq!(objects.len(), 4);
    assert!(approx_rect(frame(&objects[2]), rect(10.0, 10.0, 10.0, 10.0)), "z-order and offset kept");
    assert_eq!(objects[3].style.fill, Color::BLUE);
    assert!(paste(&mut objects, "garbage", p(0.0, 0.0)).is_err());
    assert!(paste(&mut objects, r#"{"version":2,"objects":[]}"#, p(0.0, 0.0)).is_err());
    let dup = duplicate(&mut objects, &[1], p(5.0, 5.0));
    assert_eq!(dup, vec![10]);
}

#[test]
fn group_style_patch() {
    let mut objects = vec![
        shape(1, rect(0.0, 0.0, 10.0, 10.0), Color::RED),
        Object::new(2, Kind::Line(Line::default()), Style::default()),
        Object::new(3, Kind::Text(Text::default()), Style::default()),
    ];
    let patch = StylePatch {
        color: Some(Color::GREEN),
        width: Some(9.0),
        head_end: Some(ArrowHead::Stealth),
        font_size: Some(500.0),
        shadow: Some(Some(Shadow::default())),
        ..Default::default()
    };
    assert_eq!(apply_style(&mut objects, &[1, 2, 3], &patch), 3);
    assert!(objects.iter().all(|o| o.style.color == Color::GREEN && o.style.width == 9.0));
    assert!(matches!(&objects[1].kind, Kind::Line(l) if l.end == ArrowHead::Stealth));
    assert_eq!(objects[2].style.font.size, Font::MAX_SIZE, "font size clamped to 200 pt");
    assert_eq!(apply_style(&mut objects, &[1, 2, 3], &patch), 0, "already applied");
    let copy = StylePatch::from_style(&objects[0].style);
    let mut other = Object::new(9, Kind::Shape(Shape::default()), Style::default());
    assert!(copy.apply(&mut other));
    assert_eq!(other.style, objects[0].style);
}

// ----- Tools -----

#[test]
fn every_tool_creates_and_renders() {
    let base = Image::filled(200, 120, [240, 240, 240, 255]);
    let mut objects = Vec::new();
    for tool in Tool::ALL {
        let st = default_style(tool);
        let opts = ToolOptions::for_tool(tool);
        let id = next_id(&objects);
        match create(tool, id, &st, &opts, p(20.0, 20.0), p(120.0, 80.0), &objects) {
            Some(o) => {
                assert_eq!(Tool::of(&o), tool, "round trip for {tool:?}");
                assert_eq!(tools::options_of(&o, &opts).decor, opts.decor);
                objects.push(o);
            }
            None => assert!(matches!(tool, Tool::Select | Tool::Image)),
        }
        // Clicks (no drag) also produce something sensible.
        let _ = create(tool, 999, &st, &opts, p(50.0, 50.0), p(50.0, 50.0), &objects);
        assert_eq!(Tool::from_id(tool.id()), Some(tool));
    }
    let out = render(&base, &objects, &RenderOptions::default());
    assert_eq!(out.width, 200);
    for o in &objects {
        let b = bounds(o);
        assert!(b.is_finite() && b.w >= 0.0, "{:?}", o.kind.name());
        let _ = handles(o, 1.0);
    }
    let mut o = objects[1].clone();
    let mut opts = tools::options_of(&o, &ToolOptions::default());
    opts.text_box = TextBox::Oval;
    tools::apply_options(&mut o, &opts);
    assert!(matches!(&o.kind, Kind::Text(t) if t.boxed == TextBox::Oval));
}

#[test]
fn degenerate_input_does_not_panic() {
    let base = Image::filled(50, 50, [255, 255, 255, 255]);
    let objects = vec![
        Object::new(1, Kind::Line(Line { points: vec![], ..Default::default() }), Style::default()),
        Object::new(
            2,
            Kind::Line(Line { points: vec![p(5.0, 5.0)], end: ArrowHead::ThreeD, ..Default::default() }),
            Style::default(),
        ),
        Object::new(3, Kind::Shape(Shape { rect: rect(10.0, 10.0, 0.0, 0.0), ..Default::default() }), Style::default()),
        Object::new(
            4,
            Kind::Text(Text {
                text: "\n\n".into(),
                flow: TextFlow::Path { points: vec![], smooth: true },
                ..Default::default()
            }),
            Style::default(),
        ),
        Object::new(
            5,
            Kind::Image(Picture { png: ImageData(vec![1, 2, 3].into()), ..Default::default() }),
            Style::default(),
        ),
        Object::new(6, Kind::Pencil(Pencil { points: vec![p(f32::NAN, 1.0)] }), Style::default()),
        Object::new(7, Kind::Fill(Fill { at: p(-5.0, 900.0), ..Default::default() }), Style::default()),
        Object::new(
            8,
            Kind::Blur(Blur { rect: rect(40.0, 40.0, 100.0, 100.0), strength: f32::INFINITY, ..Default::default() }),
            Style::default(),
        ),
        Object::new(
            9,
            Kind::Magnifier(Magnifier { source: rect(-50.0, -50.0, 0.0, 0.0), ..Default::default() }),
            Style::default(),
        ),
        Object::new(
            10,
            Kind::Emoji(Emoji { frame: Frame::new(rect(0.0, 0.0, 10.0, 10.0)), text: String::new() }),
            Style::default(),
        ),
        Object::new(
            11,
            Kind::Step(Step { size: 0.0, ..Default::default() }),
            Style { opacity: f32::NAN, ..Style::default() },
        ),
    ];
    let _ = render(&base, &objects, &RenderOptions::default());
    let _ = render_overlay(
        &base,
        &objects,
        &View { x: 0.0, y: 0.0, zoom: 0.0, width: 10, height: 10 },
        &RenderOptions::default(),
    );
    for o in &objects {
        let _ = hit_test(std::slice::from_ref(o), p(5.0, 5.0), 2.0);
        let mut c = o.clone();
        for h in handles(o, 1.0) {
            drag_handle(&mut c, h.id, p(3.0, 3.0), &DragConstraints { keep_aspect: true, ..Default::default() });
        }
    }
    let _ = render(&Image::new(0, 0), &objects, &RenderOptions::default());
}
