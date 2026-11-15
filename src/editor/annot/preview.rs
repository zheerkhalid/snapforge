//! Manual harness (ignored tests): drives the real canvas, draw tools, palette and
//! properties bar with synthetic pointer / wheel events in a headless egui context and
//! writes what egui tessellated to PNG files with a small software rasteriser.
//!
//! `cargo test editor::annot::preview -- --ignored --test-threads=1` writes into
//! `$SNAPFORGE_SNAP_DIR` (default `target/editor-previews`). Settings are redirected to a
//! temporary folder first, so the user's settings are neither read nor written.

use super::*;
use crate::common::Image;
use crate::editor::canvas::{self, CanvasCtx, CanvasState};
use crate::editor::commands::ToolId;
use crate::editor::history::{Budget, History};
use crate::editor::style::{self, Palette};
use crate::editor::texture;
use eframe::egui::{Event, Modifiers, PointerButton, Pos2, TextureId};
use std::path::PathBuf;

// ---------------------------------------------------------------------------------- raster

struct Tex {
    w: usize,
    h: usize,
    px: Vec<Color32>,
}

/// Premultiplied RGBA in 0..=255 floats (egui blends in gamma space).
struct Soft {
    w: usize,
    h: usize,
    px: Vec<[f32; 4]>,
}

fn apply_textures(texs: &mut HashMap<TextureId, Tex>, delta: &egui::TexturesDelta) {
    for (id, d) in &delta.set {
        let (w, h) = (d.image.width(), d.image.height());
        let px: Vec<Color32> = match &d.image {
            egui::ImageData::Color(c) => c.pixels.clone(),
            egui::ImageData::Font(f) => f.srgba_pixels(None).collect(),
        };
        match (d.pos, texs.get_mut(id)) {
            (Some([x, y]), Some(t)) => {
                for row in 0..h {
                    let at = (y + row) * t.w + x;
                    t.px[at..at + w].copy_from_slice(&px[row * w..(row + 1) * w]);
                }
            }
            _ => {
                texs.insert(*id, Tex { w, h, px });
            }
        }
    }
}

impl Tex {
    fn texel(&self, x: i64, y: i64) -> [f32; 4] {
        let x = x.clamp(0, self.w as i64 - 1) as usize;
        let y = y.clamp(0, self.h as i64 - 1) as usize;
        let c = self.px[y * self.w + x].to_array();
        [c[0] as f32, c[1] as f32, c[2] as f32, c[3] as f32]
    }

    /// Bilinear sample at a UV, 0..=255 premultiplied.
    fn sample(&self, u: f32, v: f32) -> [f32; 4] {
        let (x, y) = (u * self.w as f32 - 0.5, v * self.h as f32 - 0.5);
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let (a, b, c, d) =
            (self.texel(x0, y0), self.texel(x0 + 1, y0), self.texel(x0, y0 + 1), self.texel(x0 + 1, y0 + 1));
        let mut out = [0.0; 4];
        for i in 0..4 {
            out[i] = (a[i] * (1.0 - fx) + b[i] * fx) * (1.0 - fy) + (c[i] * (1.0 - fx) + d[i] * fx) * fy;
        }
        out
    }
}

impl Soft {
    fn new(w: usize, h: usize, bg: Color32) -> Self {
        let c = bg.to_array();
        Self { w, h, px: vec![[c[0] as f32, c[1] as f32, c[2] as f32, 255.0]; w * h] }
    }

    fn mesh(&mut self, mesh: &egui::Mesh, clip: egui::Rect, ppp: f32, texs: &HashMap<TextureId, Tex>) {
        let Some(tex) = texs.get(&mesh.texture_id) else { return };
        let cx0 = (clip.min.x * ppp).round().max(0.0) as i64;
        let cy0 = (clip.min.y * ppp).round().max(0.0) as i64;
        let cx1 = ((clip.max.x * ppp).round() as i64).min(self.w as i64);
        let cy1 = ((clip.max.y * ppp).round() as i64).min(self.h as i64);
        let edge =
            |a: [f32; 2], b: [f32; 2], p: [f32; 2]| (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]);
        for tri in mesh.indices.chunks_exact(3) {
            let v = [&mesh.vertices[tri[0] as usize], &mesh.vertices[tri[1] as usize], &mesh.vertices[tri[2] as usize]];
            let p = [
                [v[0].pos.x * ppp, v[0].pos.y * ppp],
                [v[1].pos.x * ppp, v[1].pos.y * ppp],
                [v[2].pos.x * ppp, v[2].pos.y * ppp],
            ];
            let area = edge(p[0], p[1], p[2]);
            if area.abs() < 1e-9 {
                continue;
            }
            let min = |i: usize| p[0][i].min(p[1][i]).min(p[2][i]).floor() as i64;
            let max = |i: usize| p[0][i].max(p[1][i]).max(p[2][i]).ceil() as i64;
            let (x0, x1, y0, y1) = (min(0).max(cx0), max(0).min(cx1), min(1).max(cy0), max(1).min(cy1));
            let col: Vec<[f32; 4]> = v
                .iter()
                .map(|v| {
                    let c = v.color.to_array();
                    [c[0] as f32, c[1] as f32, c[2] as f32, c[3] as f32]
                })
                .collect();
            for y in y0..y1 {
                for x in x0..x1 {
                    let s = [x as f32 + 0.5, y as f32 + 0.5];
                    let w = [edge(p[1], p[2], s) / area, edge(p[2], p[0], s) / area, edge(p[0], p[1], s) / area];
                    if w[0] < 0.0 || w[1] < 0.0 || w[2] < 0.0 {
                        continue;
                    }
                    let u = w[0] * v[0].uv.x + w[1] * v[1].uv.x + w[2] * v[2].uv.x;
                    let vv = w[0] * v[0].uv.y + w[1] * v[1].uv.y + w[2] * v[2].uv.y;
                    let t = tex.sample(u, vv);
                    let dst = &mut self.px[y as usize * self.w + x as usize];
                    let mut src = [0.0; 4];
                    for i in 0..4 {
                        src[i] = (w[0] * col[0][i] + w[1] * col[1][i] + w[2] * col[2][i]) * t[i] / 255.0;
                    }
                    let k = 1.0 - src[3] / 255.0;
                    for i in 0..4 {
                        dst[i] = src[i] + dst[i] * k;
                    }
                }
            }
        }
    }

    fn save(&self, name: &str) -> PathBuf {
        let dir = std::env::var("SNAPFORGE_SNAP_DIR").unwrap_or_else(|_| "target/editor-previews".into());
        std::fs::create_dir_all(&dir).expect("preview dir");
        let path = PathBuf::from(dir).join(format!("{name}.png"));
        let bytes: Vec<u8> = self.px.iter().flat_map(|p| p.map(|c| c.round().clamp(0.0, 255.0) as u8)).collect();
        image::save_buffer(&path, &bytes, self.w as u32, self.h as u32, image::ExtendedColorType::Rgba8)
            .expect("save png");
        println!("wrote {}", path.display());
        path
    }
}

// ---------------------------------------------------------------------------------- rig

const SIZE: egui::Vec2 = vec2(1040.0, 620.0);
const PPP: f32 = 1.0;

/// A headless editor: palette, properties bar and canvas of one document.
struct Rig {
    ctx: egui::Context,
    pal: Palette,
    texs: HashMap<TextureId, Tex>,
    doc: Document,
    canvas: CanvasState,
    checker: egui::TextureHandle,
    tool: Tool,
    time: f64,
    mods: Modifiers,
    shapes: Vec<egui::epaint::ClippedShape>,
    cursor: CursorIcon,
}

/// Light and dark areas, flat and busy, to judge the tip on every background.
fn backdrop() -> Image {
    let (w, h) = (900u32, 440u32);
    let mut img = Image::filled(w, h, [0xF6, 0xF4, 0xF1, 255]);
    for y in 0..h {
        for x in 0..w {
            let c = if x >= 600 {
                // Dark panel with stripes.
                if (y / 22) % 2 == 0 {
                    [0x2A, 0x24, 0x20, 255]
                } else {
                    [0x3A, 0x33, 0x2E, 255]
                }
            } else if y >= 300 {
                // A colour ramp.
                let t = x * 255 / 600;
                [(255 - t) as u8, (90 + t / 3) as u8, t as u8, 255]
            } else if (40..560).contains(&x)
                && (40..260).contains(&y)
                && ((y - 40) / 28) % 2 == 1
                && x < 520 - (y % 5) * 30
            {
                [0xC9, 0xC6, 0xC2, 255]
            } else {
                continue;
            };
            img.set_pixel(x, y, c);
        }
    }
    img
}

impl Rig {
    fn new(tool: Tool) -> Self {
        // Never touch the user's settings: redirect before anything reads them.
        let dir = std::env::temp_dir().join("snapforge-editor-preview-settings");
        crate::settings::set_dir_override(dir.clone());
        assert_eq!(crate::settings::dir(), dir, "settings were initialised before the override");
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(PPP);
        let pal = style::apply(&ctx, false);
        let checker = texture::checker_texture(&ctx, false);
        let mut doc = Document::from_file(1, backdrop(), PathBuf::from("preview.png"));
        doc.history = History::new_sync(Budget::default());
        with_state(|s| *s = DrawState::default());
        let mut rig = Self {
            ctx,
            pal,
            texs: HashMap::new(),
            doc,
            canvas: CanvasState::default(),
            checker,
            tool,
            time: 0.0,
            mods: Modifiers::NONE,
            shapes: Vec::new(),
            cursor: CursorIcon::Default,
        };
        for _ in 0..3 {
            rig.frame(Vec::new());
        }
        rig
    }

    fn frame(&mut self, events: Vec<Event>) {
        self.time += 1.0 / 60.0;
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, SIZE)),
            time: Some(self.time),
            modifiers: self.mods,
            events,
            focused: true,
            ..Default::default()
        };
        let Self { ctx, pal, doc, canvas, checker, tool, .. } = self;
        let (tool, pal) = (*tool, *pal);
        let out = ctx.run(raw, |ctx| {
            let frame = egui::Frame::new()
                .fill(pal.bar)
                .inner_margin(egui::Margin { left: 10, right: 10, top: 4, bottom: 5 })
                .stroke(Stroke::new(1.0_f32, pal.border));
            egui::TopBottomPanel::top("draw-props").frame(frame).show_separator_line(false).show(ctx, |ui| {
                with_state(|s| properties_bar(ui, s, Some(&mut *doc), Some(tool), &pal, 0));
            });
            palette_panel(ctx, &pal, Some(tool), true);
            egui::CentralPanel::default().frame(egui::Frame::new().fill(pal.canvas)).show(ctx, |ui| {
                let id = ToolId::External(tool_cmd_id(tool));
                let mut dt = DrawTool { tool, id: tool_cmd_id(tool) };
                let cx =
                    CanvasCtx { tool: &id, external: Some(&mut dt), space_down: false, background: [255; 4], checker };
                canvas::show(ui, doc, canvas, cx);
            });
        });
        apply_textures(&mut self.texs, &out.textures_delta);
        self.cursor = out.platform_output.cursor_icon;
        self.shapes = out.shapes;
    }

    /// Screen position of an image point.
    fn at(&self, x: f32, y: f32) -> Pos2 {
        let map = with_state(|s| s.map).expect("canvas painted");
        let s = map.to_screen([x as f64, y as f64]);
        pos2(s[0], s[1])
    }

    fn hover(&mut self, x: f32, y: f32) {
        let p = self.at(x, y);
        self.frame(vec![Event::PointerMoved(p)]);
        self.frame(Vec::new());
    }

    fn button(&mut self, x: f32, y: f32, pressed: bool) {
        let pos = self.at(x, y);
        let modifiers = self.mods;
        self.frame(vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers }]);
    }

    /// Press at the first point, drag through the others, release.
    fn drag(&mut self, pts: &[(f32, f32)]) {
        let (x, y) = pts[0];
        self.hover(x, y);
        self.button(x, y, true);
        for &(x, y) in &pts[1..] {
            let p = self.at(x, y);
            self.frame(vec![Event::PointerMoved(p)]);
        }
        let (x, y) = pts[pts.len() - 1];
        self.button(x, y, false);
        self.frame(Vec::new());
    }

    /// `notches` of Alt + mouse wheel at the pointer.
    fn alt_wheel(&mut self, notches: i32) {
        self.mods = Modifiers::ALT;
        for _ in 0..notches.abs() {
            let delta = vec2(0.0, notches.signum() as f32);
            self.frame(vec![Event::MouseWheel { unit: egui::MouseWheelUnit::Line, delta, modifiers: Modifiers::ALT }]);
        }
        self.mods = Modifiers::NONE;
        self.frame(Vec::new());
    }

    fn zoom(&mut self, zoom: f32) {
        let img = self.doc.size();
        self.doc.view.set_zoom_centered(zoom, img, [SIZE.x * PPP * 0.9, SIZE.y * PPP * 0.85]);
        self.frame(Vec::new());
        self.frame(Vec::new());
    }

    fn width(&self, tool: Tool) -> f32 {
        with_state(|s| s.style_for(tool).style.width)
    }

    fn objects(&self) -> Vec<Object> {
        layer::layer(&self.doc).map(|l| l.objects().to_vec()).unwrap_or_default()
    }

    fn render(&mut self) -> Soft {
        let mut soft = Soft::new((SIZE.x * PPP) as usize, (SIZE.y * PPP) as usize, self.pal.canvas);
        for prim in self.ctx.tessellate(std::mem::take(&mut self.shapes), PPP) {
            if let egui::epaint::Primitive::Mesh(m) = &prim.primitive {
                soft.mesh(m, prim.clip_rect, PPP, &self.texs);
            }
        }
        soft
    }
}

fn wave(x0: f32, x1: f32, y: f32, amp: f32) -> Vec<(f32, f32)> {
    let n = 40;
    (0..=n)
        .map(|i| {
            let t = i as f32 / n as f32;
            (x0 + (x1 - x0) * t, y + amp * (t * std::f32::consts::TAU * 1.5).sin())
        })
        .collect()
}

fn set_style(tool: Tool, f: impl FnOnce(&mut ToolStyle)) {
    with_state(|s| f(s.style_for(tool)));
}

/// Pencil: strokes at three sizes changed with Alt+wheel, the ring at the end of the
/// last stroke (it must sit exactly on the stroke's round end) and the size label.
#[test]
#[ignore]
fn brush_size_pencil() {
    let mut rig = Rig::new(Tool::Pencil);
    set_style(Tool::Pencil, |ts| {
        *ts = ToolStyle::for_tool(Tool::Pencil);
        ts.style.width = 4.0;
        ts.style.color = draw::Color::rgb(0xE5, 0x39, 0x35);
    });
    rig.drag(&wave(60.0, 840.0, 70.0, 22.0));
    assert_eq!(rig.objects().len(), 1, "one stroke drawn");
    assert!(with_state(|s| s.sel.is_empty()), "brush strokes are not selected");
    // 4 px at 100 %: too small to aim with, so the crosshair stays.
    assert_eq!(rig.cursor, CursorIcon::Crosshair);

    rig.hover(60.0, 170.0);
    rig.alt_wheel(8);
    let w1 = rig.width(Tool::Pencil);
    assert!(w1 > 10.0, "Alt+wheel grew the pencil to {w1}");
    assert_eq!(rig.objects()[0].style.width, 4.0, "the finished stroke keeps its width");
    rig.drag(&wave(60.0, 840.0, 170.0, 26.0));

    rig.hover(60.0, 300.0);
    rig.alt_wheel(7);
    let w2 = rig.width(Tool::Pencil);
    assert!(w2 > w1);
    let pts = wave(60.0, 700.0, 300.0, 30.0);
    rig.drag(&pts);
    assert_eq!(rig.objects().len(), 3);
    assert_eq!(rig.objects()[2].style.width, w2);

    // Hover the end of the last stroke and nudge the size down and up again so that the
    // label shows.
    let (x, y) = pts[pts.len() - 1];
    rig.hover(x, y);
    rig.alt_wheel(-1);
    rig.alt_wheel(1);
    assert_eq!(rig.width(Tool::Pencil), w2, "one notch down and up is where it started");
    assert_eq!(rig.cursor, CursorIcon::None, "the ring replaces the system cursor");
    assert_eq!(rig.doc.view.zoom, 1.0, "Alt+wheel must not zoom");
    println!("pencil widths: 4, {w1}, {w2}");
    rig.render().save("brush-size-pencil");
}

/// Eraser at 200 %: the ring is twice the radius × zoom; plain wheel resizes.
#[test]
#[ignore]
fn brush_size_eraser_zoomed() {
    let mut rig = Rig::new(Tool::Eraser);
    set_style(Tool::Eraser, |ts| {
        *ts = ToolStyle::for_tool(Tool::Eraser);
        ts.options.eraser_radius = 12.0;
    });
    rig.zoom(2.0);
    rig.drag(&[(300.0, 150.0), (380.0, 170.0), (470.0, 150.0)]);
    rig.hover(470.0, 150.0);
    let zoom_before = rig.doc.view.zoom;
    let offset = rig.doc.view.offset;
    // Plain wheel: one notch up.
    rig.frame(vec![Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: vec2(0.0, 1.0),
        modifiers: Modifiers::NONE,
    }]);
    rig.frame(Vec::new());
    let r = with_state(|s| s.style_for(Tool::Eraser).options.eraser_radius);
    assert_eq!(r, 14.0, "12 px + one notch");
    assert_eq!((rig.doc.view.zoom, rig.doc.view.offset), (zoom_before, offset), "the wheel neither zooms nor pans");
    assert_eq!(rig.cursor, CursorIcon::None);
    rig.render().save("brush-size-eraser-200");
}

/// Arrow tool: the stroke dot beside the crosshair, and a selected arrow resized with
/// Alt+wheel as one undo step.
#[test]
#[ignore]
fn brush_size_arrow() {
    let mut rig = Rig::new(Tool::Arrow);
    set_style(Tool::Arrow, |ts| {
        *ts = ToolStyle::for_tool(Tool::Arrow);
        ts.style.width = 4.0;
    });
    rig.drag(&[(120.0, 240.0), (200.0, 200.0), (420.0, 90.0)]);
    let objs = rig.objects();
    assert_eq!(objs.len(), 1);
    assert_eq!(with_state(|s| s.sel.clone()), vec![objs[0].id], "a new arrow stays selected");
    let undo_before = rig.doc.history.undo_len();

    rig.hover(640.0, 200.0);
    rig.alt_wheel(6);
    let w = rig.objects()[0].style.width;
    assert!(w > 8.0, "the selected arrow grew to {w}");
    assert_eq!(rig.width(Tool::Arrow), w, "new arrows follow");
    // The burst is one undo step, recorded once it pauses.
    std::thread::sleep(GESTURE_PAUSE + Duration::from_millis(80));
    rig.frame(Vec::new());
    assert_eq!(rig.doc.history.undo_len(), undo_before + 1, "six notches, one undo step");
    rig.alt_wheel(-1);
    rig.alt_wheel(1);
    assert_eq!(rig.cursor, CursorIcon::Crosshair);
    rig.render().save("brush-size-arrow");
}
