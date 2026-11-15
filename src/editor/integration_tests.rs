//! Tests of the integration pass: the full effect table, the draw layer's undo interplay
//! with pixel edits, `.sfcap` projects through the editor document, and the draw tool
//! commands (Tab cycling, F-keys) in the command registry.

use super::annot::{self, layer};
use super::document::Document;
use super::effects::{EffectCtx, EffectRegistry, ParamValues, Scope};
use super::history::{Budget, History};
use super::{fx_install, fx_session, menus};
use crate::common::{Image, Rect};
use crate::draw::{self, Kind, Object, Pt, RectF, Tool};
use eframe::egui::{Key, Modifiers};
use std::path::PathBuf;

fn full_registry() -> EffectRegistry {
    let mut r = EffectRegistry::with_placeholders();
    fx_install::install(&mut r);
    fx_session::install(&mut r);
    r
}

fn test_image(w: u32, h: u32) -> Image {
    let mut img = Image::new(w, h);
    for y in 0..h {
        for x in 0..w {
            img.set_pixel(x, y, [(x * 30) as u8, (y * 40) as u8, 128, 255]);
        }
    }
    img
}

#[test]
fn every_registered_effect_is_real_and_applies() {
    let r = full_registry();
    assert!(r.len() >= 38, "only {} effects", r.len());
    let img = test_image(11, 9);
    for d in r.iter() {
        assert!(!d.placeholder, "{} is still a placeholder", d.id);
        let values = ParamValues::defaults(d.params.as_deref().unwrap_or(&[]));
        for (scale, preview) in [(1.0, false), (0.5, true)] {
            let ctx = EffectCtx { scale, preview, background: [255, 255, 255, 255] };
            let out = (d.apply)(&img, &values, &ctx);
            assert!(!out.is_empty(), "{} returned an empty image", d.id);
            if d.scope == Scope::SelectionOrImage {
                assert_eq!((out.width, out.height), (img.width, img.height), "{} changed the size", d.id);
            }
        }
    }
    // The interactive ones are registered with menu entries.
    for id in fx_session::INTERACTIVE.iter().chain([&fx_session::COUNT_COLORS]) {
        assert!(r.get(id).is_some(), "{id} missing");
    }
    for k in ["Ctrl+1", "Ctrl+2", "Ctrl+3", "Ctrl+4", "Ctrl+R"] {
        assert!(r.iter().any(|d| d.shortcut == Some(k)), "{k} not bound");
    }
}

fn doc() -> Document {
    let mut d = Document::from_file(1, test_image(40, 30), PathBuf::from(r"C:\x\a.png"));
    d.history = History::new_sync(Budget::default());
    d
}

fn rect_object(id: u64, x: f32) -> Object {
    let st = draw::default_style(Tool::Rect);
    draw::create(
        Tool::Rect,
        id,
        &st,
        &draw::ToolOptions::for_tool(Tool::Rect),
        Pt::new(x, 2.0),
        Pt::new(x + 10.0, 12.0),
        &[],
    )
    .expect("rect")
}

fn ids(d: &Document) -> Vec<u64> {
    layer::layer(d).map(|l| l.objects().iter().map(|o| o.id).collect()).unwrap_or_default()
}

#[test]
fn overlay_undo_interleaves_with_pixel_edits() {
    let mut d = doc();
    let orig = (*d.image).clone();
    assert!(layer::edit(&mut d, "Rect", |objs| {
        objs.push(rect_object(1, 1.0));
        true
    }));
    assert!(d.dirty && d.has_annotations());
    // A pixel edit between two object edits.
    assert!(d.apply_image_edit("Paint", |img| {
        let mut o = img.clone();
        o.set_pixel(5, 5, [0, 0, 255, 255]);
        o
    }));
    let painted = (*d.image).clone();
    assert!(layer::edit(&mut d, "Rect", |objs| {
        objs.push(rect_object(2, 20.0));
        true
    }));
    assert_eq!(ids(&d), [1, 2]);
    assert_eq!(d.history.undo_names().collect::<Vec<_>>(), ["Rect", "Paint", "Rect"]);

    // One timeline: object, pixels, object.
    assert!(d.undo());
    assert_eq!(ids(&d), [1]);
    assert_eq!(*d.image, painted);
    assert!(d.undo());
    assert_eq!(*d.image, orig);
    assert_eq!(ids(&d), [1]);
    assert!(d.undo());
    assert!(ids(&d).is_empty());
    assert!(!d.dirty, "back at the saved state");
    assert!(!d.has_annotations());

    assert!(d.redo() && d.redo() && d.redo());
    assert_eq!(ids(&d), [1, 2]);
    assert_eq!(*d.image, painted);

    // A live drag recorded with `commit` is one step; no-op edits record nothing.
    let before = layer::layer(&d).map(|l| l.objects().to_vec()).unwrap_or_default();
    if let Some(l) = layer::layer_mut(&mut d) {
        for o in l.objects_mut().iter_mut() {
            draw::translate(o, Pt::new(3.0, 0.0));
        }
    }
    assert!(layer::commit(&mut d, "Move", &before));
    assert!(!layer::edit(&mut d, "Nothing", |_| true));
    assert_eq!(d.history.undo_len(), 4);

    // Export flattens the annotations onto the base (base itself untouched).
    let flat = d.export_job(false).run();
    assert_eq!((flat.width, flat.height), (40, 30));
    assert_ne!(*flat, *d.image);
    assert_eq!(*d.image, painted);
    // Selection export crops the flattened image.
    d.set_selection(super::selection::Selection::rect(Rect::new(0, 0, 8, 8)));
    let part = d.export_job(true).run();
    assert_eq!((part.width, part.height), (8, 8));
}

#[test]
fn draw_state_commands_record_steps() {
    let mut d = doc();
    layer::edit(&mut d, "Rect", |objs| {
        objs.push(rect_object(1, 1.0));
        objs.push(rect_object(2, 20.0));
        true
    });
    annot::with_state(|s| {
        assert!(s.select_all(&d));
        assert!(s.align(&mut d, draw::edit::Align::Left));
        assert!(s.nudge(&mut d, 2.0, 0.0));
        let json = s.copy_selected(&d).expect("copied");
        assert!(s.paste_json(&mut d, &json));
        assert_eq!(s.selection(&d).len(), 2);
        assert!(s.delete_selected(&mut d));
        assert!(!s.paste_json(&mut d, "not annotations"));
    });
    assert_eq!(ids(&d).len(), 2);
    assert_eq!(d.history.undo_len(), 5);
}

#[test]
fn sfcap_roundtrip_through_the_editor_document() {
    let mut d = doc();
    layer::edit(&mut d, "Text", |objs| {
        let mut t = draw::create(
            Tool::Text,
            1,
            &draw::default_style(Tool::Text),
            &draw::ToolOptions::for_tool(Tool::Text),
            Pt::new(2.0, 2.0),
            Pt::new(30.0, 12.0),
            &[],
        )
        .expect("text");
        if let Kind::Text(x) = &mut t.kind {
            x.text = "Hello".into();
        }
        objs.push(t);
        objs.push(rect_object(2, 5.0));
        true
    });
    let objects = layer::layer(&d).map(|l| l.objects().to_vec()).unwrap_or_default();
    let dir = std::env::temp_dir().join(format!("snapforge-sfcap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let image_path = dir.join("shot.png");
    let data = super::jobs::ProjectData {
        base: d.image.clone(),
        objects: objects.clone(),
        meta: draw::Meta::new("test", "shot"),
    };
    // Auto-save lands next to the image with the same stem.
    let saved = super::jobs::autosave_project(&image_path, &data).expect("autosave");
    assert_eq!(saved, dir.join("shot.sfcap"));
    let (img, objs, meta) = draw::load_sfcap(&saved).expect("load");
    assert_eq!(img, *d.image);
    assert_eq!(objs, objects);
    assert_eq!(meta.title, "shot");

    // Reopened as the editor does it: editable objects, clean, Save targets the project.
    let mut re = Document::new(2, img, String::new(), None, None);
    re.history = History::new_sync(Budget::default());
    re.overlay = Some(Box::new(layer::DrawLayer::with_objects(re.image.clone(), objs)));
    let rev = re.revision;
    re.on_project_saved(saved.clone(), rev);
    assert!(!re.dirty);
    assert_eq!(re.project.as_deref(), Some(saved.as_path()));
    assert_eq!(re.title, "shot.sfcap");
    assert!(layer::edit(&mut re, "Move", |o| {
        draw::translate(&mut o[0], Pt::new(1.0, 1.0));
        true
    }));
    assert!(re.dirty);
    assert!(re.undo());
    assert!(!re.dirty);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn draw_tool_commands_have_fkeys_and_tab() {
    let effects = full_registry();
    let mut tools = annot::draw_tools();
    tools.push(Box::new(fx_session::CloneTool::new()));
    let r = menus::build_registry(&effects, &tools, &[], &[]);
    assert!(r.conflicts().is_empty(), "{:?}", r.conflicts());
    let id = |k: Key, m: Modifiers| r.for_key(k, m).map(|c| c.id.clone());
    assert_eq!(id(Key::F2, Modifiers::NONE).as_deref(), Some("tool.draw.select"));
    assert_eq!(id(Key::F3, Modifiers::NONE).as_deref(), Some("tool.draw.text"));
    assert_eq!(id(Key::F4, Modifiers::NONE).as_deref(), Some("tool.draw.arrow"));
    assert_eq!(id(Key::F2, Modifiers::SHIFT).as_deref(), Some("tool.draw.callout"));
    assert_eq!(id(Key::Tab, Modifiers::NONE).as_deref(), Some("draw.next_tool"));
    assert_eq!(id(Key::Tab, Modifiers::SHIFT).as_deref(), Some("draw.prev_tool"));
    assert_eq!(id(Key::Num1, Modifiers::COMMAND).as_deref(), Some("effect:image.resize_25"));
    assert_eq!(id(Key::Num4, Modifiers::COMMAND).as_deref(), Some("effect:image.resize_200"));
    for t in Tool::ALL {
        assert!(r.get(&format!("tool.{}", annot::tool_cmd_id(t))).is_some(), "{t:?}");
        assert_eq!(annot::tool_of_id(&annot::tool_cmd_id(t)), Some(t));
    }
    // Layer geometry helper used by the eye dropper / creation.
    assert!(RectF::from_points(Pt::new(3.0, 3.0), Pt::new(1.0, 1.0)).w > 0.0);
}

/// Writes a demo image and project for manual / screenshot checks:
/// `SNAPFORGE_DEMO_DIR=... cargo test make_demo_files -- --ignored`.
#[test]
#[ignore]
fn make_demo_files() {
    let Ok(dir) = std::env::var("SNAPFORGE_DEMO_DIR") else { return };
    let dir = PathBuf::from(dir);
    let _ = std::fs::create_dir_all(&dir);
    // A fake app window: title bar, sidebar, cards.
    let (w, h) = (1100u32, 700u32);
    let mut img = Image::filled(w, h, [0xF4, 0xF1, 0xEE, 255]);
    let fill = |img: &mut Image, r: Rect, c: [u8; 4]| {
        for y in r.y..r.bottom() {
            for x in r.x..r.right() {
                img.set_pixel(x as u32, y as u32, c);
            }
        }
    };
    fill(&mut img, Rect::new(0, 0, w as i32, 48), [0x5A, 0x3C, 0x2B, 255]);
    fill(&mut img, Rect::new(0, 48, 220, h as i32 - 48), [0xE6, 0xE1, 0xDC, 255]);
    for i in 0..6 {
        fill(&mut img, Rect::new(24, 80 + i * 44, 160, 14), [0xB0, 0xA8, 0xA0, 255]);
    }
    for (i, c) in [[0xE5u8, 0x9A, 0x3B, 255], [0x4C, 0xAF, 0x50, 255], [0xC0, 0x6B, 0x36, 255]].iter().enumerate() {
        let x = 260 + i as i32 * 270;
        fill(&mut img, Rect::new(x, 90, 240, 160), [255, 255, 255, 255]);
        fill(&mut img, Rect::new(x, 90, 240, 8), *c);
        fill(&mut img, Rect::new(x + 20, 130, 150, 14), [0x90, 0x90, 0x90, 255]);
        fill(&mut img, Rect::new(x + 20, 160, 190, 10), [0xC8, 0xC8, 0xC8, 255]);
        fill(&mut img, Rect::new(x + 20, 180, 120, 10), [0xC8, 0xC8, 0xC8, 255]);
    }
    fill(&mut img, Rect::new(260, 290, 780, 360), [255, 255, 255, 255]);
    for i in 0..8 {
        fill(&mut img, Rect::new(290, 320 + i * 38, 600 - i * 40, 12), [0xD0, 0xD0, 0xD0, 255]);
    }
    let _ = crate::output::save::save_image(&img, &dir.join("demo.png"));

    let mut objs = Vec::new();
    let mut add = |tool: Tool, a: Pt, b: Pt, f: &dyn Fn(&mut Object)| {
        let id = objs.len() as u64 + 1;
        if let Some(mut o) =
            draw::create(tool, id, &draw::default_style(tool), &draw::ToolOptions::for_tool(tool), a, b, &objs)
        {
            f(&mut o);
            objs.push(o);
        }
    };
    add(Tool::Arrow, Pt::new(620.0, 470.0), Pt::new(760.0, 300.0), &|o| o.style.width = 6.0);
    add(Tool::Rect, Pt::new(250.0, 82.0), Pt::new(510.0, 258.0), &|_| {});
    add(Tool::Callout, Pt::new(560.0, 500.0), Pt::new(800.0, 560.0), &|o| {
        if let Kind::Text(t) = &mut o.kind {
            t.text = "New dashboard layout".into();
            if let Some(tail) = &mut t.tail {
                tail.tip = Pt::new(520.0, 470.0);
            }
        }
    });
    add(Tool::Step, Pt::new(240.0, 80.0), Pt::new(240.0, 80.0), &|_| {});
    add(Tool::Step, Pt::new(510.0, 80.0), Pt::new(510.0, 80.0), &|_| {});
    add(Tool::Highlighter, Pt::new(285.0, 314.0), Pt::new(900.0, 338.0), &|_| {});
    add(Tool::Text, Pt::new(860.0, 60.0), Pt::new(1080.0, 90.0), &|o| {
        if let Kind::Text(t) = &mut o.kind {
            t.text = "SnapForge".into();
        }
    });
    add(Tool::Blur, Pt::new(820.0, 120.0), Pt::new(1040.0, 200.0), &|_| {});
    add(Tool::Ellipse, Pt::new(16.0, 120.0), Pt::new(200.0, 160.0), &|o| o.style.color = draw::Color::GREEN);
    add(Tool::Magnifier, Pt::new(300.0, 470.0), Pt::new(360.0, 520.0), &|_| {});
    let _ = draw::save_sfcap(&dir.join("demo.sfcap"), &img, &objs, &draw::Meta::new("demo", "demo"));
}

#[test]
fn size_keys_are_bound() {
    let effects = full_registry();
    let tools = annot::draw_tools();
    let r = menus::build_registry(&effects, &tools, &[], &[]);
    assert!(r.conflicts().is_empty(), "{:?}", r.conflicts());
    let id = |k: Key, m: Modifiers| r.for_key(k, m).map(|c| c.id.clone());
    assert_eq!(id(Key::OpenBracket, Modifiers::NONE).as_deref(), Some("draw.size_down"));
    assert_eq!(id(Key::CloseBracket, Modifiers::NONE).as_deref(), Some("draw.size_up"));
    // Exact modifiers only: Ctrl+[ is not the size key.
    assert_eq!(id(Key::OpenBracket, Modifiers::COMMAND), None);
    let label = |cmd: &str| r.get(cmd).and_then(|c| c.shortcut.iter().next().map(|s| s.label()));
    assert_eq!(label("draw.size_down").as_deref(), Some("["));
    assert_eq!(label("draw.size_up").as_deref(), Some("]"));
}

#[test]
fn resize_steps_change_the_tool_when_nothing_is_selected() {
    let mut d = doc();
    let mut s = annot::DrawState::default();
    s.style_for(Tool::Pencil).style.width = 4.0;
    assert!(s.resize_by(Tool::Pencil, &mut d, 1));
    assert_eq!(s.style_for(Tool::Pencil).style.width, 5.0);
    assert!(s.resize_by(Tool::Pencil, &mut d, -3));
    assert_eq!(s.style_for(Tool::Pencil).style.width, 2.0);
    // The eraser's size is its radius, text tools use the font size, blur its strength.
    s.style_for(Tool::Eraser).options.eraser_radius = 20.0;
    assert!(s.resize_by(Tool::Eraser, &mut d, 1));
    assert_eq!(s.style_for(Tool::Eraser).options.eraser_radius, 23.0);
    s.style_for(Tool::Text).style.font.size = 18.0;
    assert!(s.resize_by(Tool::Text, &mut d, 1));
    assert_eq!(s.style_for(Tool::Text).style.font.size, 21.0);
    s.style_for(Tool::Blur).options.blur_strength = 80.0;
    assert!(s.resize_by(Tool::Blur, &mut d, 1));
    assert_eq!(s.style_for(Tool::Blur).options.blur_strength, 80.0, "stops at the limit");
    // Tools without a size ignore the keys; nothing was recorded for undo.
    assert!(!s.resize_by(Tool::Select, &mut d, 1));
    assert!(!s.resize_by(Tool::Fill, &mut d, 1));
    assert!(!s.resize_by(Tool::Pencil, &mut d, 0));
    assert!(s.prop_before.is_none());
    assert!(!d.history.can_undo());
    assert!(!d.has_annotations());
}

#[test]
fn resize_steps_on_selected_objects_are_one_undo_step() {
    let mut d = doc();
    assert!(layer::edit(&mut d, "Add", |o| {
        o.push(rect_object(1, 2.0));
        o.push(rect_object(2, 12.0));
        true
    }));
    let width = |d: &Document, i: usize| layer::layer(d).expect("layer").objects()[i].style.width;
    let w0 = width(&d, 0);
    let steps = d.history.undo_len();
    let mut s = annot::DrawState::default();
    assert!(s.select_all(&d));
    // A burst of three steps with the Select tool: both rectangles follow, and so do
    // new rectangles.
    for _ in 0..3 {
        assert!(s.resize_by(Tool::Select, &mut d, 1));
    }
    let w1 = width(&d, 0);
    assert_eq!(w1, w0 + 3.0);
    assert_eq!(width(&d, 1), w1);
    assert_eq!(s.style_for(Tool::Rect).style.width, w1);
    assert_eq!(d.history.undo_len(), steps, "the burst is still open");
    assert!(s.prop_before.is_some());
    // Ending the burst (pause, another action, a click) records exactly one step.
    s.end_gesture(&mut d);
    assert!(s.prop_before.is_none());
    assert_eq!(d.history.undo_len(), steps + 1);
    assert!(d.undo());
    assert_eq!((width(&d, 0), width(&d, 1)), (w0, w0));
    assert!(d.redo());
    assert_eq!(width(&d, 0), w1);
    // Without a burst open, ending one does nothing.
    s.end_gesture(&mut d);
    assert_eq!(d.history.undo_len(), steps + 1);
}

#[test]
fn clone_stamp_and_draw_tools_answer_the_size_gestures() {
    use super::ext::CanvasTool;
    let mut d = doc();
    let mut tool = fx_session::CloneTool::new();
    assert!(tool.resize(&mut d, 1));
    assert!(tool.resize(&mut d, -1));
    assert!(tool.wants_wheel());
    let tools = annot::draw_tools();
    let pencil = tools.iter().find(|t| t.id() == "draw.pencil").expect("pencil");
    assert!(pencil.wants_alt_wheel() && !pencil.wants_wheel());
    let eraser = tools.iter().find(|t| t.id() == "draw.eraser").expect("eraser");
    assert!(eraser.wants_wheel(), "the plain wheel still resizes the eraser");
}
