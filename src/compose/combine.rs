//! Combine several images side by side or stacked, with optional shadows and file-name labels.

use crate::common::Image;
use crate::effects::text::{render_text_styled, FontSpec, TextAlign};
use crate::effects::{composite, drop_shadow, flatten_onto, Color, Orientation, ShadowParams};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CombineAlign {
    /// Top (horizontal layout) or left (vertical layout).
    Start,
    #[default]
    Center,
    /// Bottom or right.
    End,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CombineParams {
    /// `Horizontal` = side by side, `Vertical` = stacked.
    pub direction: Orientation,
    /// Space between images in pixels.
    pub spacing: u32,
    /// Space around everything in pixels.
    pub margin: u32,
    pub align: CombineAlign,
    /// Canvas colour; transparent allowed.
    pub background: Color,
    pub shadow: bool,
    pub shadow_params: ShadowParams,
    /// File name (or given label) under each image.
    pub labels: bool,
    pub label_font: FontSpec,
    pub label_color: Color,
}

impl Default for CombineParams {
    fn default() -> Self {
        Self {
            direction: Orientation::Horizontal,
            spacing: 16,
            margin: 16,
            align: CombineAlign::Center,
            background: Color::WHITE,
            shadow: false,
            shadow_params: ShadowParams { offset_y: 4, blur: 14.0, opacity: 35, ..Default::default() },
            labels: false,
            label_font: FontSpec { size: 14, ..Default::default() },
            label_color: Color::rgb(60, 64, 67),
        }
    }
}

/// Gap between an image and its label.
pub const LABEL_GAP: u32 = 6;

/// One image to combine and its label (used when `labels` is on).
#[derive(Debug, Clone)]
pub struct CombineItem {
    pub image: Image,
    pub label: String,
}

/// Where each block goes: image top-left and label top-left (canvas coordinates).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CombineCell {
    pub image_x: i32,
    pub image_y: i32,
    pub label_x: i32,
    pub label_y: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CombineLayout {
    pub width: u32,
    pub height: u32,
    pub cells: Vec<CombineCell>,
}

/// Pure layout: `blocks` are the (already shadowed) image sizes, `labels` the label sizes
/// ((0, 0) for none).
pub fn combine_layout(blocks: &[(u32, u32)], labels: &[(u32, u32)], p: &CombineParams) -> CombineLayout {
    let cell = |i: usize| -> (u32, u32) {
        let (iw, ih) = blocks[i];
        let (lw, lh) = labels.get(i).copied().unwrap_or((0, 0));
        let lab_h = if lh > 0 { LABEL_GAP + lh } else { 0 };
        (iw.max(lw), ih + lab_h)
    };
    let n = blocks.len();
    let cells: Vec<(u32, u32)> = (0..n).map(cell).collect();
    let gaps = p.spacing * n.saturating_sub(1) as u32;
    let horizontal = p.direction == Orientation::Horizontal;
    let (main, cross) = if horizontal {
        (cells.iter().map(|c| c.0).sum::<u32>() + gaps, cells.iter().map(|c| c.1).max().unwrap_or(0))
    } else {
        (cells.iter().map(|c| c.1).sum::<u32>() + gaps, cells.iter().map(|c| c.0).max().unwrap_or(0))
    };
    let (width, height) = if horizontal { (main, cross) } else { (cross, main) };
    let m = p.margin;
    let mut out = Vec::with_capacity(n);
    let mut pos = 0u32;
    for (i, &(cw, ch)) in cells.iter().enumerate() {
        let (iw, ih) = blocks[i];
        let (lw, _) = labels.get(i).copied().unwrap_or((0, 0));
        let free = if horizontal { cross - ch } else { cross - cw };
        let shift = match p.align {
            CombineAlign::Start => 0,
            CombineAlign::Center => free / 2,
            CombineAlign::End => free,
        };
        let (cx, cy) = if horizontal { (pos, shift) } else { (shift, pos) };
        out.push(CombineCell {
            image_x: (m + cx + (cw - iw) / 2) as i32,
            image_y: (m + cy) as i32,
            label_x: (m + cx + (cw - lw) / 2) as i32,
            label_y: (m + cy + ih + LABEL_GAP) as i32,
        });
        pos += if horizontal { cw } else { ch } + p.spacing;
    }
    CombineLayout { width: width + 2 * m, height: height + 2 * m, cells: out }
}

/// Combine images into one.
pub fn combine_images(items: &[CombineItem], p: &CombineParams) -> Image {
    if items.is_empty() {
        return Image::new(0, 0);
    }
    let blocks: Vec<Image> = items
        .iter()
        .map(|it| {
            if p.shadow {
                drop_shadow(&it.image, &ShadowParams { background: Color::TRANSPARENT, ..p.shadow_params.clone() })
            } else {
                it.image.clone()
            }
        })
        .collect();
    let labels: Vec<Image> = items
        .iter()
        .zip(&blocks)
        .map(|(it, b)| {
            if p.labels && !it.label.is_empty() {
                // Wrap long names to the image width (at least 120 px).
                render_text_styled(&it.label, &p.label_font, p.label_color, b.width.max(120), TextAlign::Center)
            } else {
                Image::new(0, 0)
            }
        })
        .collect();
    let bs: Vec<(u32, u32)> = blocks.iter().map(|b| (b.width, b.height)).collect();
    let ls: Vec<(u32, u32)> = labels.iter().map(|l| (l.width, l.height)).collect();
    let lay = combine_layout(&bs, &ls, p);
    let mut out = Image::new(lay.width.min(crate::effects::MAX_DIM), lay.height.min(crate::effects::MAX_DIM));
    for ((b, l), c) in blocks.iter().zip(&labels).zip(&lay.cells) {
        composite(&mut out, b, c.image_x, c.image_y, 255);
        if !l.is_empty() {
            composite(&mut out, l, c.label_x, c.label_y, 255);
        }
    }
    flatten_onto(&out, p.background)
}

pub(crate) fn file_label(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}
