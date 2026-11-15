//! The effect table behind the Image and Effects menus.
//!
//! An [`EffectDef`] names an operation, places it in a menu, optionally describes its
//! parameters with a small schema ([`ParamDef`]) and provides a pure apply function. The
//! editor turns the schema into a modal dialog with live preview on a downscaled proxy
//! ([`super::param_dialog`]), runs the final apply on a worker thread and records it as
//! one undo step. With a selection, `Scope::SelectionOrImage` effects apply to the
//! selected pixels only.
//!
//! The effects module (integration pass) registers real operations with
//! [`super::register_effects`]; a registration replaces the placeholder with the same id.

use crate::common::Image;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Which top-level menu an effect lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MenuSlot {
    Image,
    Effects,
}

/// What an effect operates on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Always the whole image (resize, rotate, canvas size…).
    WholeImage,
    /// The selection if there is one (same-size result required), else the whole image.
    SelectionOrImage,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParamKind {
    Float {
        min: f32,
        max: f32,
        default: f32,
        step: f32,
        decimals: usize,
        suffix: &'static str,
        logarithmic: bool,
    },
    Int {
        min: i32,
        max: i32,
        default: i32,
        suffix: &'static str,
    },
    Bool {
        default: bool,
    },
    /// Straight (not premultiplied) RGBA.
    Color {
        default: [u8; 4],
        alpha: bool,
    },
    Choice {
        options: Vec<String>,
        default: usize,
    },
    /// Free text (captions, watermarks).
    Text {
        default: String,
        multiline: bool,
    },
}

impl ParamKind {
    pub fn default_value(&self) -> ParamValue {
        match self {
            ParamKind::Float { default, .. } => ParamValue::Float(*default),
            ParamKind::Int { default, .. } => ParamValue::Int(*default),
            ParamKind::Bool { default } => ParamValue::Bool(*default),
            ParamKind::Color { default, .. } => ParamValue::Color(*default),
            ParamKind::Choice { default, .. } => ParamValue::Choice(*default),
            ParamKind::Text { default, .. } => ParamValue::Text(default.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParamDef {
    pub id: &'static str,
    pub label: String,
    pub kind: ParamKind,
    /// Shown under "More…" (dense options stay out of the way).
    pub advanced: bool,
    pub tooltip: Option<String>,
}

impl ParamDef {
    pub fn float(id: &'static str, label: impl Into<String>, min: f32, max: f32, default: f32) -> Self {
        let step = ((max - min) / 200.0).max(0.01);
        let decimals = if step >= 1.0 {
            0
        } else if step >= 0.1 {
            1
        } else {
            2
        };
        Self::new(id, label, ParamKind::Float { min, max, default, step, decimals, suffix: "", logarithmic: false })
    }
    pub fn int(id: &'static str, label: impl Into<String>, min: i32, max: i32, default: i32) -> Self {
        Self::new(id, label, ParamKind::Int { min, max, default, suffix: "" })
    }
    pub fn bool(id: &'static str, label: impl Into<String>, default: bool) -> Self {
        Self::new(id, label, ParamKind::Bool { default })
    }
    pub fn color(id: &'static str, label: impl Into<String>, default: [u8; 4], alpha: bool) -> Self {
        Self::new(id, label, ParamKind::Color { default, alpha })
    }
    pub fn choice(id: &'static str, label: impl Into<String>, options: &[&str], default: usize) -> Self {
        Self::new(id, label, ParamKind::Choice { options: options.iter().map(|s| s.to_string()).collect(), default })
    }
    pub fn text(id: &'static str, label: impl Into<String>, default: &str, multiline: bool) -> Self {
        Self::new(id, label, ParamKind::Text { default: default.to_string(), multiline })
    }
    fn new(id: &'static str, label: impl Into<String>, kind: ParamKind) -> Self {
        Self { id, label: label.into(), kind, advanced: false, tooltip: None }
    }
    /// Put this parameter under "More…".
    pub fn advanced(mut self) -> Self {
        self.advanced = true;
        self
    }
    pub fn suffix(mut self, s: &'static str) -> Self {
        match &mut self.kind {
            ParamKind::Float { suffix, .. } | ParamKind::Int { suffix, .. } => *suffix = s,
            _ => {}
        }
        self
    }
    pub fn tooltip(mut self, t: impl Into<String>) -> Self {
        self.tooltip = Some(t.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParamValue {
    Float(f32),
    Int(i32),
    Bool(bool),
    Color([u8; 4]),
    Choice(usize),
    Text(String),
}

/// Parameter values by id. Getters fall back to a neutral value if the id is missing
/// or has another type, so apply functions never panic.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParamValues(pub BTreeMap<&'static str, ParamValue>);

impl ParamValues {
    pub fn defaults(params: &[ParamDef]) -> Self {
        Self(params.iter().map(|p| (p.id, p.kind.default_value())).collect())
    }
    pub fn set(&mut self, id: &'static str, v: ParamValue) {
        self.0.insert(id, v);
    }
    pub fn f32(&self, id: &str) -> f32 {
        match self.0.get(id) {
            Some(ParamValue::Float(v)) => *v,
            Some(ParamValue::Int(v)) => *v as f32,
            _ => 0.0,
        }
    }
    pub fn i32(&self, id: &str) -> i32 {
        match self.0.get(id) {
            Some(ParamValue::Int(v)) => *v,
            Some(ParamValue::Float(v)) => v.round() as i32,
            Some(ParamValue::Choice(v)) => *v as i32,
            _ => 0,
        }
    }
    pub fn bool(&self, id: &str) -> bool {
        matches!(self.0.get(id), Some(ParamValue::Bool(true)))
    }
    /// Straight RGBA.
    pub fn color(&self, id: &str) -> [u8; 4] {
        match self.0.get(id) {
            Some(ParamValue::Color(c)) => *c,
            _ => [0, 0, 0, 255],
        }
    }
    pub fn choice(&self, id: &str) -> usize {
        match self.0.get(id) {
            Some(ParamValue::Choice(v)) => *v,
            _ => 0,
        }
    }
    pub fn text(&self, id: &str) -> String {
        match self.0.get(id) {
            Some(ParamValue::Text(v)) => v.clone(),
            _ => String::new(),
        }
    }
}

/// Context for one apply call.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectCtx {
    /// Size of the input relative to the real image (1.0 for the final apply, < 1 for
    /// the preview proxy). Multiply pixel distances (radius, offsets) by this.
    pub scale: f32,
    pub preview: bool,
    /// Background colour (premultiplied BGRA) for areas an effect uncovers.
    pub background: [u8; 4],
}

pub type ApplyFn = Arc<dyn Fn(&Image, &ParamValues, &EffectCtx) -> Image + Send + Sync>;

#[derive(Clone)]
pub struct EffectDef {
    /// Stable id, e.g. `"image.resize"`, `"effects.blur"`.
    pub id: &'static str,
    /// Menu label (an ellipsis is added automatically when the effect has parameters).
    pub label: String,
    pub menu: MenuSlot,
    /// Items with the same group are kept together; groups are separated in the menu.
    pub group: &'static str,
    /// e.g. `"Ctrl+R"`; parsed by the command registry.
    pub shortcut: Option<&'static str>,
    pub icon: &'static str,
    pub scope: Scope,
    /// `None`: apply immediately (no dialog).
    pub params: Option<Vec<ParamDef>>,
    pub apply: ApplyFn,
    /// Not implemented yet: the dialog opens, but Apply only explains that.
    pub placeholder: bool,
}

impl EffectDef {
    pub fn new(
        id: &'static str,
        label: impl Into<String>,
        menu: MenuSlot,
        group: &'static str,
        apply: impl Fn(&Image, &ParamValues, &EffectCtx) -> Image + Send + Sync + 'static,
    ) -> Self {
        Self {
            id,
            label: label.into(),
            menu,
            group,
            shortcut: None,
            icon: "",
            scope: Scope::SelectionOrImage,
            params: None,
            apply: Arc::new(apply),
            placeholder: false,
        }
    }
    pub fn with_params(mut self, p: Vec<ParamDef>) -> Self {
        self.params = Some(p);
        self
    }
    pub fn with_shortcut(mut self, s: &'static str) -> Self {
        self.shortcut = Some(s);
        self
    }
    pub fn with_icon(mut self, i: &'static str) -> Self {
        self.icon = i;
        self
    }
    pub fn whole_image(mut self) -> Self {
        self.scope = Scope::WholeImage;
        self
    }
    pub fn menu_label(&self) -> String {
        if self.params.is_some() {
            format!("{}…", self.label)
        } else {
            self.label.clone()
        }
    }
    pub fn command_id(&self) -> String {
        format!("effect:{}", self.id)
    }
}

/// The effect table, in registration order.
#[derive(Clone, Default)]
pub struct EffectRegistry {
    defs: Vec<EffectDef>,
}

impl EffectRegistry {
    /// Add an effect, replacing any existing one with the same id (keeping its place).
    pub fn register(&mut self, def: EffectDef) {
        match self.defs.iter_mut().find(|d| d.id == def.id) {
            Some(slot) => *slot = def,
            None => self.defs.push(def),
        }
    }
    pub fn get(&self, id: &str) -> Option<&EffectDef> {
        self.defs.iter().find(|d| d.id == id)
    }
    pub fn iter(&self) -> impl Iterator<Item = &EffectDef> {
        self.defs.iter()
    }
    /// Effects of one menu, grouped (group order = first appearance).
    pub fn menu(&self, slot: MenuSlot) -> Vec<Vec<&EffectDef>> {
        let mut groups: Vec<(&str, Vec<&EffectDef>)> = Vec::new();
        for d in self.defs.iter().filter(|d| d.menu == slot) {
            match groups.iter_mut().find(|(g, _)| *g == d.group) {
                Some((_, v)) => v.push(d),
                None => groups.push((d.group, vec![d])),
            }
        }
        groups.into_iter().map(|(_, v)| v).collect()
    }
    pub fn len(&self) -> usize {
        self.defs.len()
    }
    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    /// The menu skeleton until the effects module registers the real operations.
    pub fn with_placeholders() -> Self {
        use MenuSlot::{Effects as E, Image as I};
        let mut r = Self::default();
        let mut ph =
            |id, label: &str, menu, group, params: Option<Vec<ParamDef>>, shortcut: Option<&'static str>, whole| {
                let mut d = EffectDef::new(id, label, menu, group, |img, _, _| img.clone());
                d.params = params;
                d.shortcut = shortcut;
                d.placeholder = true;
                if whole {
                    d.scope = Scope::WholeImage;
                }
                r.register(d);
            };
        let pct = |id, l: &str, def| ParamDef::float(id, l, 1.0, 400.0, def).suffix("%");
        ph(
            "image.resize",
            "Resize",
            I,
            "size",
            Some(vec![
                pct("width", "Width", 100.0),
                pct("height", "Height", 100.0),
                ParamDef::bool("keep_ratio", "Keep aspect ratio", true),
                ParamDef::choice("filter", "Resampling", &["Lanczos", "Bicubic", "Bilinear", "Nearest"], 0).advanced(),
            ]),
            Some("Ctrl+R"),
            true,
        );
        ph(
            "image.canvas_size",
            "Canvas Size",
            I,
            "size",
            Some(vec![
                ParamDef::int("add_left", "Left", -4000, 4000, 0).suffix(" px"),
                ParamDef::int("add_top", "Top", -4000, 4000, 0).suffix(" px"),
                ParamDef::int("add_right", "Right", -4000, 4000, 0).suffix(" px"),
                ParamDef::int("add_bottom", "Bottom", -4000, 4000, 0).suffix(" px"),
                ParamDef::color("fill", "Fill", [255, 255, 255, 255], true),
            ]),
            None,
            true,
        );
        ph("image.rotate_left", "Rotate Left", I, "rotate", None, Some("Ctrl+L"), true);
        ph("image.rotate_right", "Rotate Right", I, "rotate", None, None, true);
        ph(
            "image.rotate",
            "Rotate by Angle",
            I,
            "rotate",
            Some(vec![
                ParamDef::float("angle", "Angle", -180.0, 180.0, 0.0).suffix("°"),
                ParamDef::bool("expand", "Expand canvas", true),
                ParamDef::color("fill", "Fill", [255, 255, 255, 0], true).advanced(),
            ]),
            None,
            true,
        );
        ph("image.flip_h", "Flip Horizontal", I, "flip", None, None, false);
        ph("image.flip_v", "Flip Vertical", I, "flip", None, None, false);
        ph(
            "image.color_reduction",
            "Color Reduction",
            I,
            "color",
            Some(vec![
                ParamDef::choice("colors", "Colors", &["2", "16", "256", "Grayscale"], 2),
                ParamDef::bool("dither", "Dither", true),
            ]),
            None,
            false,
        );
        ph(
            "image.transparency",
            "Transparency",
            I,
            "color",
            Some(vec![
                ParamDef::color("key", "Make color transparent", [255, 255, 255, 255], false),
                ParamDef::int("tolerance", "Tolerance", 0, 255, 0),
            ]),
            None,
            false,
        );
        ph(
            "effects.adjust",
            "Adjust Colors",
            E,
            "color",
            Some(vec![
                ParamDef::float("brightness", "Brightness", -100.0, 100.0, 0.0),
                ParamDef::float("contrast", "Contrast", -100.0, 100.0, 0.0),
                ParamDef::float("gamma", "Gamma", 0.2, 5.0, 1.0),
                ParamDef::float("saturation", "Saturation", -100.0, 100.0, 0.0),
                ParamDef::float("hue", "Hue", -180.0, 180.0, 0.0).suffix("°").advanced(),
            ]),
            Some("Ctrl+Shift+A"),
            false,
        );
        ph(
            "effects.sharpen",
            "Sharpen",
            E,
            "filter",
            Some(vec![ParamDef::float("amount", "Amount", 0.0, 100.0, 30.0)]),
            None,
            false,
        );
        ph(
            "effects.blur",
            "Blur",
            E,
            "filter",
            Some(vec![
                ParamDef::float("radius", "Radius", 0.5, 50.0, 4.0).suffix(" px"),
                ParamDef::choice("kind", "Kind", &["Gaussian", "Box", "Pixelate"], 0),
            ]),
            None,
            false,
        );
        ph(
            "effects.spotlight",
            "Spotlight",
            E,
            "decorate",
            Some(vec![ParamDef::float("dim", "Dim outside", 0.0, 100.0, 50.0).suffix("%")]),
            None,
            true,
        );
        ph(
            "effects.shadow",
            "Drop Shadow",
            E,
            "decorate",
            Some(vec![
                ParamDef::float("size", "Size", 0.0, 60.0, 12.0).suffix(" px"),
                ParamDef::float("opacity", "Opacity", 0.0, 100.0, 50.0).suffix("%"),
                ParamDef::color("color", "Color", [0, 0, 0, 255], false).advanced(),
                ParamDef::int("dx", "Offset X", -50, 50, 6).advanced(),
                ParamDef::int("dy", "Offset Y", -50, 50, 6).advanced(),
            ]),
            None,
            true,
        );
        ph(
            "effects.frame",
            "Frame",
            E,
            "decorate",
            Some(vec![
                ParamDef::int("width", "Width", 1, 100, 8),
                ParamDef::color("color", "Color", [40, 40, 40, 255], true),
            ]),
            None,
            true,
        );
        ph(
            "effects.torn_edge",
            "Torn Edge",
            E,
            "decorate",
            Some(vec![
                ParamDef::choice("edges", "Edges", &["Bottom", "Right", "Bottom & Right", "All"], 0),
                ParamDef::int("size", "Tooth size", 2, 60, 12),
            ]),
            None,
            true,
        );
        r
    }
}

/// Downscale with a box filter so the longest side is at most `max_side` (for preview
/// proxies). Returns the scale factor used (≤ 1).
pub fn downscale_proxy(img: &Image, max_side: u32) -> (Image, f32) {
    let longest = img.width.max(img.height);
    if longest <= max_side || img.is_empty() {
        return (img.clone(), 1.0);
    }
    let s = max_side as f32 / longest as f32;
    let (w, h) = (((img.width as f32 * s).round() as u32).max(1), ((img.height as f32 * s).round() as u32).max(1));
    let mut out = Image::new(w, h);
    for oy in 0..h {
        let y0 = (oy as u64 * img.height as u64 / h as u64) as u32;
        let y1 = (((oy + 1) as u64 * img.height as u64).div_ceil(h as u64) as u32).clamp(y0 + 1, img.height);
        for ox in 0..w {
            let x0 = (ox as u64 * img.width as u64 / w as u64) as u32;
            let x1 = (((ox + 1) as u64 * img.width as u64).div_ceil(w as u64) as u32).clamp(x0 + 1, img.width);
            let mut acc = [0u32; 4];
            for y in y0..y1 {
                let row = img.row(y);
                for p in row[x0 as usize * 4..x1 as usize * 4].chunks_exact(4) {
                    for c in 0..4 {
                        acc[c] += p[c] as u32;
                    }
                }
            }
            let n = (y1 - y0) * (x1 - x0);
            out.set_pixel(ox, oy, acc.map(|v| ((v + n / 2) / n) as u8));
        }
    }
    (out, w as f32 / img.width as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_replaces_by_id_and_keeps_order() {
        let mut r = EffectRegistry::with_placeholders();
        let n = r.len();
        let first = r.iter().next().map(|d| d.id);
        r.register(EffectDef::new("image.resize", "Resize", MenuSlot::Image, "size", |i, _, _| i.clone()));
        assert_eq!(r.len(), n);
        assert_eq!(r.iter().next().map(|d| d.id), first);
        assert!(!r.get("image.resize").expect("resize").placeholder);
        r.register(EffectDef::new("effects.new", "New", MenuSlot::Effects, "filter", |i, _, _| i.clone()));
        assert_eq!(r.len(), n + 1);
        // Grouping keeps first-appearance order and puts the new one with its group.
        let groups = r.menu(MenuSlot::Effects);
        let filter = groups.iter().find(|g| g[0].group == "filter").expect("filter group");
        assert_eq!(filter.last().map(|d| d.id), Some("effects.new"));
        assert!(r.menu(MenuSlot::Image).iter().flatten().all(|d| d.menu == MenuSlot::Image));
    }

    #[test]
    fn param_values_are_typed_and_safe() {
        let defs = vec![
            ParamDef::float("r", "R", 0.0, 10.0, 2.5),
            ParamDef::int("n", "N", 0, 9, 3),
            ParamDef::bool("b", "B", true),
            ParamDef::color("c", "C", [1, 2, 3, 4], true),
            ParamDef::choice("k", "K", &["a", "b"], 1),
            ParamDef::text("t", "T", "hello", false),
        ];
        let v = ParamValues::defaults(&defs);
        assert_eq!(v.f32("r"), 2.5);
        assert_eq!(v.i32("n"), 3);
        assert!(v.bool("b"));
        assert_eq!(v.color("c"), [1, 2, 3, 4]);
        assert_eq!(v.choice("k"), 1);
        assert_eq!(v.text("t"), "hello");
        assert_eq!(v.text("k"), "");
        // Missing or mistyped ids never panic.
        assert_eq!(v.f32("missing"), 0.0);
        assert!(!v.bool("r"));
        assert_eq!(v.color("n"), [0, 0, 0, 255]);
        assert_eq!(v.f32("n"), 3.0);
    }

    #[test]
    fn proxy_downscale() {
        let img = Image::filled(4000, 1000, [10, 20, 30, 255]);
        let (p, s) = downscale_proxy(&img, 800);
        assert_eq!((p.width, p.height), (800, 200));
        assert!((s - 0.2).abs() < 1e-6);
        assert_eq!(p.pixel(400, 100), [10, 20, 30, 255]);
        let small = Image::filled(3, 2, [1, 1, 1, 1]);
        let (p, s) = downscale_proxy(&small, 800);
        assert_eq!((p.width, p.height, s), (3, 2, 1.0));
        let tall = Image::filled(1, 5000, [9, 9, 9, 9]);
        let (p, _) = downscale_proxy(&tall, 100);
        assert_eq!((p.width, p.height), (1, 100));
    }
}
