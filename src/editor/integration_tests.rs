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
    draw::create(Tool::Rect, id, &st, &draw::ToolOptions::for_tool(Tool::Rect), Pt::new(x, 2.0), Pt::new(x + 10.0, 12.0), &[])
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
        assert!(s.align(&mut d, draw::Align::Left));
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
