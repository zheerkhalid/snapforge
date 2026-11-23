//! Menu bar, ribbon toolbar, workspace strip, tab strip and status bar. Every item is a
//! [`Command`] from the registry, so labels, shortcuts, tooltips and enabled state are
//! the same in menus, on the ribbon and in the command palette.

use super::app::{file_name, EditorApp};
use super::commands::{self as c, Action, Category, CmdState, Command, CommandRegistry, ToolId, ZoomCmd};
use super::document::DocId;
use super::effects::{EffectRegistry, MenuSlot};
use super::ext::CanvasTool;
use super::fileops::format_bytes;
use super::style::{self, Palette};
use super::view;
use crate::i18n::{t, tf};
use crate::settings;
use eframe::egui::{self, vec2, Align, Color32, CornerRadius, Id, Layout, Margin, RichText, Sense, Stroke};
use std::path::Path;

/// Every command the editor knows, in menu order.
pub fn build_registry(
    effects: &EffectRegistry,
    tools: &[Box<dyn CanvasTool>],
    recent: &[String],
    editors: &[String],
) -> CommandRegistry {
    use Category as K;
    let mut r = CommandRegistry::default();
    let mut add = |cmd: Command| r.register(cmd);

    // File
    add(Command::new("file.open", t("editor-open"), K::File, Action::Open)
        .key("Ctrl+O")
        .icon("📂")
        .keywords("load file"));
    add(Command::new("file.save", t("editor-save"), K::File, Action::Save).key("Ctrl+S").icon("💾").when(c::has_doc));
    add(Command::new("file.save_as", t("editor-save-as"), K::File, Action::SaveAs)
        .key("Alt+S")
        .icon("🖫")
        .when(c::has_doc));
    add(Command::new("file.save_all", t("editor-save-all"), K::File, Action::SaveAll)
        .key("Ctrl+Shift+S")
        .icon("💾")
        .when(c::any_dirty));
    add(Command::new("file.save_sequence", t("editor-save-sequence"), K::File, Action::SaveAllSequence)
        .when(c::has_doc)
        .keywords("numbered batch base name"));
    add(Command::new("file.save_project", t("draw-save-project"), K::File, Action::SaveProject)
        .key("Ctrl+Alt+S")
        .icon("🗐")
        .when(c::has_doc)
        .keywords("sfcap annotations editable project"));
    add(Command::new("file.print", t("editor-print"), K::File, Action::Print).key("Ctrl+P").icon("🖶").when(c::has_doc));
    add(Command::new("file.scan", t("editor-scan"), K::File, Action::Scan)
        .icon("📠")
        .keywords("scanner wia acquire twain"));
    for (i, name) in editors.iter().enumerate() {
        add(Command::new(
            format!("file.edit_with.{i}"),
            tf("editor-edit-with-named", &[("name", name)]),
            K::File,
            Action::EditWith(i),
        )
        .when(c::has_doc)
        .icon("✏")
        .keywords("external program open with"));
    }
    add(Command::new("file.configure_editors", t("editor-configure-editors"), K::File, Action::ConfigureEditors)
        .keywords("external edit with programs"));
    add(Command::new("file.properties", t("editor-properties"), K::File, Action::Properties)
        .key("I")
        .icon("ℹ")
        .when(c::has_doc));
    add(Command::new("file.open_folder", t("editor-open-folder"), K::File, Action::OpenFolder).key("W").icon("🗁"));
    add(Command::new("file.delete", t("editor-delete-file"), K::File, Action::DeleteFile)
        .key("Ctrl+Del")
        .icon("🗑")
        .when(c::has_path));
    add(Command::new("file.continue", t("editor-continue-cmd"), K::File, Action::Continue).when(c::can_continue));
    add(Command::new("file.close", t("editor-close-tab"), K::File, Action::CloseTab).key("Ctrl+W"));
    add(Command::new("file.close_saved", t("editor-close-saved"), K::File, Action::CloseSaved)
        .key("Ctrl+Q")
        .when(c::has_doc));
    add(Command::new("file.clear_recent", t("editor-recent-clear"), K::File, Action::ClearRecent).hidden());
    for (i, p) in recent.iter().enumerate() {
        add(Command::new(
            format!("recent.{i}"),
            file_name(Path::new(p)),
            K::Recent,
            Action::OpenRecent(vec![p.clone()]),
        )
        .icon("🕘")
        .keywords(p));
    }

    // Edit
    add(Command::new("edit.undo", t("editor-undo"), K::Edit, Action::Undo).key("Ctrl+Z").icon("⟲").when(c::can_undo));
    add(Command::new("edit.redo", t("editor-redo"), K::Edit, Action::Redo)
        .key("Ctrl+Y")
        .alt_key("Ctrl+Shift+Z")
        .icon("⟳")
        .when(c::can_redo));
    add(Command::new("edit.cut", t("editor-cut"), K::Edit, Action::Cut).key("Ctrl+X").icon("✂").when(c::has_selection));
    add(Command::new("edit.copy", t("editor-copy"), K::Edit, Action::Copy).key("Ctrl+C").icon("📋").when(c::has_doc));
    add(Command::new("edit.copy_image", t("editor-copy-image"), K::Edit, Action::CopyImage)
        .icon("📋")
        .keywords("clipboard whole image")
        .when(c::has_doc));
    add(Command::new("edit.paste", t("editor-paste"), K::Edit, Action::Paste).key("Ctrl+V").icon("📥"));
    add(Command::new("edit.paste_tab", t("editor-paste-new-tab"), K::Edit, Action::PasteNewTab)
        .key("Ctrl+Shift+V")
        .icon("📥"));
    add(Command::new("edit.select_all", t("editor-select-all"), K::Select, Action::SelectAll)
        .key("Ctrl+A")
        .when(c::editable));
    add(Command::new("edit.deselect", t("editor-deselect"), K::Select, Action::Deselect)
        .key("Ctrl+D")
        .when(c::has_selection));
    add(Command::new("edit.delete", t("editor-delete-selection"), K::Select, Action::DeleteSelection)
        .key("Del")
        .when(c::can_delete));
    add(Command::new("draw.duplicate", t("draw-duplicate"), K::Edit, Action::DuplicateObjects)
        .key("Ctrl+J")
        .icon("⧉")
        .when(c::has_objects));
    for (id, key, dx, dy) in [
        ("draw.nudge_left", "ArrowLeft", -1, 0),
        ("draw.nudge_right", "ArrowRight", 1, 0),
        ("draw.nudge_up", "ArrowUp", 0, -1),
        ("draw.nudge_down", "ArrowDown", 0, 1),
        ("draw.nudge_left10", "Shift+ArrowLeft", -10, 0),
        ("draw.nudge_right10", "Shift+ArrowRight", 10, 0),
        ("draw.nudge_up10", "Shift+ArrowUp", 0, -10),
        ("draw.nudge_down10", "Shift+ArrowDown", 0, 10),
    ] {
        add(Command::new(id, t("draw-nudge"), K::Edit, Action::Nudge(dx, dy)).key(key).when(c::has_objects).hidden());
    }
    add(Command::new("draw.next_tool", t("draw-next-tool"), K::Tools, Action::CycleTool(1))
        .key("Tab")
        .when(c::has_doc));
    add(Command::new("draw.prev_tool", t("draw-prev-tool"), K::Tools, Action::CycleTool(-1))
        .key("Shift+Tab")
        .when(c::has_doc));
    add(Command::new("draw.size_down", t("draw-size-down"), K::Tools, Action::ToolSize(-1)).key("[").when(c::has_doc));
    add(Command::new("draw.size_up", t("draw-size-up"), K::Tools, Action::ToolSize(1)).key("]").when(c::has_doc));
    add(Command::new("edit.crop", t("editor-crop"), K::Select, Action::CropToSelection)
        .key("Ctrl+T")
        .icon("⛶")
        .when(c::has_selection));
    add(Command::new("edit.new_tab", t("editor-send-new-tab"), K::Edit, Action::SendToNewTab)
        .key("Alt+C")
        .icon("⧉")
        .when(c::has_doc));
    add(Command::new("edit.pin", t("editor-pin"), K::Edit, Action::PinToScreen)
        .icon("📌")
        .when(c::has_doc)
        .keywords("float on top"));
    add(Command::new("tool.pan", t("editor-tool-pan"), K::Select, Action::Tool(ToolId::Pan))
        .key("H")
        .icon("✋")
        .keywords("hand move"));
    add(Command::new("tool.rect", t("editor-tool-rect"), K::Select, Action::Tool(ToolId::SelectRect))
        .key("M")
        .icon("⬚")
        .keywords("marquee"));
    add(Command::new("tool.free", t("editor-tool-free"), K::Select, Action::Tool(ToolId::SelectFree))
        .key("L")
        .icon("➰")
        .keywords("lasso"));
    for tool in tools {
        let mut cmd = Command::new(
            format!("tool.{}", tool.id()),
            tool.label(),
            K::Tools,
            Action::Tool(ToolId::External(tool.id().to_string())),
        )
        .when(c::has_doc);
        cmd.shortcut = tool.shortcut().and_then(c::Shortcut::parse);
        cmd.icon = tool.icon();
        add(cmd);
    }

    // Image / Effects
    for d in effects.iter() {
        let cat = if d.menu == MenuSlot::Image { K::Image } else { K::Effects };
        let mut cmd =
            Command::new(d.command_id(), d.menu_label(), cat, Action::Effect(d.id.to_string())).when(c::editable);
        cmd.shortcut = d.shortcut.and_then(c::Shortcut::parse);
        cmd.icon = d.icon;
        add(cmd);
    }

    // Tools
    add(Command::new("tools.combine", t("editor-tool-combine"), K::Tools, Action::CombineImages)
        .icon("▦")
        .keywords("merge join stitch"));
    add(Command::new("tools.pdf", t("editor-tool-pdf"), K::Tools, Action::ImagesToPdf).icon("📄"));
    add(Command::new("tools.batch_print", t("editor-tool-batch-print"), K::Tools, Action::BatchPrint).icon("🖶"));
    add(Command::new("tools.video_editor", t("menu-video-editor"), K::Tools, Action::VideoEditor)
        .icon("🎬")
        .keywords("video trim cut mp4 recording"));
    add(Command::new("tools.convert_mp4", t("menu-convert-mp4"), K::Tools, Action::ConvertToMp4)
        .icon("🎞")
        .keywords("video wmv audio mp3 m4a extract resize"));
    add(Command::new("tools.schedule_recording", t("menu-record-schedule"), K::Tools, Action::ScheduleRecording)
        .icon("⏰")
        .keywords("record timer"));
    add(Command::new("tools.history", t("editor-tool-history"), K::Tools, Action::History)
        .key("Ctrl+H")
        .icon("🕘")
        .keywords("captures recent previous search find clipboard"));
    add(Command::new("tools.settings", t("editor-settings"), K::Tools, Action::Settings)
        .key("Ctrl+,")
        .icon("⚙")
        .keywords("options preferences configure"));
    add(Command::new("tools.palette", t("editor-palette"), K::Tools, Action::Palette)
        .key("Ctrl+K")
        .icon("🔍")
        .hidden());

    // View
    add(Command::new("view.zoom_in", t("editor-zoom-in"), K::View, Action::Zoom(ZoomCmd::In))
        .key("Ctrl+=")
        .alt_key("Ctrl++")
        .alt_key("+")
        .alt_key("=")
        .icon("➕")
        .when(c::has_doc));
    add(Command::new("view.zoom_out", t("editor-zoom-out"), K::View, Action::Zoom(ZoomCmd::Out))
        .key("Ctrl+-")
        .alt_key("-")
        .icon("➖")
        .when(c::has_doc));
    add(Command::new("view.actual", t("editor-zoom-actual"), K::View, Action::Zoom(ZoomCmd::Actual))
        .key("Ctrl+0")
        .when(c::has_doc));
    add(Command::new("view.fit", t("editor-zoom-fit-window"), K::View, Action::Zoom(ZoomCmd::FitWindow))
        .key("F")
        .when(c::has_doc));
    add(Command::new("view.fit_width", t("editor-zoom-fit-width-cmd"), K::View, Action::Zoom(ZoomCmd::FitWidth))
        .key("K")
        .when(c::has_doc));
    add(Command::new("view.zoom_selection", t("editor-zoom-selection"), K::View, Action::Zoom(ZoomCmd::Selection))
        .key("Z")
        .when(c::has_selection));
    for p in view::PRESETS {
        add(Command::new(
            format!("view.zoom.{p}"),
            tf("editor-zoom-to", &[("pct", &p.to_string())]),
            K::View,
            Action::Zoom(ZoomCmd::Set(p)),
        )
        .when(c::has_doc));
    }
    add(Command::new("view.next_tab", t("editor-next-tab"), K::View, Action::NextTab)
        .key("Ctrl+Tab")
        .when(c::multiple_docs));
    add(Command::new("view.prev_tab", t("editor-prev-tab"), K::View, Action::PrevTab)
        .key("Ctrl+Shift+Tab")
        .when(c::multiple_docs));
    add(Command::new("view.workspaces", t("editor-workspaces-toggle"), K::View, Action::ToggleWorkspaces)
        .keywords("group tabs"));
    add(Command::new("view.new_workspace", t("editor-workspace-new"), K::View, Action::NewWorkspace)
        .keywords("group tabs"));
    add(Command::new("view.draw_palette", t("draw-palette-toggle"), K::View, Action::ToggleDrawPalette)
        .keywords("draw tools annotate sidebar"));

    // Help
    add(Command::new("help.shortcuts", t("editor-shortcuts"), K::Help, Action::Shortcuts).key("F1").icon("⌨"));
    add(Command::new("help.about", t("editor-about"), K::Help, Action::About).icon("ℹ"));
    r
}

impl EditorApp {
    fn cmd_enabled(&self, id: &str) -> bool {
        self.registry.get(id).is_some_and(|c| (c.enabled)(&self.cmd_state))
    }

    /// One menu entry for a registered command.
    /// Right-click menu on the image.
    pub(super) fn canvas_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(220.0);
        self.item(ui, "edit.copy_image");
        if self.cmd_state.has_selection {
            self.item(ui, "edit.copy");
            self.item(ui, "edit.cut");
        }
        self.item(ui, "edit.paste");
        ui.separator();
        if self.cmd_state.has_selection {
            self.item(ui, "edit.crop");
            self.item(ui, "edit.deselect");
        } else {
            self.item(ui, "edit.select_all");
        }
        ui.separator();
        self.item(ui, "file.save");
        self.item(ui, "file.save_as");
        self.item(ui, "edit.pin");
        if self.cmd_state.has_path {
            self.item(ui, "file.open_folder");
        }
    }

    fn item(&mut self, ui: &mut egui::Ui, id: &str) {
        let Some(cmd) = self.registry.get(id) else { return };
        if menu_entry(ui, cmd, &self.cmd_state) {
            self.actions.push(cmd.action.clone());
            ui.close_menu();
        }
    }

    pub(super) fn menu_bar(&mut self, ctx: &egui::Context) {
        let frame = egui::Frame::new().fill(self.pal.bar).inner_margin(Margin { left: 6, right: 8, top: 3, bottom: 0 });
        egui::TopBottomPanel::top("menubar").frame(frame).show_separator_line(false).show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.visuals_mut().widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
                ui.menu_button(t("editor-file"), |ui| self.file_menu(ui));
                ui.menu_button(t("editor-menu-edit"), |ui| self.edit_menu(ui));
                ui.menu_button(t("editor-menu-image"), |ui| self.effects_menu(ui, MenuSlot::Image));
                ui.menu_button(t("editor-menu-effects"), |ui| self.effects_menu(ui, MenuSlot::Effects));
                ui.menu_button(t("editor-menu-tools"), |ui| self.tools_menu(ui));
                ui.menu_button(t("editor-menu-view"), |ui| self.view_menu(ui));
                ui.menu_button(t("editor-menu-help"), |ui| {
                    ui.set_min_width(240.0);
                    self.item(ui, "tools.palette");
                    self.item(ui, "help.shortcuts");
                    ui.separator();
                    self.item(ui, "help.about");
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let label = format!("🔍  {}", t("editor-palette-button"));
                    let btn = egui::Button::new(RichText::new(label).color(self.pal.muted_text))
                        .shortcut_text("Ctrl+K")
                        .min_size(vec2(240.0, 24.0))
                        .fill(if self.pal.dark { Color32::from_rgb(0x2A, 0x2C, 0x33) } else { Color32::WHITE })
                        .stroke(Stroke::new(1.0_f32, self.pal.border));
                    if ui.add(btn).on_hover_text(t("editor-palette-tip")).clicked() {
                        self.actions.push(Action::Palette);
                    }
                });
            });
        });
    }

    fn file_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(260.0);
        self.item(ui, "file.open");
        ui.menu_button(t("editor-recent"), |ui| self.recent_menu(ui));
        ui.separator();
        for id in ["file.save", "file.save_as", "file.save_all", "file.save_sequence", "file.save_project"] {
            self.item(ui, id);
        }
        ui.separator();
        self.item(ui, "file.print");
        self.item(ui, "file.scan");
        ui.menu_button(t("editor-edit-with"), |ui| {
            ui.set_min_width(220.0);
            let n = settings::get().editor.external_editors.len();
            for i in 0..n {
                self.item(ui, &format!("file.edit_with.{i}"));
            }
            if n > 0 {
                ui.separator();
            }
            self.item(ui, "file.configure_editors");
        });
        ui.separator();
        for id in ["file.properties", "file.open_folder", "file.delete"] {
            self.item(ui, id);
        }
        if self.cmd_enabled("file.continue") {
            self.item(ui, "file.continue");
        }
        ui.separator();
        self.item(ui, "file.close");
        self.item(ui, "file.close_saved");
    }

    /// Recent files: click opens one; Ctrl+click marks several, then "Open Selected".
    fn recent_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(340.0);
        let recent = settings::get().editor.recent_files.clone();
        if recent.is_empty() {
            ui.add_enabled(false, egui::Button::new(t("editor-recent-empty")));
        }
        for p in &recent {
            let marked = self.recent_marks.iter().any(|m| m.eq_ignore_ascii_case(p));
            let mark = if marked { "☑ " } else { "" };
            let resp = ui
                .add(egui::Button::new(format!("{mark}{}", file_name(Path::new(p)))).selected(marked))
                .on_hover_text(format!("{p}\n{}", t("editor-recent-ctrl-tip")));
            if resp.clicked() {
                if ui.input(|i| i.modifiers.ctrl || i.modifiers.command) {
                    if marked {
                        self.recent_marks.retain(|m| !m.eq_ignore_ascii_case(p));
                    } else {
                        self.recent_marks.push(p.clone());
                    }
                } else {
                    let mut all = std::mem::take(&mut self.recent_marks);
                    if !all.iter().any(|m| m.eq_ignore_ascii_case(p)) {
                        all.push(p.clone());
                    }
                    self.actions.push(Action::OpenRecent(all));
                    ui.close_menu();
                }
            }
        }
        if !self.recent_marks.is_empty() {
            ui.separator();
            let label = tf("editor-recent-open-selected", &[("count", &self.recent_marks.len().to_string())]);
            if ui.add(style::primary_button(label)).clicked() {
                self.actions.push(Action::OpenRecent(std::mem::take(&mut self.recent_marks)));
                ui.close_menu();
            }
        }
        if !recent.is_empty() {
            ui.separator();
            if ui.button(t("editor-recent-clear")).clicked() {
                self.actions.push(Action::ClearRecent);
                ui.close_menu();
            }
        }
    }

    fn edit_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(260.0);
        self.item(ui, "edit.undo");
        self.item(ui, "edit.redo");
        // Named history: jump several steps at once.
        let (undo, redo): (Vec<String>, Vec<String>) = match self.tabs.active() {
            Some(d) => (
                d.history.undo_names().take(20).map(String::from).collect(),
                d.history.redo_names().take(20).map(String::from).collect(),
            ),
            None => (Vec::new(), Vec::new()),
        };
        ui.add_enabled_ui(!undo.is_empty() || !redo.is_empty(), |ui| {
            ui.menu_button(t("editor-history"), |ui| {
                ui.set_min_width(240.0);
                for (i, name) in redo.iter().enumerate().rev() {
                    if ui
                        .button(RichText::new(format!("↷  {name}")).color(self.pal.muted_text))
                        .on_hover_text(t("editor-redo"))
                        .clicked()
                    {
                        self.actions.push(Action::RedoMany(i + 1));
                        ui.close_menu();
                    }
                }
                ui.label(RichText::new(format!("●  {}", t("editor-history-current"))).strong());
                for (i, name) in undo.iter().enumerate() {
                    if ui.button(format!("↶  {name}")).on_hover_text(t("editor-undo")).clicked() {
                        self.actions.push(Action::UndoMany(i + 1));
                        ui.close_menu();
                    }
                }
            });
        });
        ui.separator();
        for id in ["edit.cut", "edit.copy", "edit.paste", "edit.paste_tab"] {
            self.item(ui, id);
        }
        ui.separator();
        for id in ["edit.select_all", "edit.deselect", "edit.delete", "edit.crop"] {
            self.item(ui, id);
        }
        ui.separator();
        self.item(ui, "edit.new_tab");
        self.item(ui, "edit.pin");
    }

    fn effects_menu(&mut self, ui: &mut egui::Ui, slot: MenuSlot) {
        ui.set_min_width(250.0);
        let groups: Vec<Vec<String>> =
            self.effects.menu(slot).into_iter().map(|g| g.into_iter().map(|d| d.command_id()).collect()).collect();
        if slot == MenuSlot::Image {
            self.item(ui, "edit.crop");
            ui.separator();
        }
        for (i, g) in groups.iter().enumerate() {
            if i > 0 {
                ui.separator();
            }
            for id in g {
                self.item(ui, id);
            }
        }
    }

    fn tools_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(250.0);
        for id in ["tool.pan", "tool.rect", "tool.free"] {
            self.item(ui, id);
        }
        let ext: Vec<String> =
            self.ext_tools.iter().filter(|t| t.on_ribbon()).map(|t| format!("tool.{}", t.id())).collect();
        let draw: Vec<String> =
            self.ext_tools.iter().filter(|t| !t.on_ribbon()).map(|t| format!("tool.{}", t.id())).collect();
        if !ext.is_empty() || !draw.is_empty() {
            ui.separator();
            for id in ext {
                self.item(ui, &id);
            }
            if !draw.is_empty() {
                ui.menu_button(t("editor-menu-draw"), |ui| {
                    ui.set_min_width(240.0);
                    for id in draw {
                        self.item(ui, &id);
                    }
                    ui.separator();
                    self.item(ui, "draw.next_tool");
                    self.item(ui, "draw.prev_tool");
                });
            }
        }
        self.item(ui, "tools.settings");
        ui.separator();
        for id in ["tools.combine", "tools.pdf", "tools.batch_print"] {
            self.item(ui, id);
        }
        ui.separator();
        for id in ["tools.video_editor", "tools.convert_mp4", "tools.schedule_recording"] {
            self.item(ui, id);
        }
        ui.separator();
        self.item(ui, "tools.history");
        self.item(ui, "tools.palette");
    }

    fn view_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(250.0);
        for id in ["view.zoom_in", "view.zoom_out", "view.actual", "view.fit", "view.fit_width", "view.zoom_selection"]
        {
            self.item(ui, id);
        }
        ui.menu_button(t("editor-zoom-presets"), |ui| {
            for p in view::PRESETS {
                self.item(ui, &format!("view.zoom.{p}"));
            }
        });
        ui.separator();
        self.item(ui, "view.next_tab");
        self.item(ui, "view.prev_tab");
        ui.separator();
        let on = self.cmd_state.workspaces;
        if ui.checkbox(&mut on.clone(), t("editor-workspaces-toggle")).clicked() {
            self.actions.push(Action::ToggleWorkspaces);
            ui.close_menu();
        }
        self.item(ui, "view.new_workspace");
        ui.separator();
        self.item(ui, "view.draw_palette");
    }

    // ------------------------------------------------------------------ ribbon

    pub(super) fn ribbon(&mut self, ctx: &egui::Context) {
        let frame = egui::Frame::new().fill(self.pal.bar).inner_margin(Margin { left: 8, right: 8, top: 2, bottom: 4 });
        egui::TopBottomPanel::top("ribbon").frame(frame).show_separator_line(false).show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.visuals_mut().widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
                ui.spacing_mut().item_spacing.x = 2.0;
                self.group(ui, "editor-group-file", &["file.open", "file.save", "file.save_all"]);
                self.group(ui, "editor-group-history", &["edit.undo", "edit.redo"]);
                self.group(ui, "editor-group-clipboard", &["edit.copy", "edit.cut", "edit.paste"]);
                let mut tools = vec!["tool.pan".to_string(), "tool.rect".into(), "tool.free".into()];
                tools.extend(self.ext_tools.iter().filter(|t| t.on_ribbon()).map(|t| format!("tool.{}", t.id())));
                let tools: Vec<&str> = tools.iter().map(String::as_str).collect();
                self.group(ui, "editor-group-tools", &tools);
                self.group(ui, "editor-group-selection", &["edit.crop", "edit.new_tab", "edit.pin"]);
                self.zoom_group(ui);
                self.active_tool_options(ui);
                let cont = self.tabs.active().and_then(|d| d.continue_to);
                if let Some(dest) = cont {
                    // Inline (not right-aligned) so it wraps with the groups on narrow windows.
                    let label = tf("editor-continue", &[("dest", &t(dest.label_key()))]);
                    let btn = style::primary_button(format!("{label}  ➡")).min_size(vec2(0.0, 30.0));
                    ui.add_space(8.0);
                    if ui.add(btn).clicked() {
                        self.actions.push(Action::Continue);
                    }
                }
            });
        });
    }

    /// A ribbon group: icon buttons with a small caption underneath.
    fn group(&mut self, ui: &mut egui::Ui, caption: &str, ids: &[&str]) {
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                for id in ids {
                    let Some(cmd) = self.registry.get(id) else { continue };
                    let enabled = (cmd.enabled)(&self.cmd_state);
                    let selected = matches!(&cmd.action, Action::Tool(t) if *t == self.tool);
                    let icon = if cmd.icon.is_empty() {
                        cmd.label.chars().next().map(String::from).unwrap_or_default()
                    } else {
                        cmd.icon.to_string()
                    };
                    let btn =
                        egui::Button::new(RichText::new(icon).size(17.0)).min_size(vec2(34.0, 30.0)).selected(selected);
                    let resp =
                        ui.add_enabled(enabled, btn).on_hover_text(cmd.tooltip()).on_disabled_hover_text(cmd.tooltip());
                    if resp.clicked() {
                        self.actions.push(cmd.action.clone());
                    }
                }
            });
            ui.label(RichText::new(t(caption)).size(10.5).color(self.pal.muted_text));
        });
        separator(ui, &self.pal);
    }

    fn zoom_group(&mut self, ui: &mut egui::Ui) {
        let has = self.cmd_state.has_doc;
        let pct = self.tabs.active().map(|d| d.view.percent()).unwrap_or(100);
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                self.icon_button(ui, "view.zoom_out");
                ui.add_enabled_ui(has, |ui| {
                    ui.menu_button(RichText::new(format!("{pct}%")).monospace(), |ui| {
                        for p in view::PRESETS {
                            self.item(ui, &format!("view.zoom.{p}"));
                        }
                    })
                    .response
                    .on_hover_text(t("editor-zoom-presets"));
                });
                self.icon_button(ui, "view.zoom_in");
                for (id, label) in [
                    ("view.fit", t("editor-zoom-fit")),
                    ("view.actual", "1:1".into()),
                    ("view.fit_width", t("editor-zoom-fit-width")),
                ] {
                    if let Some(cmd) = self.registry.get(id) {
                        let enabled = (cmd.enabled)(&self.cmd_state);
                        let resp = ui
                            .add_enabled(enabled, egui::Button::new(label).min_size(vec2(0.0, 30.0)))
                            .on_hover_text(cmd.tooltip());
                        if resp.clicked() {
                            self.actions.push(cmd.action.clone());
                        }
                    }
                }
            });
            ui.label(RichText::new(t("editor-group-view")).size(10.5).color(self.pal.muted_text));
        });
    }

    fn icon_button(&mut self, ui: &mut egui::Ui, id: &str) {
        let Some(cmd) = self.registry.get(id) else { return };
        let enabled = (cmd.enabled)(&self.cmd_state);
        let resp = ui
            .add_enabled(enabled, egui::Button::new(cmd.icon).min_size(vec2(30.0, 30.0)))
            .on_hover_text(cmd.tooltip());
        if resp.clicked() {
            self.actions.push(cmd.action.clone());
        }
    }

    /// The active Phase 3 tool's option strip.
    fn active_tool_options(&mut self, ui: &mut egui::Ui) -> Option<()> {
        let ToolId::External(id) = &self.tool else { return None };
        let id = id.clone();
        let tool = self.ext_tools.iter_mut().find(|t| t.id() == id)?;
        tool.options_ui(ui);
        Some(())
    }

    // ------------------------------------------------------------------ workspaces & tabs

    pub(super) fn workspace_strip(&mut self, ctx: &egui::Context) {
        if !self.cmd_state.workspaces {
            return;
        }
        let list: Vec<(u64, String)> =
            settings::get().editor_ext.workspaces.iter().map(|w| (w.id, w.name.clone())).collect();
        let active = EditorApp::active_ws();
        let frame =
            egui::Frame::new().fill(self.pal.bar).inner_margin(Margin { left: 10, right: 8, top: 2, bottom: 2 });
        egui::TopBottomPanel::top("workspaces").frame(frame).show_separator_line(false).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(RichText::new(t("editor-workspace")).size(11.0).color(self.pal.muted_text));
                for (id, name) in &list {
                    let count = self.tabs.docs.iter().filter(|d| d.workspace == *id).count();
                    let selected = *id == active;
                    let text = RichText::new(format!("{name}  {count}")).size(12.0);
                    let btn = egui::Button::new(text)
                        .selected(selected)
                        .corner_radius(CornerRadius::same(10))
                        .min_size(vec2(0.0, 22.0));
                    let resp = ui.add(btn).on_hover_text(t("editor-workspace-tip"));
                    if resp.clicked() && !selected {
                        self.actions.push(Action::SwitchWorkspace(*id));
                    }
                    resp.context_menu(|ui| {
                        if ui.button(t("editor-workspace-rename")).clicked() {
                            self.actions.push(Action::RenameWorkspace(*id));
                            ui.close_menu();
                        }
                        if ui.add_enabled(list.len() > 1, egui::Button::new(t("editor-workspace-close"))).clicked() {
                            self.actions.push(Action::CloseWorkspace(*id));
                            ui.close_menu();
                        }
                    });
                }
                if ui
                    .add(egui::Button::new("＋").min_size(vec2(22.0, 22.0)).corner_radius(CornerRadius::same(10)))
                    .on_hover_text(t("editor-workspace-new"))
                    .clicked()
                {
                    self.actions.push(Action::NewWorkspace);
                }
            });
        });
    }

    pub(super) fn tab_strip(&mut self, ctx: &egui::Context) {
        if self.tabs.is_empty() {
            return;
        }
        let frame = egui::Frame::new().fill(self.pal.bar).inner_margin(Margin { left: 8, right: 8, top: 0, bottom: 0 });
        egui::TopBottomPanel::top("tabs").frame(frame).show_separator_line(false).show(ctx, |ui| {
            egui::ScrollArea::horizontal()
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        let active = self.tabs.active_id();
                        let tabs: Vec<(DocId, String, bool)> =
                            self.tabs.visible().map(|d| (d.id, d.title.clone(), d.dirty)).collect();
                        for (id, title, dirty) in tabs {
                            self.tab(ui, id, &title, dirty, Some(id) == active);
                        }
                    });
                });
        });
    }

    fn tab(&mut self, ui: &mut egui::Ui, id: DocId, title: &str, dirty: bool, selected: bool) {
        let pal = self.pal;
        let font = egui::FontId::proportional(13.5);
        let text_color = if selected { ui.visuals().strong_text_color() } else { pal.muted_text };
        let shown: String = if title.chars().count() > 32 {
            format!("{}…", title.chars().take(31).collect::<String>())
        } else {
            title.to_string()
        };
        let galley = ui.painter().layout_no_wrap(shown, font, text_color);
        let (pad, close, h) = (12.0, 18.0, 32.0);
        let dot_w = if dirty { 12.0 } else { 0.0 };
        let w = pad + galley.size().x + dot_w + 8.0 + close + pad * 0.6;
        let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
        let close_rect = egui::Rect::from_center_size(
            egui::pos2(rect.right() - pad * 0.6 - close / 2.0, rect.center().y),
            vec2(close, close),
        );
        let close_resp = ui.interact(close_rect, Id::new(("tab-close", id)), Sense::click());
        let resp = resp.on_hover_text(title);

        let fill = if selected {
            pal.tab_active
        } else if resp.hovered() || close_resp.hovered() {
            pal.tab_hover
        } else {
            Color32::TRANSPARENT
        };
        let p = ui.painter();
        p.rect_filled(rect, CornerRadius { nw: 8, ne: 8, sw: 0, se: 0 }, fill);
        if selected {
            let bar = egui::Rect::from_min_max(
                egui::pos2(rect.left() + 6.0, rect.top()),
                egui::pos2(rect.right() - 6.0, rect.top() + 2.5),
            );
            p.rect_filled(bar, CornerRadius::same(2), style::ACCENT);
        }
        let text_pos = egui::pos2(rect.left() + pad, rect.center().y - galley.size().y / 2.0);
        let text_w = galley.size().x;
        p.galley(text_pos, galley, text_color);
        if dirty {
            p.circle_filled(egui::pos2(text_pos.x + text_w + 8.0, rect.center().y), 3.5, pal.dirty);
        }
        if close_resp.hovered() {
            p.rect_filled(close_rect, CornerRadius::same(4), ui.visuals().widgets.hovered.weak_bg_fill);
        }
        if selected || resp.hovered() || close_resp.hovered() {
            let c = close_rect.center();
            let s = 4.0;
            let stroke = Stroke::new(
                1.4_f32,
                if close_resp.hovered() { ui.visuals().strong_text_color() } else { pal.muted_text },
            );
            p.line_segment([c + vec2(-s, -s), c + vec2(s, s)], stroke);
            p.line_segment([c + vec2(-s, s), c + vec2(s, -s)], stroke);
        }
        resp.context_menu(|ui| {
            if ui.button(t("editor-close-tab")).clicked() {
                self.actions.push(Action::Close(id));
                ui.close_menu();
            }
            if ui.button(t("editor-close-saved")).clicked() {
                self.actions.push(Action::CloseSaved);
                ui.close_menu();
            }
            let ws: Vec<(u64, String)> = if self.cmd_state.workspaces {
                settings::get().editor_ext.workspaces.iter().map(|w| (w.id, w.name.clone())).collect()
            } else {
                Vec::new()
            };
            if ws.len() > 1 {
                ui.menu_button(t("editor-move-to-workspace"), |ui| {
                    for (wid, name) in ws {
                        if ui.button(name).clicked() {
                            if let Some(d) = self.tabs.get_mut(id) {
                                d.workspace = wid;
                            }
                            self.apply_workspace_filter();
                            ui.close_menu();
                        }
                    }
                });
            }
        });
        if close_resp.on_hover_text(format!("{} (Ctrl+W)", t("editor-close-tab"))).clicked() || resp.middle_clicked() {
            self.actions.push(Action::Close(id));
        } else if resp.clicked() {
            self.actions.push(Action::Activate(id));
        }
    }

    // ------------------------------------------------------------------ status bar

    pub(super) fn status_bar(&mut self, ctx: &egui::Context) {
        let frame = egui::Frame::new().fill(self.pal.bar).inner_margin(Margin::symmetric(12, 4));
        egui::TopBottomPanel::bottom("status").frame(frame).exact_height(28.0).show_separator_line(false).show(
            ctx,
            |ui| {
                ui.horizontal_centered(|ui| {
                    let muted = self.pal.muted_text;
                    let dot = |ui: &mut egui::Ui| ui.label(RichText::new("·").color(muted));
                    if let Some(d) = self.tabs.active() {
                        ui.label(RichText::new(format!("{} × {} px", d.image.width, d.image.height)).color(muted));
                        dot(ui);
                        ui.label(RichText::new(format!("{}%", d.view.percent())).color(muted));
                        if let Some((x, y)) = self.hover_px {
                            dot(ui);
                            ui.label(RichText::new(format!("{x}, {y}")).monospace().color(muted));
                        }
                        if let Some(sel) = &d.selection {
                            dot(ui);
                            let b = sel.bounds;
                            let colors = match self.stats.colors {
                                Some(n) => n.to_string(),
                                None => "…".into(),
                            };
                            let text = tf(
                                "editor-selection-stats",
                                &[
                                    ("w", &b.w.to_string()),
                                    ("h", &b.h.to_string()),
                                    ("pixels", &sel.pixel_count().to_string()),
                                    ("colors", &colors),
                                ],
                            );
                            ui.label(RichText::new(text).color(style::ACCENT))
                                .on_hover_text(format!("{}, {}  {} × {}", b.x, b.y, b.w, b.h));
                        }
                        dot(ui);
                        let hist = tf(
                            "editor-history-status",
                            &[
                                ("steps", &d.history.undo_len().to_string()),
                                ("size", &format_bytes(d.history.total_bytes() as u64)),
                            ],
                        );
                        ui.label(RichText::new(hist).color(muted));
                        dot(ui);
                        match (&d.path, &d.project) {
                            (Some(p), _) | (None, Some(p)) => {
                                ui.label(RichText::new(p.display().to_string()).color(muted))
                            }
                            (None, None) => ui.label(RichText::new(t("editor-unsaved")).color(muted).italics()),
                        };
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let saving = self.tabs.docs.iter().any(|d| d.saving);
                        let busy = self.tabs.active().is_some_and(|d| d.busy());
                        let key = if saving {
                            Some("editor-saving")
                        } else if self.loading > 0 {
                            Some("editor-loading")
                        } else if self.applying.is_some() {
                            Some("editor-applying")
                        } else if self.scanning {
                            Some("editor-scanning")
                        } else if busy {
                            Some("editor-restoring")
                        } else {
                            None
                        };
                        if let Some(key) = key {
                            ui.label(RichText::new(t(key)).color(muted));
                            ui.add(egui::Spinner::new().size(14.0));
                        }
                    });
                });
            },
        );
    }
}

/// Menu entry with shortcut text and tooltip; returns true when clicked.
fn menu_entry(ui: &mut egui::Ui, cmd: &Command, st: &CmdState) -> bool {
    let enabled = (cmd.enabled)(st);
    let label =
        if cmd.icon.is_empty() { format!("     {}", cmd.label) } else { format!("{}  {}", cmd.icon, cmd.label) };
    let btn = egui::Button::new(label).shortcut_text(cmd.shortcut_text());
    ui.add_enabled(enabled, btn).clicked()
}

fn separator(ui: &mut egui::Ui, pal: &Palette) {
    let (r, _) = ui.allocate_exact_size(vec2(13.0, 44.0), Sense::hover());
    ui.painter().vline(r.center().x, (r.top() + 4.0)..=(r.bottom() - 4.0), Stroke::new(1.0_f32, pal.border));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_no_shortcut_conflicts_and_covers_spec_keys() {
        let effects = EffectRegistry::with_placeholders();
        let r = build_registry(&effects, &[], &["C:\\a.png".into()], &["MS Paint".into()]);
        assert!(r.conflicts().is_empty(), "{:?}", r.conflicts());
        let key = |s: &str| r.get(s).and_then(|c| c.shortcut).map(|s| s.label());
        assert_eq!(key("edit.undo").as_deref(), Some("Ctrl+Z"));
        assert_eq!(key("edit.redo").as_deref(), Some("Ctrl+Y"));
        assert_eq!(key("file.save_all").as_deref(), Some("Ctrl+Shift+S"));
        assert_eq!(key("file.delete").as_deref(), Some("Ctrl+Del"));
        assert_eq!(key("file.close_saved").as_deref(), Some("Ctrl+Q"));
        assert_eq!(key("edit.new_tab").as_deref(), Some("Alt+C"));
        assert_eq!(key("edit.crop").as_deref(), Some("Ctrl+T"));
        assert_eq!(key("view.zoom_selection").as_deref(), Some("Z"));
        assert_eq!(key("view.fit_width").as_deref(), Some("K"));
        assert_eq!(key("view.actual").as_deref(), Some("Ctrl+0"));
        assert_eq!(key("file.properties").as_deref(), Some("I"));
        assert_eq!(key("tools.palette").as_deref(), Some("Ctrl+K"));
        // Effects, recent files and editors are commands too.
        assert!(r.get("effect:effects.blur").is_some());
        assert!(r.get("recent.0").is_some());
        assert!(r.get("file.edit_with.0").is_some());
        assert!(r.search("blur", 3).iter().any(|c| c.id == "effect:effects.blur"));
        assert!(r.search("a.png", 3).iter().any(|c| c.id == "recent.0"));
    }
}
