//! Modal dialogs: unsaved changes, About, File Properties, Delete File, Save All with
//! sequence, external editors, scanning, reload after an external edit, shortcuts and
//! workspace rename. Enter confirms and Esc cancels everywhere.

use super::app::{default_folder, file_name, remember_folder, EditorApp};
use super::commands::{Action, Category};
use super::document::DocId;
use super::fileops::{format_bytes, plan_sequence, FileProps};
use super::scan_post::PostOptions;
use super::{dialogs, style};
use crate::common::win;
use crate::i18n::{t, tf};
use crate::settings::editor_ext::ScanSettings;
use crate::settings::{self, ExternalEditor, ImageFormat};
use eframe::egui::{self, vec2, Align, Id, Key, Layout, Modifiers, RichText};
use std::path::PathBuf;

pub struct SequenceForm {
    pub folder: String,
    pub base: String,
    pub start: u32,
    pub digits: u32,
    pub format: ImageFormat,
}

pub enum Modal {
    ConfirmClose(DocId),
    About,
    Properties(DocId, Option<FileProps>),
    ConfirmDelete(DocId, PathBuf),
    Sequence(SequenceForm),
    Editors(Vec<ExternalEditor>),
    Scan(ScanSettings),
    NoScanner,
    Reload { doc: DocId, path: PathBuf, editor: String },
    Shortcuts,
    RenameWorkspace(u64, String),
}

impl Modal {
    pub fn sequence() -> Modal {
        let s = settings::get();
        let e = &s.editor_ext;
        let folder = if e.sequence_folder.trim().is_empty() {
            default_folder().to_string_lossy().into_owned()
        } else {
            e.sequence_folder.clone()
        };
        Modal::Sequence(SequenceForm {
            folder,
            base: e.sequence_base.clone(),
            start: e.sequence_start,
            digits: e.sequence_digits.clamp(1, 9),
            format: s.output.format,
        })
    }
}

/// What the buttons row returned.
#[derive(PartialEq)]
enum Choice {
    None,
    Ok,
    Alt,
    Cancel,
}

fn title(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(17.0).strong());
    ui.add_space(6.0);
}

/// Right-aligned OK / (optional alt) / Cancel buttons; Enter = OK, Esc = Cancel.
fn buttons(ui: &mut egui::Ui, ok: &str, alt: Option<&str>, ok_enabled: bool) -> Choice {
    let mut c = Choice::None;
    ui.add_space(14.0);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if ui
            .add_enabled(ok_enabled, style::primary_button(ok).min_size(vec2(88.0, 28.0)))
            .on_hover_text("Enter")
            .clicked()
        {
            c = Choice::Ok;
        }
        if let Some(a) = alt {
            if ui.add(egui::Button::new(a).min_size(vec2(88.0, 28.0))).clicked() {
                c = Choice::Alt;
            }
        }
        if ui.add(egui::Button::new(t("editor-cancel")).min_size(vec2(84.0, 28.0))).on_hover_text("Esc").clicked() {
            c = Choice::Cancel;
        }
    });
    if c == Choice::None && ok_enabled && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
        c = Choice::Ok;
    }
    c
}

fn kv(ui: &mut egui::Ui, k: &str, v: &str) {
    ui.label(RichText::new(k).weak());
    ui.add(egui::Label::new(v).wrap());
    ui.end_row();
}

impl EditorApp {
    pub(super) fn modals(&mut self, ctx: &egui::Context) {
        let Some(modal) = self.modal.take() else { return };
        self.modal = match modal {
            Modal::ConfirmClose(id) => self.confirm_close(ctx, id),
            Modal::About => self.about(ctx),
            Modal::Properties(id, p) => self.properties(ctx, id, p),
            Modal::ConfirmDelete(id, p) => self.confirm_delete(ctx, id, p),
            Modal::Sequence(f) => self.sequence(ctx, f),
            Modal::Editors(list) => self.editors(ctx, list),
            Modal::Scan(s) => self.scan_options(ctx, s),
            Modal::NoScanner => self.no_scanner(ctx),
            Modal::Reload { doc, path, editor } => self.reload_prompt(ctx, doc, path, editor),
            Modal::Shortcuts => self.shortcuts_list(ctx),
            Modal::RenameWorkspace(id, name) => self.rename_workspace(ctx, id, name),
        };
    }

    fn confirm_close(&mut self, ctx: &egui::Context, id: DocId) -> Option<Modal> {
        let title_text = self.tabs.docs.iter().find(|d| d.id == id).map(|d| d.title.clone())?;
        let mut choice = Choice::None;
        let resp = egui::Modal::new(Id::new("confirm-close")).show(ctx, |ui| {
            ui.set_width(400.0);
            title(ui, &t("editor-close-prompt-title"));
            ui.label(tf("editor-close-prompt", &[("name", &title_text)]));
            choice = buttons(ui, &t("editor-save"), Some(&t("editor-discard")), true);
        });
        match choice {
            Choice::Ok => {
                self.save(id, false, true);
                None
            }
            Choice::Alt => {
                self.actions.push(Action::Discard(id));
                None
            }
            Choice::Cancel => None,
            Choice::None if resp.should_close() => None,
            Choice::None => Some(Modal::ConfirmClose(id)),
        }
    }

    fn about(&mut self, ctx: &egui::Context) -> Option<Modal> {
        if self.about_icon.is_none() {
            self.about_icon = style::app_icon().map(|i| {
                let ci = egui::ColorImage::from_rgba_unmultiplied([i.width as usize, i.height as usize], &i.rgba);
                ctx.load_texture("about-icon", ci, egui::TextureOptions::LINEAR)
            });
        }
        let icon = self.about_icon.clone();
        let mut close = false;
        let resp = egui::Modal::new(Id::new("about")).show(ctx, |ui| {
            ui.set_width(340.0);
            ui.vertical_centered(|ui| {
                if let Some(icon) = &icon {
                    ui.add(egui::Image::new(icon).fit_to_exact_size(vec2(64.0, 64.0)));
                }
                ui.add_space(4.0);
                ui.label(RichText::new(t("app-name")).size(22.0).strong());
                ui.label(
                    RichText::new(tf("editor-version", &[("version", env!("CARGO_PKG_VERSION"))]))
                        .color(self.pal.muted_text),
                );
                ui.add_space(6.0);
                ui.label(t("app-tagline"));
                ui.add_space(10.0);
                if ui.link(t("app-credit")).on_hover_text("https://zheer.dev").clicked() {
                    win::shell_open("https://zheer.dev");
                }
                ui.add_space(14.0);
                if ui.add(style::primary_button(t("editor-close")).min_size(vec2(96.0, 28.0))).clicked() {
                    close = true;
                }
            });
        });
        let enter = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
        (!(close || enter || resp.should_close())).then_some(Modal::About)
    }

    fn properties(&mut self, ctx: &egui::Context, id: DocId, props: Option<FileProps>) -> Option<Modal> {
        let d = self.tabs.docs.iter().find(|d| d.id == id)?;
        let (name, w, h, alpha) = (d.title.clone(), d.image.width, d.image.height, d.has_alpha);
        let mut close = false;
        let resp = egui::Modal::new(Id::new("properties")).show(ctx, |ui| {
            ui.set_width(460.0);
            title(ui, &tf("editor-properties-title", &[("name", &name)]));
            egui::Grid::new("props").num_columns(2).spacing(vec2(16.0, 6.0)).show(ui, |ui| {
                let p = props.as_ref();
                kv(ui, &t("editor-prop-dimensions"), &format!("{w} × {h} px  ({:.1} MP)", w as f64 * h as f64 / 1e6));
                kv(ui, &t("editor-prop-format"), p.map(|p| p.format.as_str()).filter(|s| !s.is_empty()).unwrap_or("—"));
                let size = p
                    .and_then(|p| p.size_bytes)
                    .map(|b| format!("{} ({b} bytes)", format_bytes(b)))
                    .unwrap_or_else(|| "—".into());
                kv(ui, &t("editor-prop-size"), &size);
                let mem = format_bytes(w as u64 * h as u64 * 4);
                kv(ui, &t("editor-prop-memory"), &mem);
                kv(ui, &t("editor-prop-alpha"), &if alpha { t("editor-yes") } else { t("editor-no") });
                let colors = match p.and_then(|p| p.colors) {
                    Some(c) => c.to_string(),
                    None => t("editor-counting"),
                };
                kv(ui, &t("editor-prop-colors"), &colors);
                kv(ui, &t("editor-prop-created"), p.and_then(|p| p.created.as_deref()).unwrap_or("—"));
                kv(ui, &t("editor-prop-modified"), p.and_then(|p| p.modified.as_deref()).unwrap_or("—"));
                let path = p
                    .and_then(|p| p.path.as_ref())
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| t("editor-unsaved"));
                kv(ui, &t("editor-prop-path"), &path);
            });
            ui.add_space(12.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(style::primary_button(t("editor-close")).min_size(vec2(88.0, 28.0))).clicked() {
                    close = true;
                }
                let has_path = props.as_ref().and_then(|p| p.path.clone());
                if let Some(p) = has_path {
                    if ui.button(t("editor-open-folder")).clicked() {
                        win::reveal_in_explorer(&p);
                    }
                }
            });
        });
        let enter = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
        (!(close || enter || resp.should_close())).then_some(Modal::Properties(id, props))
    }

    fn confirm_delete(&mut self, ctx: &egui::Context, id: DocId, path: PathBuf) -> Option<Modal> {
        let mut choice = Choice::None;
        let resp = egui::Modal::new(Id::new("confirm-delete")).show(ctx, |ui| {
            ui.set_width(420.0);
            title(ui, &t("editor-delete-title"));
            ui.label(tf("editor-delete-prompt", &[("name", &file_name(&path))]));
            ui.label(RichText::new(path.display().to_string()).small().weak());
            choice = buttons(ui, &t("editor-delete-confirm"), None, true);
        });
        match choice {
            Choice::Ok => {
                self.jobs.recycle(id, path);
                None
            }
            Choice::None if !resp.should_close() => Some(Modal::ConfirmDelete(id, path)),
            _ => None,
        }
    }

    fn sequence(&mut self, ctx: &egui::Context, mut f: SequenceForm) -> Option<Modal> {
        let docs: Vec<DocId> = self.tabs.visible().map(|d| d.id).collect();
        let folder = PathBuf::from(f.folder.trim());
        let names =
            plan_sequence(docs.len(), &f.base, f.start, f.digits, f.format.extension(), |n| folder.join(n).exists());
        let mut choice = Choice::None;
        let mut browse = false;
        let resp = egui::Modal::new(Id::new("save-sequence")).show(ctx, |ui| {
            ui.set_width(480.0);
            title(ui, &t("editor-save-sequence-title"));
            ui.label(RichText::new(tf("editor-save-sequence-hint", &[("count", &docs.len().to_string())])).weak());
            ui.add_space(6.0);
            egui::Grid::new("seq").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
                ui.label(t("editor-seq-folder"));
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut f.folder).desired_width(300.0));
                    browse = ui.button(t("editor-browse")).clicked();
                });
                ui.end_row();
                ui.label(t("editor-seq-base"));
                ui.add(egui::TextEdit::singleline(&mut f.base).desired_width(200.0));
                ui.end_row();
                ui.label(t("editor-seq-start"));
                ui.add(egui::DragValue::new(&mut f.start).range(0..=999_999));
                ui.end_row();
                ui.label(t("editor-seq-digits"));
                ui.add(egui::Slider::new(&mut f.digits, 1..=6));
                ui.end_row();
                ui.label(t("editor-seq-format"));
                egui::ComboBox::from_id_salt("seq-format")
                    .selected_text(f.format.extension().to_ascii_uppercase())
                    .show_ui(ui, |ui| {
                        for fmt in ImageFormat::ALL {
                            ui.selectable_value(&mut f.format, fmt, fmt.extension().to_ascii_uppercase());
                        }
                    });
                ui.end_row();
            });
            ui.add_space(6.0);
            let preview = match names.as_slice() {
                [] => String::new(),
                [one] => one.clone(),
                [first, .., last] => format!("{first}  …  {last}"),
            };
            ui.label(
                RichText::new(tf("editor-seq-preview", &[("names", &preview)])).small().color(self.pal.muted_text),
            );
            let ok = !docs.is_empty() && folder.is_dir() && !f.base.trim().is_empty();
            choice = buttons(ui, &t("editor-save-all"), None, ok);
        });
        if browse {
            if let Some(dir) = dialogs::pick_folder(self.hwnd, Some(&folder)) {
                f.folder = dir.to_string_lossy().into_owned();
            }
        }
        match choice {
            Choice::Ok => {
                for (id, name) in docs.iter().zip(&names) {
                    let path = folder.join(name);
                    if let Some(d) = self.tabs.get_mut(*id) {
                        if !d.saving {
                            d.saving = true;
                            let img = d.composited();
                            self.jobs.save(*id, img, path, d.revision, false);
                        }
                    }
                }
                remember_folder(&folder);
                let (fo, base, start, digits) = (f.folder.clone(), f.base.clone(), f.start, f.digits);
                settings::update(|s| {
                    s.editor_ext.sequence_folder = fo;
                    s.editor_ext.sequence_base = base;
                    s.editor_ext.sequence_start = start;
                    s.editor_ext.sequence_digits = digits;
                });
                None
            }
            Choice::None if !resp.should_close() => Some(Modal::Sequence(f)),
            _ => None,
        }
    }

    fn editors(&mut self, ctx: &egui::Context, mut list: Vec<ExternalEditor>) -> Option<Modal> {
        let mut choice = Choice::None;
        let mut browse: Option<usize> = None;
        let resp = egui::Modal::new(Id::new("editors")).show(ctx, |ui| {
            ui.set_width(560.0);
            title(ui, &t("editor-configure-editors"));
            ui.label(RichText::new(t("editor-editors-hint")).weak());
            ui.add_space(6.0);
            let mut remove = None;
            egui::Grid::new("editors-grid").num_columns(4).spacing(vec2(8.0, 6.0)).show(ui, |ui| {
                ui.label(RichText::new(t("editor-editor-name")).strong());
                ui.label(RichText::new(t("editor-editor-command")).strong());
                ui.label("");
                ui.label("");
                ui.end_row();
                for (i, e) in list.iter_mut().enumerate() {
                    ui.add(egui::TextEdit::singleline(&mut e.name).desired_width(120.0));
                    ui.add(
                        egui::TextEdit::singleline(&mut e.path).desired_width(280.0).hint_text("C:\\…\\app.exe \"%1\""),
                    );
                    if ui.button("…").on_hover_text(t("editor-browse")).clicked() {
                        browse = Some(i);
                    }
                    if ui.button("🗑").on_hover_text(t("editor-remove")).clicked() {
                        remove = Some(i);
                    }
                    ui.end_row();
                }
            });
            if let Some(i) = remove {
                list.remove(i);
            }
            if ui.button(format!("＋ {}", t("editor-add-editor"))).clicked() {
                list.push(ExternalEditor { name: String::new(), path: String::new() });
            }
            let ok = list.iter().all(|e| !e.name.trim().is_empty() && !e.path.trim().is_empty());
            choice = buttons(ui, &t("editor-save-settings"), None, ok);
        });
        if let Some(i) = browse {
            if let Some(p) = dialogs::pick_program(self.hwnd) {
                if let Some(e) = list.get_mut(i) {
                    if e.name.trim().is_empty() {
                        e.name = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    }
                    e.path = format!("\"{}\"", p.display());
                }
            }
        }
        match choice {
            Choice::Ok => {
                settings::update(|s| s.editor.external_editors = list);
                None
            }
            Choice::None if !resp.should_close() => Some(Modal::Editors(list)),
            _ => None,
        }
    }

    fn scan_options(&mut self, ctx: &egui::Context, mut s: ScanSettings) -> Option<Modal> {
        let mut choice = Choice::None;
        let resp = egui::Modal::new(Id::new("scan")).show(ctx, |ui| {
            ui.set_width(400.0);
            title(ui, &t("editor-scan-title"));
            ui.label(RichText::new(t("editor-scan-hint")).weak());
            ui.add_space(6.0);
            ui.checkbox(&mut s.deskew, t("editor-scan-deskew")).on_hover_text(t("editor-scan-deskew-tip"));
            ui.checkbox(&mut s.auto_crop, t("editor-scan-crop")).on_hover_text(t("editor-scan-crop-tip"));
            ui.checkbox(&mut s.auto_contrast, t("editor-scan-contrast")).on_hover_text(t("editor-scan-contrast-tip"));
            ui.separator();
            ui.checkbox(&mut s.batch, t("editor-scan-batch")).on_hover_text(t("editor-scan-batch-tip"));
            choice = buttons(ui, &t("editor-scan-start"), None, !self.scanning);
        });
        match choice {
            Choice::Ok => {
                let opts = PostOptions { deskew: s.deskew, auto_contrast: s.auto_contrast, auto_crop: s.auto_crop };
                let batch = s.batch;
                settings::update(|st| st.editor_ext.scan = s);
                self.scanning = true;
                self.jobs.scan(self.hwnd, opts, batch);
                None
            }
            Choice::None if !resp.should_close() => Some(Modal::Scan(s)),
            _ => None,
        }
    }

    fn no_scanner(&mut self, ctx: &egui::Context) -> Option<Modal> {
        let mut close = false;
        let resp = egui::Modal::new(Id::new("no-scanner")).show(ctx, |ui| {
            ui.set_width(380.0);
            title(ui, &format!("📠  {}", t("editor-no-scanner-title")));
            ui.label(t("editor-no-scanner"));
            ui.add_space(12.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                close = ui.add(style::primary_button(t("editor-close")).min_size(vec2(88.0, 28.0))).clicked();
            });
        });
        let enter = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
        (!(close || enter || resp.should_close())).then_some(Modal::NoScanner)
    }

    fn reload_prompt(&mut self, ctx: &egui::Context, doc: DocId, path: PathBuf, editor: String) -> Option<Modal> {
        let name = self.tabs.docs.iter().find(|d| d.id == doc).map(|d| d.title.clone())?;
        let mut choice = Choice::None;
        let resp = egui::Modal::new(Id::new("reload")).show(ctx, |ui| {
            ui.set_width(420.0);
            title(ui, &t("editor-reload-title"));
            ui.label(tf("editor-reload-prompt", &[("name", &name), ("editor", &editor)]));
            ui.label(RichText::new(t("editor-reload-undo")).small().weak());
            choice = buttons(ui, &t("editor-reload"), None, true);
        });
        match choice {
            Choice::Ok => {
                self.show_doc(doc);
                self.jobs.reload(doc, path);
                None
            }
            Choice::None if !resp.should_close() => Some(Modal::Reload { doc, path, editor }),
            _ => None,
        }
    }

    fn shortcuts_list(&mut self, ctx: &egui::Context) -> Option<Modal> {
        let mut close = false;
        let resp = egui::Modal::new(Id::new("shortcuts")).show(ctx, |ui| {
            ui.set_width(520.0);
            title(ui, &t("editor-shortcuts"));
            egui::ScrollArea::vertical().max_height(440.0).show(ui, |ui| {
                let mut cats: Vec<Category> =
                    self.registry.iter().filter(|c| c.shortcut.is_some()).map(|c| c.category).collect();
                cats.sort();
                cats.dedup();
                for cat in cats {
                    ui.add_space(4.0);
                    ui.label(RichText::new(t(cat.label_key())).strong().color(style::ACCENT));
                    egui::Grid::new(("sc", cat as u8)).num_columns(2).spacing(vec2(24.0, 4.0)).show(ui, |ui| {
                        for c in self.registry.iter().filter(|c| c.category == cat && c.shortcut.is_some()) {
                            ui.label(c.label.trim_end_matches('…'));
                            ui.label(RichText::new(c.shortcut_text()).monospace());
                            ui.end_row();
                        }
                    });
                }
                ui.add_space(4.0);
                ui.label(RichText::new(t("editor-shortcuts-mouse")).small().weak());
            });
            ui.add_space(10.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                close = ui.add(style::primary_button(t("editor-close")).min_size(vec2(88.0, 28.0))).clicked();
            });
        });
        let enter = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
        (!(close || enter || resp.should_close())).then_some(Modal::Shortcuts)
    }

    fn rename_workspace(&mut self, ctx: &egui::Context, id: u64, mut name: String) -> Option<Modal> {
        let mut choice = Choice::None;
        let resp = egui::Modal::new(Id::new("rename-ws")).show(ctx, |ui| {
            ui.set_width(340.0);
            title(ui, &t("editor-workspace-rename"));
            let r = ui.add(egui::TextEdit::singleline(&mut name).desired_width(f32::INFINITY));
            if !r.has_focus() && !r.lost_focus() {
                r.request_focus();
            }
            choice = buttons(ui, &t("editor-ok"), None, !name.trim().is_empty());
        });
        match choice {
            Choice::Ok => {
                let n = name.trim().to_string();
                settings::update(|s| {
                    if let Some(w) = s.editor_ext.workspaces.iter_mut().find(|w| w.id == id) {
                        w.name = n;
                    }
                });
                None
            }
            Choice::None if !resp.should_close() => Some(Modal::RenameWorkspace(id, name)),
            _ => None,
        }
    }
}
