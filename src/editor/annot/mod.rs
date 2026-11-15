//! Phase 3 draw tools in the editor: one [`CanvasTool`] per `draw::Tool` (registered with
//! [`super::register_canvas_tools`]), the [`layer::DrawLayer`] overlay, the left tool
//! palette, the context-sensitive properties bar and in-place text editing.
//!
//! The tools share one [`DrawState`] (selection, drags, per-tool styles), reachable from
//! the tool handles and from the app through [`with_state`]. It is only ever touched on
//! the editor thread; the mutex just satisfies `Send` for the tool registry. Never call
//! [`with_state`] while already inside it.

pub mod keys;
pub mod layer;
mod palette;
mod props;
mod widgets;

use super::document::{DocId, Document};
use super::ext::{CanvasTool, ToolInput};
use super::selection::CanvasMap;
use super::style::ACCENT;
use crate::draw::edit::{self as dedit, objects_in_rect};
use crate::draw::{
    self, create, drag_handle, handle_at, handles, hit_test, translate, Color, DragConstraints, HandleId, HandleKind,
    Kind, Object, ObjectId, Pt, RectF, Tool,
};
use crate::i18n::t;
use crate::settings::{self, draw::ToolStyle};
use eframe::egui::{self, pos2, vec2, Color32, CursorIcon, Stroke};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

pub use palette::{palette_panel, PaletteAction};
pub use props::properties_bar;

/// Command / tool id of a draw tool (`"draw.arrow"`).
pub fn tool_cmd_id(t: Tool) -> String {
    format!("draw.{}", t.id())
}

/// The draw tool behind a canvas tool id.
pub fn tool_of_id(id: &str) -> Option<Tool> {
    id.strip_prefix("draw.").and_then(Tool::from_id)
}

pub fn tool_label(t: Tool) -> String {
    crate::i18n::t(&format!("draw-tool-{}", t.id()))
}

pub fn tool_tip(t: Tool) -> String {
    crate::i18n::t(&format!("draw-tip-{}", t.id()))
}

/// A glyph for menus and the command palette (the tool palette paints vector icons).
fn glyph(t: Tool) -> &'static str {
    match t {
        Tool::Select => "⬉",
        Tool::Text => "T",
        Tool::Callout => "💬",
        Tool::Line => "╱",
        Tool::Arrow => "↗",
        Tool::Curve => "〰",
        Tool::Polyline => "⌇",
        Tool::Elbow => "⌐",
        Tool::Rect => "▭",
        Tool::Ellipse => "◯",
        Tool::Polygon => "⬠",
        Tool::Bracket => "}",
        Tool::Highlighter => "▬",
        Tool::LineHighlighter => "🖍",
        Tool::Magnifier => "🔍",
        Tool::Step => "①",
        Tool::Emoji => "☺",
        Tool::Sticker => "➡",
        Tool::Image => "🖼",
        Tool::Cursor => "🖱",
        Tool::Blur => "💧",
        Tool::Pixelate => "▦",
        Tool::Eraser => "⌫",
        Tool::Fill => "🪣",
        Tool::Pencil => "✏",
    }
}

// ---------------------------------------------------------------------------------- state

static STATE: OnceLock<Mutex<DrawState>> = OnceLock::new();

/// Run `f` on the shared draw state (editor thread only; not re-entrant).
pub fn with_state<R>(f: impl FnOnce(&mut DrawState) -> R) -> R {
    let m = STATE.get_or_init(|| Mutex::new(DrawState::default()));
    let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut g)
}

/// A colour slot the eye dropper / screen picker fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSlot {
    Stroke,
    Fill,
    Text,
    Color2,
    Outline,
    Shadow,
}

/// Work the app does for the draw tools (dialogs, clipboard).
pub enum Request {
    InsertImage { doc: DocId, at: Pt },
}

enum Drag {
    None,
    Create { tool: Tool, id: ObjectId, start: Pt, before: Vec<Object> },
    Freehand { tool: Tool, id: ObjectId, start: Pt, before: Vec<Object> },
    Move { start: Pt, snaps: Vec<Object>, before: Vec<Object> },
    Handle { id: ObjectId, handle: HandleId, start: Pt, snap: Box<Object>, before: Vec<Object> },
    Rubber { start: Pt, cur: Pt, add: bool },
}

struct Poly {
    doc: DocId,
    id: ObjectId,
    tool: Tool,
    before: Vec<Object>,
}

pub struct TextEditing {
    pub doc: DocId,
    pub id: ObjectId,
    pub text: String,
    before: Vec<Object>,
    is_new: bool,
    focus: bool,
}

pub struct DrawState {
    styles: HashMap<Tool, ToolStyle>,
    /// Selected objects of `sel_doc`.
    pub sel: Vec<ObjectId>,
    sel_doc: DocId,
    drag: Drag,
    poly: Option<Poly>,
    pub editing: Option<TextEditing>,
    pub cursor: CursorIcon,
    hover: Option<Pt>,
    hover_obj: Option<ObjectId>,
    /// Last canvas mapping (for overlays drawn by the app).
    pub map: Option<CanvasMap>,
    /// Copied objects (JSON, also put on the system clipboard as text).
    pub clipboard: Option<String>,
    pub fonts: widgets::Fonts,
    /// Objects before a property edit that is still being dragged (one undo step).
    pub prop_before: Option<(DocId, Vec<Object>)>,
    /// The eye dropper waits for a canvas click for this slot.
    pub pick: Option<ColorSlot>,
    /// A picked colour waiting to be applied by the properties bar.
    pub picked: Option<(ColorSlot, Color)>,
    pub requests: Vec<Request>,
    /// Hide the palette (View menu).
    pub palette_hidden: bool,
    /// Colours used since the last save of the recent list.
    recent: Vec<Color>,
    /// The current move / handle drag passed the click threshold.
    drag_moved: bool,
    /// Canvas mapping when the button went down (drags keep using it).
    press_map: Option<CanvasMap>,
}

impl Default for DrawState {
    fn default() -> Self {
        Self {
            styles: HashMap::new(),
            sel: Vec::new(),
            sel_doc: 0,
            drag: Drag::None,
            poly: None,
            editing: None,
            cursor: CursorIcon::Crosshair,
            hover: None,
            hover_obj: None,
            map: None,
            clipboard: None,
            fonts: widgets::Fonts::default(),
            prop_before: None,
            pick: None,
            picked: None,
            requests: Vec::new(),
            palette_hidden: false,
            recent: Vec::new(),
            drag_moved: false,
            press_map: None,
        }
    }
}

fn find(objs: &[Object], id: ObjectId) -> Option<&Object> {
    objs.iter().find(|o| o.id == id)
}

fn replace(objs: &mut [Object], o: Object) {
    if let Some(slot) = objs.iter_mut().find(|x| x.id == o.id) {
        *slot = o;
    }
}

fn step_name(tool: Tool) -> String {
    tool_label(tool)
}

/// Straight colour of a premultiplied BGRA pixel.
fn straight(p: [u8; 4]) -> Color {
    let c = crate::effects::Color::from_premul(p);
    Color::rgba(c.r, c.g, c.b, c.a)
}

impl DrawState {
    /// Current style and options for new objects of `tool`.
    pub fn style_for(&mut self, tool: Tool) -> &mut ToolStyle {
        self.styles.entry(tool).or_insert_with(|| settings::get().draw.style_for(tool))
    }

    /// Persist the style last used with a tool (after an edit, not per frame).
    pub fn remember(&mut self, tool: Tool) {
        let ts = self.style_for(tool).clone();
        if settings::get().draw.last_styles.get(tool.id()) != Some(&ts) {
            settings::update(|s| s.draw.remember(tool, ts));
        }
    }

    pub fn is_dragging(&self) -> bool {
        !matches!(self.drag, Drag::None) || self.poly.is_some()
    }

    fn sync_doc(&mut self, doc: &Document) {
        if doc.id != self.sel_doc {
            self.sel.clear();
            self.sel_doc = doc.id;
            self.drag = Drag::None;
            self.poly = None;
            self.prop_before = None;
        }
        if let Some(l) = layer::layer(doc) {
            let objs = l.objects();
            self.sel.retain(|id| objs.iter().any(|o| o.id == *id));
        } else {
            self.sel.clear();
        }
    }

    /// Selected objects of this document.
    pub fn selection(&mut self, doc: &Document) -> Vec<ObjectId> {
        self.sync_doc(doc);
        self.sel.clone()
    }

    // ------------------------------------------------------------------ canvas input

    pub fn on_input(&mut self, tool: Tool, doc: &mut Document, input: &ToolInput) {
        self.sync_doc(doc);
        self.map = Some(input.map);
        let Some(pf) = input.pos else { return };
        let p = Pt::new(pf[0] as f32, pf[1] as f32);
        let scale = input.map.scale.max(1e-4);
        self.hover = input.hovered.then_some(p);

        if tool == Tool::Eraser && input.scroll != 0.0 {
            let o = &mut self.style_for(tool).options;
            let r = o.eraser_radius;
            o.eraser_radius = (r * if input.scroll > 0.0 { 1.15 } else { 1.0 / 1.15 }).clamp(1.0, 500.0);
        }

        // While the button is held, map the pointer with the canvas mapping of the press:
        // panels appearing on selection (the properties bar) shift the canvas, and the
        // drag must follow the pointer on screen, not jump with the layout.
        let held = |m: CanvasMap| {
            let s = input.map.to_screen(pf);
            let q = m.to_image(s);
            Pt::new(q[0] as f32, q[1] as f32)
        };
        if input.pressed {
            if let Some(slot) = self.pick.take() {
                let (x, y) = (p.x.floor() as i64, p.y.floor() as i64);
                if x >= 0 && y >= 0 && (x as u32) < doc.image.width && (y as u32) < doc.image.height {
                    self.picked = Some((slot, straight(doc.image.pixel(x as u32, y as u32))));
                }
                return;
            }
            self.press_map = Some(input.map);
            self.press(tool, doc, p, input);
        } else if input.down {
            let dp = self.press_map.map(held).unwrap_or(p);
            self.drag_to(doc, dp, input);
        } else if self.poly.is_some() {
            self.poly_hover(doc, p);
        }
        if input.released {
            let rp = self.press_map.take().map(held).unwrap_or(p);
            self.release(doc, rp);
        }
        if input.double_clicked {
            self.double_click(tool, doc, p, scale);
        }
        self.update_cursor(tool, doc, p, scale);
    }

    fn update_cursor(&mut self, tool: Tool, doc: &Document, p: Pt, scale: f32) {
        self.hover_obj = None;
        self.cursor = if self.pick.is_some() {
            CursorIcon::Crosshair
        } else if tool == Tool::Eraser {
            CursorIcon::None
        } else if tool == Tool::Text {
            CursorIcon::Text
        } else {
            CursorIcon::Crosshair
        };
        let Some(l) = layer::layer(doc) else { return };
        let objs = l.objects();
        if self.sel.len() == 1 {
            if let Some(h) = find(objs, self.sel[0]).and_then(|o| handle_at(o, p, 7.0 / scale, scale)) {
                self.cursor = handle_cursor(h);
                return;
            }
        }
        if matches!(self.drag, Drag::Move { .. }) {
            self.cursor = CursorIcon::Grabbing;
            return;
        }
        if tool == Tool::Select || self.sel.iter().any(|id| Some(*id) == hit_test(objs, p, 5.0 / scale)) {
            if let Some(id) = hit_test(objs, p, 5.0 / scale) {
                self.hover_obj = Some(id);
                self.cursor = CursorIcon::Move;
            } else if tool == Tool::Select {
                self.cursor = CursorIcon::Default;
            }
        }
    }

    fn press(&mut self, tool: Tool, doc: &mut Document, p: Pt, input: &ToolInput) {
        let scale = input.map.scale.max(1e-4);
        self.drag_moved = false;
        let shift = input.modifiers.shift;
        if self.editing.is_some() {
            self.finish_text(doc);
            if matches!(tool, Tool::Text | Tool::Callout | Tool::Select) {
                return;
            }
        }
        if let Some(poly) = &self.poly {
            if poly.doc == doc.id {
                let id = poly.id;
                if let Some(l) = layer::layer_mut(doc) {
                    let objs = l.objects_mut();
                    if let Some(o) = objs.iter_mut().find(|o| o.id == id) {
                        if let Some(pts) = poly_points(o) {
                            pts.push(p);
                        }
                        sync_poly_rect(o);
                    }
                }
                return;
            }
            self.poly = None;
        }
        let Some(l) = layer::layer_mut(doc) else { return };
        let objs = l.objects().to_vec();
        // A handle of the single selected object wins with any tool.
        if self.sel.len() == 1 {
            if let Some(o) = find(&objs, self.sel[0]) {
                if let Some(h) = handle_at(o, p, 7.0 / scale, scale) {
                    self.drag =
                        Drag::Handle { id: o.id, handle: h, start: p, snap: Box::new(o.clone()), before: objs.clone() };
                    return;
                }
            }
        }
        let hit = hit_test(&objs, p, 5.0 / scale);
        if tool == Tool::Select {
            match hit {
                Some(id) => {
                    if shift {
                        if let Some(i) = self.sel.iter().position(|x| *x == id) {
                            self.sel.remove(i);
                        } else {
                            self.sel.push(id);
                        }
                    } else if !self.sel.contains(&id) {
                        self.sel = vec![id];
                    }
                    if self.sel.contains(&id) {
                        let snaps = objs.iter().filter(|o| self.sel.contains(&o.id)).cloned().collect();
                        self.drag = Drag::Move { start: p, snaps, before: objs };
                    }
                }
                None => {
                    if !shift {
                        self.sel.clear();
                    }
                    self.drag = Drag::Rubber { start: p, cur: p, add: shift };
                }
            }
            return;
        }
        // Drawing tools: dragging an already selected object moves it.
        if let Some(id) = hit.filter(|id| self.sel.contains(id)) {
            let _ = id;
            let snaps = objs.iter().filter(|o| self.sel.contains(&o.id)).cloned().collect();
            self.drag = Drag::Move { start: p, snaps, before: objs };
            return;
        }
        if tool == Tool::Image {
            self.requests.push(Request::InsertImage { doc: doc.id, at: p });
            return;
        }
        let ts = self.style_for(tool).clone();
        let id = dedit::next_id(&objs);
        let obj = match tool {
            Tool::Polyline => Some(Object::new(
                id,
                Kind::Line(draw::Line {
                    kind: draw::LineKind::Polyline,
                    points: vec![p, p],
                    start: ts.options.head_start,
                    end: ts.options.head_end,
                    head_size: ts.options.head_size,
                    decor: ts.options.decor,
                    color2: ts.options.color2,
                }),
                ts.style.clone(),
            )),
            Tool::Polygon => Some(Object::new(
                id,
                Kind::Shape(draw::Shape {
                    kind: draw::ShapeKind::Polygon,
                    rect: RectF::from_points(p, p),
                    radius: 0.0,
                    points: vec![p, p],
                    facing: ts.options.facing,
                }),
                ts.style.clone(),
            )),
            _ => create(tool, id, &ts.style, &ts.options, p, p, &objs).map(|mut o| {
                if tool.is_freehand() {
                    set_freehand(&mut o, vec![p]);
                }
                o
            }),
        };
        let Some(obj) = obj else { return };
        l.objects_mut().push(obj);
        self.sel = vec![id];
        if tool.is_multi_point() {
            self.poly = Some(Poly { doc: doc.id, id, tool, before: objs });
        } else if tool.is_freehand() {
            self.drag = Drag::Freehand { tool, id, start: p, before: objs };
        } else {
            self.drag = Drag::Create { tool, id, start: p, before: objs };
        }
    }

    fn poly_hover(&mut self, doc: &mut Document, p: Pt) {
        let Some(poly) = &self.poly else { return };
        if poly.doc != doc.id {
            return;
        }
        let id = poly.id;
        if let Some(l) = layer::layer_mut(doc) {
            if let Some(o) = l.objects_mut().iter_mut().find(|o| o.id == id) {
                if let Some(last) = poly_points(o).and_then(|v| v.last_mut()) {
                    *last = p;
                }
                sync_poly_rect(o);
            }
        }
    }

    fn drag_to(&mut self, doc: &mut Document, p: Pt, input: &ToolInput) {
        let m = input.modifiers;
        if self.poly.is_some() {
            self.poly_hover(doc, p);
            return;
        }
        let mut styles = std::mem::take(&mut self.styles);
        let Some(l) = layer::layer_mut(doc) else {
            self.styles = styles;
            return;
        };
        match &mut self.drag {
            Drag::None => {}
            Drag::Create { tool, id, start, .. } => {
                let (tool, id, a) = (*tool, *id, *start);
                let b = constrain(tool, a, p, m.shift);
                let ts = styles.entry(tool).or_insert_with(|| settings::get().draw.style_for(tool));
                let others: Vec<Object> = l.objects().iter().filter(|o| o.id != id).cloned().collect();
                let fill = tool == Tool::Fill;
                let b = if fill { a } else { b };
                if let Some(mut o) = create(tool, id, &ts.style, &ts.options, a, b, &others) {
                    // Keep text typed so far (never happens during a create drag, but cheap).
                    if let (Kind::Text(n), Some(Kind::Text(old))) =
                        (&mut o.kind, find(l.objects(), id).map(|x| &x.kind))
                    {
                        n.text = old.text.clone();
                    }
                    replace(l.objects_mut(), o);
                }
            }
            Drag::Freehand { tool, id, start, .. } => {
                let (tool, id, a) = (*tool, *id, *start);
                let objs = l.objects_mut();
                if let Some(o) = objs.iter_mut().find(|o| o.id == id) {
                    if tool == Tool::LineHighlighter && (m.ctrl || m.command) {
                        let b = if m.shift { dedit::constrain_to_hv(a, p) } else { p };
                        set_freehand(o, vec![a, b]);
                    } else if let Some(pts) = freehand_points(o) {
                        let min = 0.75 / input.map.scale.max(1e-4);
                        if pts.last().map_or(true, |q| q.dist(p) >= min.min(1.0)) {
                            pts.push(p);
                        }
                    }
                }
            }
            Drag::Move { start, .. } | Drag::Handle { start, .. }
                if !self.drag_moved && (p - *start).len() * input.map.scale < 3.0 =>
            {
                // A click, not a drag yet: change nothing.
            }
            Drag::Move { start, snaps, .. } => {
                self.drag_moved = true;
                let mut d = p - *start;
                if m.shift {
                    d = dedit::constrain_to_hv(Pt::default(), d);
                }
                let objs = l.objects_mut();
                for s in snaps.iter() {
                    let mut o = s.clone();
                    translate(&mut o, d);
                    replace(objs, o);
                }
            }
            Drag::Handle { handle, start, snap, .. } => {
                self.drag_moved = true;
                let c = DragConstraints { keep_aspect: m.shift, from_center: m.alt, snap_angle: m.shift };
                let mut o = (**snap).clone();
                drag_handle(&mut o, *handle, p - *start, &c);
                replace(l.objects_mut(), o);
            }
            Drag::Rubber { cur, .. } => *cur = p,
        }
        self.styles = styles;
    }

    fn release(&mut self, doc: &mut Document, p: Pt) {
        match std::mem::replace(&mut self.drag, Drag::None) {
            Drag::None => {}
            Drag::Create { tool, id, before, .. } => {
                if matches!(tool, Tool::Text | Tool::Callout) {
                    self.start_text(doc, id, before, true);
                } else {
                    layer::commit(doc, &step_name(tool), &before);
                    self.remember(tool);
                }
            }
            Drag::Freehand { tool, before, .. } => {
                layer::commit(doc, &step_name(tool), &before);
                self.remember(tool);
            }
            Drag::Move { before, .. } => {
                layer::commit(doc, &t("draw-step-move"), &before);
            }
            Drag::Handle { before, .. } => {
                layer::commit(doc, &t("draw-step-resize"), &before);
            }
            Drag::Rubber { start, add, .. } => {
                if let Some(l) = layer::layer(doc) {
                    let r = RectF::from_points(start, p);
                    if r.w > 0.5 || r.h > 0.5 {
                        let ids = objects_in_rect(l.objects(), r);
                        if add {
                            for id in ids {
                                if !self.sel.contains(&id) {
                                    self.sel.push(id);
                                }
                            }
                        } else {
                            self.sel = ids;
                        }
                    }
                }
            }
        }
    }

    fn double_click(&mut self, tool: Tool, doc: &mut Document, p: Pt, scale: f32) {
        if self.poly.is_some() {
            self.finish_poly(doc);
            return;
        }
        if !matches!(tool, Tool::Select | Tool::Text | Tool::Callout) {
            return;
        }
        let Some(l) = layer::layer(doc) else { return };
        let objs = l.objects().to_vec();
        if let Some(id) = hit_test(&objs, p, 5.0 / scale) {
            if matches!(find(&objs, id).map(|o| &o.kind), Some(Kind::Text(_))) {
                self.sel = vec![id];
                self.start_text(doc, id, objs, false);
            }
        }
    }

    /// Finish a polyline / polygon being built (double-click, Enter, Esc, tool switch).
    pub fn finish_poly(&mut self, doc: &mut Document) {
        let Some(poly) = self.poly.take() else { return };
        if poly.doc != doc.id {
            return;
        }
        let tol = 2.0 / self.map.map(|m| m.scale).unwrap_or(1.0).max(1e-4);
        let mut keep = true;
        if let Some(l) = layer::layer_mut(doc) {
            let objs = l.objects_mut();
            if let Some(o) = objs.iter_mut().find(|o| o.id == poly.id) {
                if let Some(pts) = poly_points(o) {
                    // Drop the rubber-band point and doubled clicks.
                    pts.pop();
                    pts.dedup_by(|a, b| a.dist(*b) < tol);
                    let min = if poly.tool == Tool::Polygon { 3 } else { 2 };
                    keep = pts.len() >= min;
                }
                sync_poly_rect(o);
            }
            if !keep {
                objs.retain(|o| o.id != poly.id);
                self.sel.clear();
            }
        }
        layer::commit(doc, &step_name(poly.tool), &poly.before);
        self.remember(poly.tool);
    }

    /// Finish whatever is in progress (tool switch, Esc).
    pub fn finish_pending(&mut self, doc: &mut Document) {
        self.finish_poly(doc);
        self.finish_text(doc);
        if !matches!(self.drag, Drag::None) {
            let p = self.hover.unwrap_or_default();
            self.release(doc, p);
        }
        self.commit_props(doc);
    }

    /// Esc: finish the current drawing, else clear the object selection. Returns true if
    /// it did something.
    pub fn escape(&mut self, doc: &mut Document) -> bool {
        self.sync_doc(doc);
        if self.poly.is_some() || self.editing.is_some() || !matches!(self.drag, Drag::None) {
            self.finish_pending(doc);
            return true;
        }
        self.pick = None;
        if !self.sel.is_empty() {
            self.sel.clear();
            return true;
        }
        false
    }

    // ------------------------------------------------------------------ text

    fn start_text(&mut self, doc: &mut Document, id: ObjectId, before: Vec<Object>, is_new: bool) {
        let Some(l) = layer::layer_mut(doc) else { return };
        let text = match find(l.objects(), id).map(|o| &o.kind) {
            Some(Kind::Text(t)) => t.text.clone(),
            _ => return,
        };
        l.hidden = vec![id];
        l.objects_mut();
        self.editing = Some(TextEditing { doc: doc.id, id, text, before, is_new, focus: true });
    }

    /// Commit in-place text editing as one undo step (an empty new text is dropped).
    pub fn finish_text(&mut self, doc: &mut Document) {
        let Some(ed) = self.editing.take() else { return };
        if ed.doc != doc.id {
            return;
        }
        let Some(l) = layer::layer_mut(doc) else { return };
        l.hidden.clear();
        let objs = l.objects_mut();
        let tool = find(objs, ed.id).map(Tool::of).unwrap_or(Tool::Text);
        let empty = ed.text.trim().is_empty();
        if empty {
            objs.retain(|o| o.id != ed.id);
            self.sel.retain(|id| *id != ed.id);
        } else if let Some(Kind::Text(t)) = objs.iter_mut().find(|o| o.id == ed.id).map(|o| &mut o.kind) {
            t.text = ed.text.clone();
        }
        layer::commit(doc, &step_name(tool), &ed.before);
        if ed.is_new && !empty {
            self.remember(tool);
        }
    }

    /// The in-place text editor over the object being edited (call after the canvas).
    pub fn text_edit_ui(&mut self, ctx: &egui::Context, doc: &mut Document) {
        let Some(ed) = self.editing.as_mut() else { return };
        if ed.doc != doc.id {
            self.editing = None;
            return;
        }
        let Some(map) = self.map else { return };
        let Some(l) = layer::layer_mut(doc) else { return };
        let Some(obj) = find(l.objects(), ed.id).cloned() else {
            self.editing = None;
            return;
        };
        let Kind::Text(text) = &obj.kind else { return };
        let frame = dedit::frame(&obj);
        let s0 = map.to_screen([frame.x as f64, frame.y as f64]);
        let font_pts = (obj.style.font.px() * map.scale).clamp(8.0, 160.0);
        let width = (frame.w * map.scale).max(120.0);
        let fc = obj.style.font.color;
        let color = Color32::from_rgba_unmultiplied(fc.r, fc.g, fc.b, 255);
        let bg = if fc.luma() > 0.6 {
            Color32::from_rgba_unmultiplied(30, 32, 38, 235)
        } else {
            Color32::from_rgba_unmultiplied(255, 255, 255, 240)
        };
        let pad = (text.padding * map.scale).clamp(2.0, 16.0);
        let mut finished = false;
        let before_text = ed.text.clone();
        egui::Area::new(egui::Id::new(("draw-text-edit", ed.id)))
            .fixed_pos(pos2(s0[0], s0[1]))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(bg)
                    .stroke(Stroke::new(1.5_f32, ACCENT))
                    .corner_radius(4.0)
                    .inner_margin(pad)
                    .show(ui, |ui| {
                        let te = egui::TextEdit::multiline(&mut ed.text)
                            .font(egui::FontId::proportional(font_pts))
                            .text_color(color)
                            .desired_width(width - 2.0 * pad)
                            .desired_rows(1)
                            .frame(false)
                            .hint_text(t("draw-text-hint"));
                        let r = ui.add(te);
                        if ed.focus {
                            r.request_focus();
                            ed.focus = false;
                        } else if r.lost_focus() {
                            finished = true;
                        }
                    });
            });
        let ctrl_enter = ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter));
        if ed.text != before_text {
            let (id, txt) = (ed.id, ed.text.clone());
            if let Some(Kind::Text(t)) = l.objects_mut().iter_mut().find(|o| o.id == id).map(|o| &mut o.kind) {
                t.text = txt;
            }
        }
        if finished || ctrl_enter {
            self.finish_text(doc);
        }
    }

    // ------------------------------------------------------------------ painting

    pub fn paint(&mut self, tool: Tool, doc: &Document, painter: &egui::Painter, map: &CanvasMap) {
        self.map = Some(*map);
        let scale = map.scale;
        let to = |p: Pt| {
            let s = map.to_screen([p.x as f64, p.y as f64]);
            pos2(s[0], s[1])
        };
        let objs: &[Object] = layer::layer(doc).map(|l| l.objects()).unwrap_or(&[]);
        let editing = self.editing.as_ref().map(|e| e.id);
        // Hover outline.
        if let Some(id) = self.hover_obj.filter(|id| !self.sel.contains(id)) {
            if let Some(o) = find(objs, id) {
                let b = dedit::bounds(o);
                painter.rect_stroke(
                    egui::Rect::from_min_max(to(b.tl()), to(b.br())),
                    2.0,
                    Stroke::new(1.0_f32, ACCENT.gamma_multiply(0.7)),
                    egui::StrokeKind::Outside,
                );
            }
        }
        // Selection box and handles.
        let sel: Vec<&Object> = objs.iter().filter(|o| self.sel.contains(&o.id) && Some(o.id) != editing).collect();
        for o in &sel {
            let b = dedit::bounds(o);
            dashed_rect(painter, egui::Rect::from_min_max(to(b.tl()), to(b.br())).expand(2.0));
        }
        if sel.len() == 1 && self.poly.is_none() {
            let o = sel[0];
            let hs = handles(o, scale);
            // Guides from curve controls and the rotate knob.
            for h in &hs {
                match h.kind {
                    HandleKind::Control => {
                        if let Kind::Line(l) = &o.kind {
                            for q in [l.points.first(), l.points.last()].into_iter().flatten() {
                                painter.line_segment(
                                    [to(*q), to(h.pos)],
                                    Stroke::new(1.0_f32, ACCENT.gamma_multiply(0.6)),
                                );
                            }
                        }
                    }
                    HandleKind::Rotate => {
                        let f = dedit::frame(o);
                        let top = Pt::new(f.center().x, f.y);
                        let rot = match &o.kind {
                            Kind::Emoji(e) => e.frame.rotation,
                            Kind::Sticker(s) => s.frame.rotation,
                            Kind::Image(p) => p.frame.rotation,
                            Kind::Cursor(c) => c.frame.rotation,
                            _ => 0.0,
                        };
                        let top = top.rotate_around(f.center(), rot);
                        painter.line_segment([to(top), to(h.pos)], Stroke::new(1.0_f32, ACCENT));
                    }
                    _ => {}
                }
            }
            for h in hs {
                paint_handle(painter, to(h.pos), h.kind);
            }
        }
        if let Drag::Rubber { start, cur, .. } = &self.drag {
            let r = egui::Rect::from_two_pos(to(*start), to(*cur));
            painter.rect_filled(r, 0.0, ACCENT.gamma_multiply(0.12));
            painter.rect_stroke(r, 0.0, Stroke::new(1.0_f32, ACCENT), egui::StrokeKind::Inside);
        }
        if tool == Tool::Eraser {
            if let Some(h) = self.hover {
                let r = self.styles.get(&Tool::Eraser).map(|s| s.options.eraser_radius).unwrap_or(10.0) * scale;
                painter.circle_stroke(to(h), r.max(2.0), Stroke::new(2.5_f32, Color32::from_black_alpha(140)));
                painter.circle_stroke(to(h), r.max(2.0), Stroke::new(1.0_f32, Color32::WHITE));
            }
        }
    }

    // ------------------------------------------------------------------ object commands

    pub fn has_selection(&mut self, doc: &Document) -> bool {
        self.sync_doc(doc);
        !self.sel.is_empty()
    }

    pub fn delete_selected(&mut self, doc: &mut Document) -> bool {
        let ids = self.selection(doc);
        if ids.is_empty() {
            return false;
        }
        let ok = layer::edit(doc, &t("draw-step-delete"), |objs| !dedit::delete(objs, &ids).is_empty());
        self.sel.clear();
        ok
    }

    /// JSON of the selected objects (None when nothing is selected).
    pub fn copy_selected(&mut self, doc: &Document) -> Option<String> {
        let ids = self.selection(doc);
        if ids.is_empty() {
            return None;
        }
        let json = dedit::serialize_selection(layer::layer(doc)?.objects(), &ids);
        self.clipboard = Some(json.clone());
        Some(json)
    }

    /// Paste objects from JSON; true if it was annotation JSON.
    pub fn paste_json(&mut self, doc: &mut Document, json: &str) -> bool {
        if !json.contains("\"objects\"") {
            return false;
        }
        let off = settings::get().draw.paste_offset;
        let mut ids = Vec::new();
        let ok = layer::edit(doc, &t("editor-paste"), |objs| match dedit::paste(objs, json, Pt::new(off, off)) {
            Ok(v) => {
                ids = v;
                !ids.is_empty()
            }
            Err(e) => {
                log::debug!("draw: paste: {e:#}");
                false
            }
        });
        if ok {
            self.sel_doc = doc.id;
            self.sel = ids;
        }
        ok
    }

    pub fn duplicate(&mut self, doc: &mut Document) -> bool {
        let ids = self.selection(doc);
        let off = settings::get().draw.paste_offset;
        let mut new = Vec::new();
        let ok = layer::edit(doc, &t("draw-step-duplicate"), |objs| {
            new = dedit::duplicate(objs, &ids, Pt::new(off, off));
            !new.is_empty()
        });
        if ok {
            self.sel = new;
        }
        ok
    }

    pub fn select_all(&mut self, doc: &Document) -> bool {
        self.sync_doc(doc);
        let ids: Vec<ObjectId> =
            layer::layer(doc).map(|l| l.objects().iter().map(|o| o.id).collect()).unwrap_or_default();
        let any = !ids.is_empty();
        self.sel = ids;
        any
    }

    pub fn nudge(&mut self, doc: &mut Document, dx: f32, dy: f32) -> bool {
        let ids = self.selection(doc);
        if ids.is_empty() {
            return false;
        }
        layer::edit(doc, &t("draw-step-move"), |objs| dedit::move_objects(objs, &ids, Pt::new(dx, dy)) > 0)
    }

    pub fn align(&mut self, doc: &mut Document, how: dedit::Align) -> bool {
        let ids = self.selection(doc);
        layer::edit(doc, &t("draw-step-align"), |objs| dedit::align(objs, &ids, how))
    }

    pub fn order(&mut self, doc: &mut Document, how: Order) -> bool {
        let ids = self.selection(doc);
        layer::edit(doc, &t("draw-step-order"), |objs| match how {
            Order::Front => dedit::bring_to_front(objs, &ids),
            Order::Back => dedit::send_to_back(objs, &ids),
            Order::Forward => dedit::bring_forward(objs, &ids),
            Order::Backward => dedit::send_backward(objs, &ids),
        })
    }

    /// Insert an image object (PNG bytes) with its top-left at `at`, selected.
    pub fn insert_picture(&mut self, doc: &mut Document, png: Vec<u8>, size: (u32, u32), at: Pt) -> bool {
        let max_w = (doc.image.width as f32 * 0.9).max(16.0);
        let mut id = 0;
        let ok = layer::edit(doc, &tool_label(Tool::Image), |objs| {
            id = dedit::next_id(objs);
            objs.push(draw::create_picture(id, png, size, at, Some(max_w)));
            true
        });
        if ok {
            self.sel_doc = doc.id;
            self.sel = vec![id];
        }
        ok
    }

    /// Record a property edit that has finished (pointer released).
    pub fn commit_props(&mut self, doc: &mut Document) {
        if let Some((d, before)) = self.prop_before.take() {
            if d == doc.id {
                layer::commit(doc, &t("draw-step-style"), &before);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    Front,
    Back,
    Forward,
    Backward,
}

fn constrain(tool: Tool, a: Pt, b: Pt, shift: bool) -> Pt {
    if !shift {
        return b;
    }
    match tool {
        Tool::Line | Tool::Arrow | Tool::Curve | Tool::Elbow => dedit::constrain_to_axis(a, b),
        _ => {
            // Squares and circles.
            let d = b - a;
            let s = d.x.abs().max(d.y.abs());
            Pt::new(a.x + s * d.x.signum(), a.y + s * d.y.signum())
        }
    }
}

fn poly_points(o: &mut Object) -> Option<&mut Vec<Pt>> {
    match &mut o.kind {
        Kind::Line(l) => Some(&mut l.points),
        Kind::Shape(s) => Some(&mut s.points),
        _ => None,
    }
}

fn sync_poly_rect(o: &mut Object) {
    if let Kind::Shape(s) = &mut o.kind {
        if !s.points.is_empty() {
            s.rect = RectF::bounding(&s.points);
        }
    }
}

fn freehand_points(o: &mut Object) -> Option<&mut Vec<Pt>> {
    match &mut o.kind {
        Kind::Pencil(p) => Some(&mut p.points),
        Kind::Eraser(e) => Some(&mut e.points),
        Kind::Highlight(h) => Some(&mut h.points),
        _ => None,
    }
}

fn set_freehand(o: &mut Object, pts: Vec<Pt>) {
    if let Some(v) = freehand_points(o) {
        *v = pts;
    }
}

fn handle_cursor(h: HandleId) -> CursorIcon {
    use crate::draw::Dir;
    match h {
        HandleId::Resize(d) | HandleId::Source(d) => match d {
            Dir::N | Dir::S => CursorIcon::ResizeVertical,
            Dir::E | Dir::W => CursorIcon::ResizeHorizontal,
            Dir::NE | Dir::SW => CursorIcon::ResizeNeSw,
            Dir::NW | Dir::SE => CursorIcon::ResizeNwSe,
        },
        HandleId::Rotate => CursorIcon::Alias,
        HandleId::SourceMove => CursorIcon::Move,
        _ => CursorIcon::PointingHand,
    }
}

fn paint_handle(painter: &egui::Painter, c: egui::Pos2, kind: HandleKind) {
    let dark = Stroke::new(1.0_f32, Color32::from_black_alpha(160));
    match kind {
        HandleKind::Resize => {
            let r = egui::Rect::from_center_size(c, vec2(8.0, 8.0));
            painter.rect_filled(r, 1.5, Color32::WHITE);
            painter.rect_stroke(r, 1.5, Stroke::new(1.5_f32, ACCENT), egui::StrokeKind::Inside);
        }
        HandleKind::Vertex => {
            painter.circle_filled(c, 5.0, Color32::WHITE);
            painter.circle_stroke(c, 5.0, Stroke::new(1.5_f32, ACCENT));
        }
        HandleKind::Control => {
            painter.circle_filled(c, 4.5, ACCENT);
            painter.circle_stroke(c, 4.5, Stroke::new(1.0_f32, Color32::WHITE));
        }
        HandleKind::Rotate => {
            painter.circle_filled(c, 5.5, ACCENT);
            painter.circle_stroke(c, 5.5, Stroke::new(1.5_f32, Color32::WHITE));
        }
        HandleKind::Special => {
            let s = 5.5;
            let pts = vec![c + vec2(0.0, -s), c + vec2(s, 0.0), c + vec2(0.0, s), c + vec2(-s, 0.0)];
            painter.add(egui::Shape::convex_polygon(pts, Color32::from_rgb(0xFF, 0xB0, 0x20), dark));
        }
    }
}

fn dashed_rect(painter: &egui::Painter, r: egui::Rect) {
    let pts = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
    painter.add(egui::Shape::line(pts.to_vec(), Stroke::new(1.0_f32, Color32::WHITE)));
    painter.extend(egui::Shape::dashed_line(&pts, Stroke::new(1.0_f32, ACCENT), 4.0, 3.0));
}

// ---------------------------------------------------------------------------------- tools

/// The canvas tool for one draw tool; all of them share [`DrawState`].
struct DrawTool {
    tool: Tool,
    id: String,
}

impl CanvasTool for DrawTool {
    fn id(&self) -> &str {
        &self.id
    }
    fn label(&self) -> String {
        tool_label(self.tool)
    }
    fn icon(&self) -> &'static str {
        glyph(self.tool)
    }
    fn shortcut(&self) -> Option<&str> {
        keys::shortcut(self.tool)
    }
    fn cursor(&self) -> CursorIcon {
        with_state(|s| s.cursor)
    }
    fn on_input(&mut self, doc: &mut Document, input: &ToolInput) {
        let tool = self.tool;
        with_state(|s| s.on_input(tool, doc, input));
    }
    fn paint(&mut self, doc: &Document, painter: &egui::Painter, map: &CanvasMap) {
        let tool = self.tool;
        with_state(|s| s.paint(tool, doc, painter, map));
    }
    fn deactivate(&mut self, doc: &mut Document) {
        with_state(|s| s.finish_pending(doc));
    }
    fn wants_wheel(&self) -> bool {
        self.tool == Tool::Eraser
    }
    fn on_ribbon(&self) -> bool {
        false
    }
}

/// Factory for [`super::register_canvas_tools`]: every draw tool, in palette order.
pub fn draw_tools() -> Vec<Box<dyn CanvasTool>> {
    keys::ORDER.iter().map(|&t| Box::new(DrawTool { tool: t, id: tool_cmd_id(t) }) as Box<dyn CanvasTool>).collect()
}
