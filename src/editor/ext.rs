//! Extension points for later phases: canvas tools and annotation layers (Phase 3
//! `draw`), and the Tools menu host (Phase 2/5 tool windows).
//!
//! Registration is global and may happen from any thread before or after the editor
//! starts (see [`super::register_effects`], [`super::register_canvas_tools`],
//! [`super::set_tools_host`]).

use super::document::Document;
use super::selection::CanvasMap;
use crate::app::{post, AppCommand, ToolWindow};
use crate::common::Image;
use eframe::egui;
use std::any::Any;
use std::path::PathBuf;
use std::sync::Arc;

/// Renders an annotation layer onto a base image (runs on worker threads).
pub type Flattener = Arc<dyn Fn(&Image) -> Image + Send + Sync>;

/// An annotation layer attached to a document ([`Document::overlay`]). The editor paints
/// it above the pixels, flattens it for save / copy, and forwards undo / redo of the
/// [`super::history::StepKind::Overlay`] markers the layer records with
/// [`Document::record_overlay_step`].
pub trait DocOverlay: Send {
    /// Paint the objects. `map` converts image coordinates to screen points; `base` is the
    /// document's current pixels (magnifiers, highlighters and raster objects read them).
    fn paint(&mut self, ctx: &egui::Context, painter: &egui::Painter, map: &CanvasMap, base: &Arc<Image>);
    /// The image with the objects rendered on top.
    fn flatten(&self, base: &Image) -> Image;
    /// A detached flattener for worker threads (save, copy, print), `None` when empty.
    fn flattener(&self) -> Option<Flattener>;
    fn undo(&mut self);
    fn redo(&mut self);
    fn is_empty(&self) -> bool;
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// Pointer state handed to a [`CanvasTool`], in image coordinates.
#[derive(Debug, Clone, Copy)]
pub struct ToolInput {
    pub map: CanvasMap,
    /// Pointer position in image coordinates (continuous), if over the canvas.
    pub pos: Option<[f64; 2]>,
    /// Primary button went down / is held / was released this frame.
    pub pressed: bool,
    pub down: bool,
    pub released: bool,
    pub double_clicked: bool,
    pub modifiers: egui::Modifiers,
    /// Vertical wheel movement in points this frame (tools that [`CanvasTool::wants_wheel`]).
    pub scroll: f32,
    /// The pointer is over the canvas.
    pub hovered: bool,
}

/// A tool that works on the canvas (Phase 3 draw tools register these). The built-in
/// selection tools are handled by the editor itself.
pub trait CanvasTool: Send {
    /// Stable id (`"draw.arrow"`), also the command-palette id suffix.
    fn id(&self) -> &str;
    fn label(&self) -> String;
    /// A single glyph shown on the toolbar.
    fn icon(&self) -> &'static str;
    /// Shortcut text, e.g. `"A"` or `"Ctrl+Shift+A"` (parsed like menu shortcuts).
    fn shortcut(&self) -> Option<&str> {
        None
    }
    fn cursor(&self) -> egui::CursorIcon {
        egui::CursorIcon::Crosshair
    }
    /// Pointer input over the canvas while this tool is active.
    fn on_input(&mut self, doc: &mut Document, input: &ToolInput);
    /// Paint tool feedback (rubber bands, handles) above the image.
    fn paint(&mut self, _doc: &Document, _painter: &egui::Painter, _map: &CanvasMap) {}
    /// Optional options strip shown in the ribbon while the tool is active.
    fn options_ui(&mut self, _ui: &mut egui::Ui) {}
    /// The tool was switched away from (finish pending edits).
    fn deactivate(&mut self, _doc: &mut Document) {}
    /// The plain mouse wheel goes to the tool (e.g. brush size) instead of scrolling.
    fn wants_wheel(&self) -> bool {
        false
    }
    /// Listed in the ribbon's tool group (draw tools have their own palette instead).
    fn on_ribbon(&self) -> bool {
        true
    }
}

/// What a Tools-menu command gets: the open documents of the current workspace.
pub struct ToolsContext {
    /// Files of the open tabs that are saved on disk.
    pub files: Vec<PathBuf>,
    /// All open images (title, pixels), active one first.
    pub images: Vec<(String, Arc<Image>)>,
}

/// Implemented by the integration pass for Tools ▸ Combine Images / Convert Images to
/// PDF / Batch Print. The default posts [`AppCommand::OpenTool`].
pub trait ToolsHost: Send + Sync {
    fn combine_images(&self, ctx: ToolsContext);
    fn images_to_pdf(&self, ctx: ToolsContext);
    fn batch_print(&self, ctx: ToolsContext);
}

pub struct DefaultToolsHost;

impl ToolsHost for DefaultToolsHost {
    fn combine_images(&self, _ctx: ToolsContext) {
        post(AppCommand::OpenTool(ToolWindow::CombineImages));
    }
    fn images_to_pdf(&self, _ctx: ToolsContext) {
        post(AppCommand::OpenTool(ToolWindow::ImagesToPdf));
    }
    fn batch_print(&self, _ctx: ToolsContext) {
        post(AppCommand::OpenTool(ToolWindow::BatchPrint));
    }
}
