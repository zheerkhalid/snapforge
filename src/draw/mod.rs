//! Annotation core (Phase 3): object model, rendering, editing, undo and `.sfcap` projects.
//!
//! - [`model`]: [`Object`] (id, [`Kind`], [`Style`], optional [`Timing`]); z-order = list order.
//! - Rendering: [`render`] (flatten for export), [`render_overlay`] (transparent live-view layer
//!   at a zoom/viewport), [`RasterCache`] (base image with raster objects applied).
//! - Editing ([`edit`]): hit-testing, handles and drags, move, align, z-order, group style
//!   edits, copy/paste. Undo: [`History`] over a [`Document`].
//! - Tools and presets ([`tools`]): creating objects from drag gestures, per-tool options.
//! - Projects ([`sfcap`]): zip with `original.png`, `annotations.json`, `metadata.json`.
//!
//! Text is laid out and outlined with DirectWrite ([`dwrite`]); see DECISIONS.md.

pub mod b64;
pub mod dwrite;
pub mod edit;
pub mod fx;
pub mod history;
pub mod model;
pub mod path;
pub mod raster;
pub mod render;
pub mod sfcap;
pub mod tools;

#[cfg(test)]
mod edit_tests;
#[cfg(test)]
mod golden_tests;
#[cfg(test)]
mod tests;

// The parts of the API the rest of the app reaches as `draw::…`.
pub use edit::{
    bounds, drag_handle, handle_at, handles, hit_test, next_id, translate, Dir, DragConstraints, HandleId, HandleKind,
};
pub use history::{Document, History};
pub use model::*;
pub use raster::RasterCache;
pub use render::{render, render_object, render_overlay, RenderOptions, View};
pub use sfcap::{load_sfcap, save_sfcap, Meta};
pub use tools::{create, create_picture, default_style, Tool, ToolOptions};
