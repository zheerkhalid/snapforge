//! Registers every schema-driven operation of [`crate::effects`] in the editor's effect
//! table: parameter schemas mapped from the effect parameter structs, i18n labels from the
//! `fx-*` keys, and pixel distances scaled by [`EffectCtx::scale`] so the downscaled
//! preview matches the final result.
//!
//! The interactive operations (make transparent, spotlight, strips, image watermark,
//! colour count, clone stamp) need the canvas and are registered elsewhere.

// Wired up by the editor's registration (`register_effects(fx_install::install)`).

use super::effects::{EffectCtx, EffectDef, EffectRegistry, MenuSlot, ParamDef, ParamKind, ParamValues};
#[cfg(test)]
use crate::common::Image;
use crate::effects::text::{FontSpec, TextAlign};
use crate::effects::{self as fx, Anchor, Color, Sides};
use crate::i18n::{t, tf};
use std::sync::Arc;

/// Ids reserved for the interactive canvas dialogs (not registered here).
#[cfg(test)]
pub(crate) const INTERACTIVE_IDS: [&str; 2] = ["image.transparency", "effects.spotlight"];

/// Menu / dialog label from a `fx-*` key, without the trailing ellipsis.
fn lbl(key: &str) -> String {
    t(key).trim_end_matches('…').trim_end_matches("...").trim().to_string()
}

fn color(v: &ParamValues, id: &str) -> Color {
    let c = v.color(id);
    Color::rgba(c[0], c[1], c[2], c[3])
}

/// A pixel distance scaled for the preview proxy (never negative).
fn px(v: &ParamValues, id: &str, ctx: &EffectCtx) -> u32 {
    (v.f32(id) * ctx.scale).round().max(0.0) as u32
}

/// Like [`px`] but at least 1.
fn px1(v: &ParamValues, id: &str, ctx: &EffectCtx) -> u32 {
    px(v, id, ctx).max(1)
}

fn spx(v: &ParamValues, id: &str, ctx: &EffectCtx) -> i32 {
    (v.f32(id) * ctx.scale).round() as i32
}

fn choice_of(id: &'static str, label: impl Into<String>, options: Vec<String>, default: usize) -> ParamDef {
    let mut d = ParamDef::choice(id, label, &[], 0);
    d.kind = ParamKind::Choice { options, default };
    d
}

fn choice_keys(id: &'static str, label_key: &str, keys: &[&str], default: usize) -> ParamDef {
    choice_of(id, lbl(label_key), keys.iter().map(|k| t(k)).collect(), default)
}

fn anchor_choice(id: &'static str, default: Anchor) -> ParamDef {
    let opts = Anchor::ALL.iter().map(|a| t(a.label_key())).collect();
    let def = Anchor::ALL.iter().position(|a| *a == default).unwrap_or(4);
    choice_of(id, lbl("fx-param-anchor"), opts, def)
}

fn anchor(v: &ParamValues, id: &str) -> Anchor {
    Anchor::ALL.get(v.choice(id)).copied().unwrap_or_default()
}

fn sides_params() -> Vec<ParamDef> {
    vec![
        ParamDef::bool("top", lbl("fx-side-top"), true),
        ParamDef::bool("right", lbl("fx-side-right"), true),
        ParamDef::bool("bottom", lbl("fx-side-bottom"), true),
        ParamDef::bool("left", lbl("fx-side-left"), true),
    ]
}

fn sides(v: &ParamValues) -> Sides {
    Sides { top: v.bool("top"), right: v.bool("right"), bottom: v.bool("bottom"), left: v.bool("left") }
}

const RESAMPLE: [fx::ResampleFilter; 4] = [
    fx::ResampleFilter::Lanczos3,
    fx::ResampleFilter::Mitchell,
    fx::ResampleFilter::Bilinear,
    fx::ResampleFilter::Nearest,
];

/// Installed font families (DirectWrite), "Segoe UI" first-choice default.
fn font_list() -> Arc<Vec<String>> {
    let mut f = crate::draw::dwrite::font_families();
    if f.is_empty() {
        f.push("Segoe UI".into());
    }
    Arc::new(f)
}

fn font_choice(id: &'static str, fonts: &[String]) -> ParamDef {
    let def = fonts.iter().position(|f| f.eq_ignore_ascii_case("Segoe UI")).unwrap_or(0);
    choice_of(id, lbl("fx-caption-font"), fonts.to_vec(), def)
}

fn font_name(fonts: &[String], v: &ParamValues, id: &str) -> String {
    fonts.get(v.choice(id)).cloned().unwrap_or_else(|| "Segoe UI".into())
}

/// Install the schema-driven effects (replacing placeholders with the same id).
pub(crate) fn install(r: &mut EffectRegistry) {
    use MenuSlot::{Effects as E, Image as I};
    let fonts = font_list();

    // ------------------------------------------------------------- Image ▸ size
    r.register(
        EffectDef::new("image.resize", lbl("fx-resize"), I, "size", |img, v, ctx| {
            let unit = if v.choice("unit") == 1 { fx::ResizeUnit::Pixels } else { fx::ResizeUnit::Percent };
            let p = fx::ResizeParams {
                unit,
                width: px(v, "width", ctx),
                height: px(v, "height", ctx),
                percent: v.f32("percent").max(0.1),
                keep_aspect: v.bool("keep_aspect"),
                filter: RESAMPLE.get(v.choice("filter")).copied().unwrap_or_default(),
            };
            fx::resize(img, &p)
        })
        .with_params(vec![
            choice_keys("unit", "fx-resize-unit", &["fx-resize-percent", "fx-resize-pixels"], 0),
            ParamDef::float("percent", lbl("fx-resize-percent"), 1.0, 800.0, 100.0).suffix("%"),
            ParamDef::int("width", lbl("fx-param-width"), 0, 20000, 0).suffix(" px").tooltip(t("fx-size-zero-tip")),
            ParamDef::int("height", lbl("fx-param-height"), 0, 20000, 0).suffix(" px").tooltip(t("fx-size-zero-tip")),
            ParamDef::bool("keep_aspect", lbl("fx-resize-keep-aspect"), true),
            choice_of("filter", lbl("fx-resize-filter"), RESAMPLE.iter().map(|f| t(f.label_key())).collect(), 0)
                .advanced(),
        ])
        .with_shortcut("Ctrl+R")
        .whole_image(),
    );
    for (slot, key) in [(1u8, "Ctrl+1"), (2, "Ctrl+2"), (3, "Ctrl+3"), (4, "Ctrl+4")] {
        let pct = fx::quick_resize_percent(slot).unwrap_or(100.0);
        let id = match slot {
            1 => "image.resize_25",
            2 => "image.resize_50",
            3 => "image.resize_75",
            _ => "image.resize_200",
        };
        r.register(
            EffectDef::new(
                id,
                tf("fx-resize-quick", &[("percent", &format!("{pct}"))]),
                I,
                "size",
                move |img, _, _| fx::quick_resize(img, slot),
            )
            .with_shortcut(key)
            .whole_image(),
        );
    }
    r.register(
        EffectDef::new("image.canvas_size", lbl("fx-canvas-size"), I, "size", |img, v, ctx| {
            let dim = |id: &str, cur: u32| if v.i32(id) <= 0 { cur } else { px1(v, id, ctx) };
            let p = fx::CanvasParams {
                width: dim("width", img.width),
                height: dim("height", img.height),
                anchor: anchor(v, "anchor"),
                fill: color(v, "fill"),
            };
            fx::canvas_size(img, &p)
        })
        .with_params(vec![
            ParamDef::int("width", lbl("fx-param-width"), 0, 20000, 0).suffix(" px").tooltip(t("fx-size-zero-tip")),
            ParamDef::int("height", lbl("fx-param-height"), 0, 20000, 0).suffix(" px").tooltip(t("fx-size-zero-tip")),
            anchor_choice("anchor", Anchor::Center),
            ParamDef::color("fill", lbl("fx-canvas-fill"), [255, 255, 255, 0], true),
        ])
        .whole_image(),
    );
    r.register(
        EffectDef::new("image.expand_canvas", lbl("fx-expand-canvas"), I, "size", |img, v, ctx| {
            let p = fx::ExpandParams {
                edges: fx::Edges {
                    left: px(v, "left", ctx),
                    top: px(v, "top", ctx),
                    right: px(v, "right", ctx),
                    bottom: px(v, "bottom", ctx),
                },
                fill: color(v, "fill"),
            };
            fx::expand_canvas(img, &p)
        })
        .with_params(vec![
            ParamDef::int("left", lbl("fx-expand-left"), 0, 4000, 20).suffix(" px"),
            ParamDef::int("top", lbl("fx-expand-top"), 0, 4000, 20).suffix(" px"),
            ParamDef::int("right", lbl("fx-expand-right"), 0, 4000, 20).suffix(" px"),
            ParamDef::int("bottom", lbl("fx-expand-bottom"), 0, 4000, 20).suffix(" px"),
            ParamDef::color("fill", lbl("fx-canvas-fill"), [255, 255, 255, 255], true),
        ])
        .whole_image(),
    );

    // ------------------------------------------------------------- Image ▸ rotate / flip
    r.register(
        EffectDef::new("image.rotate_left", lbl("fx-rotate-270"), I, "rotate", |img, _, _| {
            fx::rotate_quarter(img, fx::QuarterTurn::Cw270)
        })
        .with_shortcut("Ctrl+L")
        .whole_image(),
    );
    r.register(
        EffectDef::new("image.rotate_right", lbl("fx-rotate-90"), I, "rotate", |img, _, _| {
            fx::rotate_quarter(img, fx::QuarterTurn::Cw90)
        })
        .whole_image(),
    );
    r.register(
        EffectDef::new("image.rotate_180", lbl("fx-rotate-180"), I, "rotate", |img, _, _| {
            fx::rotate_quarter(img, fx::QuarterTurn::Cw180)
        })
        .whole_image(),
    );
    r.register(
        EffectDef::new("image.rotate", lbl("fx-rotate-any"), I, "rotate", |img, v, _| {
            fx::rotate(img, &fx::RotateParams { angle: v.f32("angle"), fill: color(v, "fill") })
        })
        .with_params(vec![
            ParamDef::float("angle", lbl("fx-rotate-angle"), -180.0, 180.0, 0.0).suffix("°"),
            ParamDef::color("fill", lbl("fx-canvas-fill"), [255, 255, 255, 0], true).advanced(),
        ])
        .whole_image(),
    );
    r.register(EffectDef::new("image.flip_h", lbl("fx-flip-horizontal"), I, "flip", |img, _, _| {
        fx::flip_horizontal(img)
    }));
    r.register(EffectDef::new("image.flip_v", lbl("fx-flip-vertical"), I, "flip", |img, _, _| fx::flip_vertical(img)));

    // ------------------------------------------------------------- Image ▸ color
    let presets: Vec<String> =
        fx::REDUCE_PRESETS.iter().map(|n| tf("fx-reduce-colors-n", &[("count", &n.to_string())])).collect();
    r.register(
        EffectDef::new("image.color_reduction", lbl("fx-reduce-colors"), I, "color", |img, v, _| {
            let colors = fx::REDUCE_PRESETS.get(v.choice("colors")).copied().unwrap_or(256);
            fx::reduce_colors(img, &fx::ReduceParams { colors, dither: v.bool("dither") })
        })
        .with_params(vec![
            choice_of("colors", lbl("fx-reduce-count"), presets, 0),
            ParamDef::bool("dither", lbl("fx-reduce-dither"), true),
        ]),
    );

    // ------------------------------------------------------------- Effects ▸ color
    r.register(
        EffectDef::new("effects.adjust", lbl("fx-adjust-colors"), E, "color", |img, v, _| {
            let p = fx::AdjustParams {
                brightness: v.i32("brightness"),
                contrast: v.i32("contrast"),
                gamma: v.f32("gamma").clamp(0.1, 5.0),
                saturation: v.i32("saturation"),
                hue: v.i32("hue"),
                red: v.i32("red"),
                green: v.i32("green"),
                blue: v.i32("blue"),
            };
            fx::adjust_colors(img, &p)
        })
        .with_params(vec![
            ParamDef::int("brightness", lbl("fx-adjust-brightness"), -100, 100, 0),
            ParamDef::int("contrast", lbl("fx-adjust-contrast"), -100, 100, 0),
            ParamDef::float("gamma", lbl("fx-adjust-gamma"), 0.1, 5.0, 1.0),
            ParamDef::int("saturation", lbl("fx-adjust-saturation"), -100, 100, 0),
            ParamDef::int("hue", lbl("fx-adjust-hue"), -180, 180, 0).suffix("°").advanced(),
            ParamDef::int("red", lbl("fx-adjust-red"), -100, 100, 0).advanced(),
            ParamDef::int("green", lbl("fx-adjust-green"), -100, 100, 0).advanced(),
            ParamDef::int("blue", lbl("fx-adjust-blue"), -100, 100, 0).advanced(),
        ])
        .with_shortcut("Ctrl+Shift+A"),
    );
    r.register(EffectDef::new("effects.auto_levels", lbl("fx-auto-levels"), E, "color", |img, _, _| {
        fx::auto_levels(img)
    }));
    r.register(EffectDef::new("effects.auto_contrast", lbl("fx-auto-contrast"), E, "color", |img, _, _| {
        fx::auto_contrast(img)
    }));
    r.register(EffectDef::new("effects.grayscale", lbl("fx-grayscale"), E, "color", |img, _, _| fx::grayscale(img)));
    r.register(EffectDef::new("effects.sepia", lbl("fx-sepia"), E, "color", |img, _, _| fx::sepia(img)));
    r.register(EffectDef::new("effects.negative", lbl("fx-negative"), E, "color", |img, _, _| fx::negative(img)));

    // ------------------------------------------------------------- Effects ▸ filter
    r.register(
        EffectDef::new("effects.sharpen", lbl("fx-sharpen"), E, "filter", |img, v, ctx| {
            let p = fx::SharpenParams {
                amount: v.f32("amount"),
                radius: (v.f32("radius") * ctx.scale).max(0.1),
                threshold: v.i32("threshold").clamp(0, 255) as u8,
            };
            fx::sharpen(img, &p)
        })
        .with_params(vec![
            ParamDef::float("amount", lbl("fx-sharpen-amount"), 0.0, 500.0, 100.0).suffix("%"),
            ParamDef::float("radius", lbl("fx-sharpen-radius"), 0.5, 20.0, 2.0).suffix(" px"),
            ParamDef::int("threshold", lbl("fx-sharpen-threshold"), 0, 255, 0).advanced(),
        ]),
    );
    r.register(
        EffectDef::new("effects.blur", lbl("fx-blur"), E, "filter", |img, v, ctx| {
            fx::blur(img, &fx::BlurParams { radius: (v.f32("radius") * ctx.scale).max(0.1) })
        })
        .with_params(vec![ParamDef::float("radius", lbl("fx-blur-radius"), 0.5, 100.0, 4.0).suffix(" px")]),
    );
    r.register(
        EffectDef::new("effects.pixelate", lbl("fx-pixelate"), E, "filter", |img, v, ctx| {
            fx::pixelate(img, &fx::PixelateParams { block: px1(v, "block", ctx) })
        })
        .with_params(vec![ParamDef::int("block", lbl("fx-pixelate-block"), 2, 100, 10).suffix(" px")]),
    );

    // ------------------------------------------------------------- Effects ▸ artistic
    r.register(
        EffectDef::new("effects.oil_paint", lbl("fx-oil-paint"), E, "artistic", |img, v, ctx| {
            fx::oil_paint(img, &fx::OilPaintParams { radius: px1(v, "radius", ctx).min(15) })
        })
        .with_params(vec![ParamDef::int("radius", lbl("fx-oil-radius"), 1, 15, 4)]),
    );
    r.register(
        EffectDef::new("effects.sketch", lbl("fx-sketch"), E, "artistic", |img, v, _| {
            fx::sketch(
                img,
                &fx::SketchParams { strength: v.i32("strength").clamp(0, 100) as u8, color: v.bool("color") },
            )
        })
        .with_params(vec![
            ParamDef::int("strength", lbl("fx-sketch-strength"), 0, 100, 60).suffix("%"),
            ParamDef::bool("color", lbl("fx-sketch-color"), false),
        ]),
    );

    // ------------------------------------------------------------- Effects ▸ decorate
    r.register(
        EffectDef::new("effects.shadow", lbl("fx-drop-shadow"), E, "decorate", |img, v, ctx| {
            fx::drop_shadow(img, &shadow_params(v, ctx))
        })
        .with_params(vec![
            ParamDef::int("offset_x", lbl("fx-shadow-offset-x"), -100, 100, 0).suffix(" px"),
            ParamDef::int("offset_y", lbl("fx-shadow-offset-y"), -100, 100, 10).suffix(" px"),
            ParamDef::float("blur", lbl("fx-shadow-blur"), 0.0, 100.0, 28.0).suffix(" px"),
            ParamDef::int("opacity", lbl("fx-shadow-opacity"), 0, 100, 45).suffix("%"),
            ParamDef::color("color", lbl("fx-shadow-color"), [0, 0, 0, 255], false).advanced(),
            ParamDef::int("corner_radius", lbl("fx-shadow-corner-radius"), 0, 200, 0).suffix(" px").advanced(),
            ParamDef::color("background", lbl("fx-param-background"), [255, 255, 255, 0], true).advanced(),
        ])
        .whole_image(),
    );
    r.register(
        EffectDef::new("effects.frame", lbl("fx-frame"), E, "decorate", |img, v, ctx| {
            let style = if v.choice("style") == 1 { fx::FrameStyle::Double } else { fx::FrameStyle::Solid };
            let p = fx::FrameParams {
                style,
                width: px1(v, "width", ctx),
                color: color(v, "color"),
                gap_color: color(v, "gap_color"),
                corner_radius: px(v, "corner_radius", ctx),
                background: color(v, "background"),
            };
            fx::frame(img, &p)
        })
        .with_params(vec![
            choice_keys("style", "fx-frame-style", &["fx-frame-solid", "fx-frame-double"], 0),
            ParamDef::int("width", lbl("fx-frame-width"), 1, 200, 8).suffix(" px"),
            ParamDef::color("color", lbl("fx-param-color"), [64, 64, 72, 255], true),
            ParamDef::color("gap_color", lbl("fx-frame-gap-color"), [255, 255, 255, 255], true).advanced(),
            ParamDef::int("corner_radius", lbl("fx-frame-corner-radius"), 0, 200, 0).suffix(" px").advanced(),
            ParamDef::color("background", lbl("fx-param-background"), [255, 255, 255, 0], true).advanced(),
        ])
        .whole_image(),
    );
    let mut torn = vec![
        ParamDef::bool("top", lbl("fx-side-top"), false),
        ParamDef::bool("right", lbl("fx-side-right"), false),
        ParamDef::bool("bottom", lbl("fx-side-bottom"), true),
        ParamDef::bool("left", lbl("fx-side-left"), false),
    ];
    torn.extend([
        ParamDef::int("depth", lbl("fx-torn-depth"), 2, 100, 14).suffix(" px"),
        ParamDef::int("tooth", lbl("fx-torn-tooth"), 4, 100, 16).suffix(" px"),
        ParamDef::bool("shadow", lbl("fx-torn-shadow"), true),
        ParamDef::bool("rim", lbl("fx-torn-rim"), true).advanced(),
        ParamDef::int("seed", lbl("fx-param-seed"), 0, 9999, 1).advanced(),
        ParamDef::color("background", lbl("fx-param-background"), [255, 255, 255, 0], true).advanced(),
    ]);
    r.register(
        EffectDef::new("effects.torn_edge", lbl("fx-torn-edge"), E, "decorate", |img, v, ctx| {
            let base = fx::TornEdgeParams::default();
            let sh = &base.shadow;
            let shadow = fx::ShadowParams {
                offset_x: (sh.offset_x as f32 * ctx.scale).round() as i32,
                offset_y: (sh.offset_y as f32 * ctx.scale).round() as i32,
                blur: sh.blur * ctx.scale,
                ..sh.clone()
            };
            let p = fx::TornEdgeParams {
                sides: sides(v),
                depth: px1(v, "depth", ctx),
                tooth: px1(v, "tooth", ctx),
                seed: v.i32("seed").max(0) as u64,
                paper_rim: v.bool("rim"),
                shadow_enabled: v.bool("shadow"),
                shadow,
                background: color(v, "background"),
            };
            fx::torn_edge(img, &p)
        })
        .with_params(torn)
        .whole_image(),
    );
    r.register(
        EffectDef::new("effects.round_corners", lbl("fx-round-corners"), E, "decorate", |img, v, ctx| {
            fx::round_corners(img, px1(v, "radius", ctx))
        })
        .with_params(vec![ParamDef::int("radius", lbl("fx-param-radius"), 1, 200, 16).suffix(" px")]),
    );
    let mut fade = sides_params();
    fade.extend([
        ParamDef::int("size", lbl("fx-fade-size"), 1, 400, 40).suffix(" px"),
        ParamDef::color("color", lbl("fx-fade-to"), [255, 255, 255, 0], true),
    ]);
    r.register(
        EffectDef::new("effects.fade_edge", lbl("fx-fade-edge"), E, "decorate", |img, v, ctx| {
            fx::fade_edge(
                img,
                &fx::FadeEdgeParams { sides: sides(v), size: px1(v, "size", ctx), color: color(v, "color") },
            )
        })
        .with_params(fade),
    );
    r.register(
        EffectDef::new("effects.reflection", lbl("fx-reflection"), E, "decorate", |img, v, ctx| {
            let p = fx::ReflectionParams {
                height: v.i32("height").clamp(1, 100) as u8,
                gap: px(v, "gap", ctx),
                opacity: v.i32("opacity").clamp(0, 100) as u8,
                background: color(v, "background"),
            };
            fx::reflection(img, &p)
        })
        .with_params(vec![
            ParamDef::int("height", lbl("fx-reflection-height"), 5, 100, 35).suffix("%"),
            ParamDef::int("gap", lbl("fx-reflection-gap"), 0, 100, 2).suffix(" px"),
            ParamDef::int("opacity", lbl("fx-reflection-opacity"), 0, 100, 40).suffix("%"),
            ParamDef::color("background", lbl("fx-param-background"), [255, 255, 255, 0], true).advanced(),
        ])
        .whole_image(),
    );

    // ------------------------------------------------------------- Effects ▸ text
    let f = fonts.clone();
    r.register(
        EffectDef::new("effects.watermark_text", lbl("fx-watermark-text"), E, "text", move |img, v, ctx| {
            let p = fx::TextWatermarkParams {
                text: v.text("text"),
                font: FontSpec {
                    face: font_name(&f, v, "font"),
                    size: px1(v, "size", ctx),
                    bold: v.bool("bold"),
                    italic: false,
                    underline: false,
                },
                color: color(v, "color"),
                outline: v.bool("outline"),
                placement: fx::WatermarkParams {
                    anchor: anchor(v, "anchor"),
                    margin: px(v, "margin", ctx),
                    scale: 100.0,
                    opacity: v.i32("opacity").clamp(0, 100) as u8,
                    tile: v.bool("tile"),
                    spacing: px(v, "spacing", ctx),
                },
            };
            fx::watermark_text(img, &p)
        })
        .with_params(vec![
            ParamDef::text("text", lbl("fx-watermark-text-label"), "© SnapForge", false),
            font_choice("font", &fonts),
            ParamDef::int("size", lbl("fx-caption-font-size"), 6, 300, 28).suffix(" px"),
            ParamDef::bool("bold", lbl("fx-caption-bold"), true),
            ParamDef::color("color", lbl("fx-param-color"), [255, 255, 255, 255], true),
            ParamDef::bool("outline", lbl("fx-watermark-outline"), true),
            anchor_choice("anchor", Anchor::BottomRight),
            ParamDef::int("opacity", lbl("fx-param-opacity"), 0, 100, 70).suffix("%"),
            ParamDef::int("margin", lbl("fx-watermark-margin"), 0, 500, 16).suffix(" px").advanced(),
            ParamDef::bool("tile", lbl("fx-watermark-tile"), false).advanced(),
            ParamDef::int("spacing", lbl("fx-watermark-spacing"), 0, 500, 60).suffix(" px").advanced(),
        ]),
    );
    let f = fonts.clone();
    r.register(
        EffectDef::new("effects.caption", lbl("fx-caption"), E, "text", move |img, v, ctx| {
            let position =
                if v.choice("position") == 0 { fx::CaptionPosition::Top } else { fx::CaptionPosition::Bottom };
            let align = match v.choice("align") {
                0 => TextAlign::Left,
                2 => TextAlign::Right,
                _ => TextAlign::Center,
            };
            let border = match v.choice("border") {
                0 => fx::CaptionBorder::None,
                2 => fx::CaptionBorder::Frame,
                _ => fx::CaptionBorder::Line,
            };
            let p = fx::CaptionParams {
                text: v.text("text"),
                position,
                font: FontSpec {
                    face: font_name(&f, v, "font"),
                    size: px1(v, "font_size", ctx),
                    bold: v.bool("bold"),
                    italic: v.bool("italic"),
                    underline: v.bool("underline"),
                },
                text_color: color(v, "text_color"),
                background: color(v, "background"),
                align,
                padding: px(v, "padding", ctx),
                border,
                border_color: color(v, "border_color"),
                border_width: if v.i32("border_width") > 0 { px1(v, "border_width", ctx) } else { 0 },
            };
            fx::caption(img, &p)
        })
        .with_params(vec![
            ParamDef::text("text", lbl("fx-caption-text"), "Caption", true),
            choice_keys("position", "fx-caption-position", &["fx-caption-top", "fx-caption-bottom"], 1),
            font_choice("font", &fonts),
            ParamDef::int("font_size", lbl("fx-caption-font-size"), 6, 200, 18).suffix(" px"),
            ParamDef::color("text_color", lbl("fx-caption-text-color"), [32, 33, 36, 255], true),
            ParamDef::color("background", lbl("fx-caption-bar-color"), [246, 247, 249, 255], true),
            choice_keys(
                "align",
                "fx-caption-align",
                &["fx-caption-align-left", "fx-caption-align-center", "fx-caption-align-right"],
                1,
            ),
            ParamDef::bool("bold", lbl("fx-caption-bold"), false).advanced(),
            ParamDef::bool("italic", lbl("fx-caption-italic"), false).advanced(),
            ParamDef::bool("underline", lbl("fx-caption-underline"), false).advanced(),
            ParamDef::int("padding", lbl("fx-caption-padding"), 0, 200, 12).suffix(" px").advanced(),
            choice_keys(
                "border",
                "fx-caption-border",
                &["fx-caption-border-none", "fx-caption-border-line", "fx-caption-border-frame"],
                1,
            )
            .advanced(),
            ParamDef::color("border_color", lbl("fx-caption-border-color"), [200, 203, 208, 255], true).advanced(),
            ParamDef::int("border_width", lbl("fx-caption-border-width"), 0, 20, 1).suffix(" px").advanced(),
        ])
        .whole_image(),
    );
}

fn shadow_params(v: &ParamValues, ctx: &EffectCtx) -> fx::ShadowParams {
    fx::ShadowParams {
        offset_x: spx(v, "offset_x", ctx),
        offset_y: spx(v, "offset_y", ctx),
        blur: (v.f32("blur") * ctx.scale).max(0.0),
        opacity: v.i32("opacity").clamp(0, 100) as u8,
        color: color(v, "color"),
        corner_radius: px(v, "corner_radius", ctx),
        background: color(v, "background"),
    }
}

/// Apply an installed effect with its defaults (tests and tooling).
#[cfg(test)]
fn apply_defaults(d: &EffectDef, img: &Image, ctx: &EffectCtx) -> Image {
    let v = ParamValues::defaults(d.params.as_deref().unwrap_or(&[]));
    (d.apply)(img, &v, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::effects::Scope;

    fn test_image() -> Image {
        let mut img = Image::new(9, 7);
        for y in 0..7 {
            for x in 0..9 {
                let a = if (x + y) % 5 == 0 { 128 } else { 255 };
                let s = |c: u32| ((c * a + 127) / 255) as u8;
                img.set_pixel(x, y, [s(x * 25), s(y * 30), s(200), a as u8]);
            }
        }
        img
    }

    fn installed() -> (EffectRegistry, Vec<&'static str>) {
        let mut r = EffectRegistry::with_placeholders();
        install(&mut r);
        let ids = r.iter().filter(|d| !INTERACTIVE_IDS.contains(&d.id)).map(|d| d.id).collect();
        (r, ids)
    }

    #[test]
    fn every_installed_effect_applies_on_a_small_image() {
        let (r, ids) = installed();
        assert_eq!(ids.len(), 33, "installed effects");
        let img = test_image();
        for id in ids {
            let d = r.get(id).expect("registered");
            assert!(!d.placeholder, "{id} is still a placeholder");
            for ctx in [
                EffectCtx { scale: 1.0, preview: false, background: [255, 255, 255, 255] },
                EffectCtx { scale: 0.5, preview: true, background: [0, 0, 0, 0] },
            ] {
                let out = apply_defaults(d, &img, &ctx);
                assert!(!out.is_empty(), "{id} returned an empty image");
                if d.scope == Scope::SelectionOrImage {
                    assert_eq!((out.width, out.height), (img.width, img.height), "{id} changed the size");
                }
            }
            // A 1×1 input must not panic either.
            let tiny = Image::filled(1, 1, [10, 20, 30, 255]);
            let _ = apply_defaults(d, &tiny, &EffectCtx { scale: 1.0, preview: false, background: [0; 4] });
        }
    }

    #[test]
    fn quick_resize_shortcuts_and_results() {
        let (r, _) = installed();
        for (id, key, w) in [
            ("image.resize_25", "Ctrl+1", 4u32),
            ("image.resize_50", "Ctrl+2", 8),
            ("image.resize_75", "Ctrl+3", 12),
            ("image.resize_200", "Ctrl+4", 32),
        ] {
            let d = r.get(id).expect(id);
            assert_eq!(d.shortcut, Some(key));
            assert_eq!(d.scope, Scope::WholeImage);
            let img = Image::filled(16, 8, [1, 2, 3, 255]);
            let out = apply_defaults(d, &img, &EffectCtx { scale: 1.0, preview: false, background: [0; 4] });
            assert_eq!(out.width, w, "{id}");
        }
        assert_eq!(r.get("image.resize").and_then(|d| d.shortcut), Some("Ctrl+R"));
        assert_eq!(r.get("image.rotate_left").and_then(|d| d.shortcut), Some("Ctrl+L"));
    }

    #[test]
    fn param_mapping_follows_the_schema() {
        let (r, _) = installed();
        let d = r.get("image.canvas_size").expect("canvas");
        let mut v = ParamValues::defaults(d.params.as_deref().unwrap_or(&[]));
        let img = Image::filled(10, 6, [9, 9, 9, 255]);
        let ctx = EffectCtx { scale: 1.0, preview: false, background: [0; 4] };
        // 0 keeps the current size.
        let out = (d.apply)(&img, &v, &ctx);
        assert_eq!((out.width, out.height), (10, 6));
        v.set("width", super::super::effects::ParamValue::Int(20));
        let out = (d.apply)(&img, &v, &ctx);
        assert_eq!((out.width, out.height), (20, 6));
        // Preview scale shrinks pixel distances.
        let out = (d.apply)(&img, &v, &EffectCtx { scale: 0.5, ..ctx });
        assert_eq!(out.width, 10);
        // Flip on its own is pixel exact.
        let f = r.get("image.flip_h").expect("flip");
        let mut src = Image::new(2, 1);
        src.set_pixel(0, 0, [1, 2, 3, 255]);
        let out = apply_defaults(f, &src, &ctx);
        assert_eq!(out.pixel(1, 0), [1, 2, 3, 255]);
        // Labels are translated (no raw keys, no trailing ellipsis).
        for d in r.iter().filter(|d| !d.placeholder) {
            assert!(!d.label.starts_with("fx-"), "{} label {}", d.id, d.label);
            assert!(!d.label.ends_with('…'), "{}", d.id);
        }
    }
}
