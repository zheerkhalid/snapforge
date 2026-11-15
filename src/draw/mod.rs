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

// Public API for the editor UI (built in a later phase on top of this module).
#[allow(unused_imports)]
pub use edit::{
    align, apply_style, bounds, bring_forward, bring_to_front, constrain_to_axis, constrain_to_hv, delete, drag_handle,
    duplicate, frame, handle_at, handles, hit_test, move_objects, next_id, next_step_value, next_step_value_as, paste,
    selection_bounds, send_backward, send_to_back, serialize_selection, set_frame, translate, Align, Dir,
    DragConstraints, Handle, HandleId, HandleKind, StylePatch,
};
#[allow(unused_imports)]
pub use history::{Document, Edit, History};
pub use model::*;
#[allow(unused_imports)]
pub use raster::RasterCache;
#[allow(unused_imports)]
pub use render::{render, render_object, render_overlay, render_vectors_onto, RenderOptions, View};
#[allow(unused_imports)]
pub use sfcap::{load_sfcap, save_sfcap, Meta};
#[allow(unused_imports)]
pub use tools::{create, create_picture, default_style, Tool, ToolOptions};
