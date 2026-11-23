//! One function per Settings page. Each page edits `Session::copy`.
//!
//! Adding a page: write `fn my_tab(ui: &mut Ui, s: &mut Session)`, route it in [`show`], and
//! add the variant in `model.rs` (label, reset). Sections without a page use [`generic_tab`].

use super::widgets::{self, card, form, hint, more, row, Palette};
use super::{dialogs, generic, hotkey_input, Session, Tab};
use crate::common::rng::Rng;
use crate::common::time::DateTime;
use crate::i18n::{self, t, tf};
use crate::settings::{
    filename, CaptureEngine, CaptureMode, Destination, ExternalEditor, FileNameSettings, ImageFormat, ScrollDirection,
    Size, Theme, UploadProtocol,
};
use eframe::egui::{self, RichText, Ui};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub fn show(ui: &mut Ui, s: &mut Session) {
    match s.tab {
        Tab::General => general(ui, s),
        Tab::Capture => capture(ui, s),
        Tab::Output => output(ui, s),
        Tab::Hotkeys => hotkeys(ui, s),
        Tab::FileName => file_name(ui, s),
        Tab::AutoSave => auto_save(ui, s),
        Tab::Editor => editor(ui, s),
        Tab::Toolbar => toolbar(ui, s),
        Tab::Recorder => recorder(ui, s),
        Tab::Draw => generic_tab(ui, s.palette, "draw", &mut s.copy.draw),
        Tab::Pin => generic_tab(ui, s.palette, "pin", &mut s.copy.pin),
        Tab::Ocr => generic_tab(ui, s.palette, "ocr", &mut s.copy.ocr),
        Tab::Tools => generic_tab(ui, s.palette, "tools", &mut s.copy.tools),
        Tab::Print => generic_tab(ui, s.palette, "print", &mut s.copy.print),
        Tab::History => generic_tab(ui, s.palette, "history", &mut s.copy.history),
        Tab::Language => language(ui, s),
        Tab::About => about(ui, s),
    }
}

// ---------------------------------------------------------------- helpers

/// Unit suffix for a number field (Fluent trims values, so the space is added here).
fn unit(key: &str) -> String {
    format!(" {}", t(key))
}

/// Small icon-only button with an accessible name and tooltip.
fn icon_button(ui: &mut Ui, icon: &str, label: &str) -> egui::Response {
    let r = ui.add(egui::Button::new(icon).min_size(egui::vec2(26.0, 24.0))).on_hover_text(label);
    let label = label.to_string();
    r.widget_info(move || egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label.clone()));
    r
}

fn combo<T: PartialEq + Copy>(
    ui: &mut Ui,
    id: &str,
    value: &mut T,
    items: &[T],
    label: impl Fn(T) -> String,
) -> egui::Response {
    let mut changed = false;
    let mut r = egui::ComboBox::from_id_salt(("settings-combo", id))
        .selected_text(label(*value))
        .show_ui(ui, |ui| {
            for &it in items {
                changed |= ui.selectable_value(value, it, label(it)).changed();
            }
        })
        .response;
    if changed {
        r.mark_changed();
    }
    r
}

fn pick_folder_into(s: &mut Session, target: &mut String) -> bool {
    let initial = PathBuf::from(filename::expand_env_tokens(target));
    match dialogs::pick_folder(&t("settings-pick-folder"), Some(&initial)) {
        Ok(Some(p)) => {
            *target = p.display().to_string();
            true
        }
        Ok(None) => false,
        Err(e) => {
            s.set_status(tf("settings-dialog-failed", &[("error", &format!("{e:#}"))]), true);
            false
        }
    }
}

fn pick_program(s: &mut Session) -> Option<String> {
    let filters = [
        (t("settings-filter-programs"), "*.exe;*.bat;*.cmd".to_string()),
        (t("settings-filter-all"), "*.*".to_string()),
    ];
    match dialogs::open_file(&t("settings-pick-program"), &filters, None) {
        Ok(p) => p.map(|p| p.display().to_string()),
        Err(e) => {
            s.set_status(tf("settings-dialog-failed", &[("error", &format!("{e:#}"))]), true);
            None
        }
    }
}

/// Most-recent-first folder history without duplicates, at most 10 entries.
pub fn push_history(history: &mut Vec<String>, folder: &str) {
    if folder.trim().is_empty() {
        return;
    }
    history.retain(|h| !h.eq_ignore_ascii_case(folder));
    history.insert(0, folder.to_string());
    history.truncate(10);
}

/// Windows key held (egui does not report it on Windows).
pub fn win_key_held() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LWIN, VK_RWIN};
    // SAFETY: plain key-state queries.
    unsafe { GetAsyncKeyState(VK_LWIN.0 as i32) < 0 || GetAsyncKeyState(VK_RWIN.0 as i32) < 0 }
}

/// PrtSc pressed since the last call (Windows sends no key-down message for it).
pub fn prtsc_pressed() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_SNAPSHOT};
    // SAFETY: plain key-state query.
    let v = unsafe { GetAsyncKeyState(VK_SNAPSHOT.0 as i32) };
    v < 0 || v & 1 != 0
}

// ---------------------------------------------------------------- General

fn general(ui: &mut Ui, s: &mut Session) {
    let p = s.palette;
    let g = &mut s.copy.general;
    card(ui, &p, &t("settings-startup"), |ui| {
        ui.checkbox(&mut g.start_with_windows, t("settings-start-with-windows"));
        ui.add_enabled_ui(g.start_with_windows, |ui| {
            ui.indent("startup-mode", |ui| {
                ui.radio_value(&mut g.start_minimized, false, t("settings-start-show-toolbar"));
                ui.radio_value(&mut g.start_minimized, true, t("settings-start-tray-only"));
            });
        });
    });
    ui.add_space(8.0);
    card(ui, &p, &t("settings-appearance"), |ui| {
        form(ui, "general", |ui| {
            row(ui, &t("settings-theme"), |ui| {
                combo(ui, "theme", &mut g.theme, &[Theme::System, Theme::Light, Theme::Dark], |v| {
                    t(match v {
                        Theme::System => "settings-theme-system",
                        Theme::Light => "settings-theme-light",
                        Theme::Dark => "settings-theme-dark",
                    })
                })
            });
        });
        hint(ui, &p, &t("settings-theme-hint"));
        ui.checkbox(&mut g.show_notifications, t("settings-notifications"));
    });
    more(ui, "general", |ui| {
        form(ui, "general-more", |ui| {
            row(ui, &t("settings-log-level"), |ui| {
                let levels = ["error", "warn", "info", "debug", "trace"];
                let mut idx = levels.iter().position(|l| *l == g.log_level).unwrap_or(2);
                let r = combo(ui, "log-level", &mut idx, &[0, 1, 2, 3, 4], |i| {
                    t(&format!("settings-log-{}", levels.get(i).copied().unwrap_or("info")))
                });
                if r.changed() {
                    g.log_level = levels.get(idx).copied().unwrap_or("info").to_string();
                }
                r
            });
        });
    });
}

// ---------------------------------------------------------------- Capture

fn capture(ui: &mut Ui, s: &mut Session) {
    let p = s.palette;
    let c = &mut s.copy.capture;
    card(ui, &p, &t("settings-capture-basics"), |ui| {
        ui.checkbox(&mut c.include_cursor, t("settings-include-cursor"));
        ui.checkbox(&mut c.fullscreen_all_monitors, t("settings-all-monitors"));
        ui.checkbox(&mut c.capture_on_release, t("settings-capture-on-release"));
        ui.checkbox(&mut c.show_magnifier, t("settings-magnifier"));
        ui.checkbox(&mut c.show_crosshair, t("settings-crosshair"));
        form(ui, "capture", |ui| {
            row(ui, &t("delay"), |ui| {
                ui.add(egui::DragValue::new(&mut c.delay_secs).range(0..=60).suffix(unit("settings-unit-seconds")))
            });
        });
    });
    ui.add_space(8.0);
    card(ui, &p, &t("settings-fixed-presets"), |ui| {
        hint(ui, &p, &t("settings-fixed-presets-hint"));
        let mut remove = None;
        let mut swap = None;
        let n = c.fixed_presets.len();
        egui::Grid::new("fixed-presets").num_columns(4).spacing([10.0, 6.0]).show(ui, |ui| {
            for (i, sz) in c.fixed_presets.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    let wl = ui.label(t("settings-width"));
                    ui.add(egui::DragValue::new(&mut sz.w).range(1..=16384).suffix(unit("settings-unit-px")))
                        .labelled_by(wl.id);
                    let hl = ui.label(t("settings-height"));
                    ui.add(egui::DragValue::new(&mut sz.h).range(1..=16384).suffix(unit("settings-unit-px")))
                        .labelled_by(hl.id);
                });
                ui.add_enabled_ui(i > 0, |ui| {
                    if icon_button(ui, "⬆", &t("settings-move-up")).clicked() {
                        swap = Some((i, i - 1));
                    }
                });
                ui.add_enabled_ui(i + 1 < n, |ui| {
                    if icon_button(ui, "⬇", &t("settings-move-down")).clicked() {
                        swap = Some((i, i + 1));
                    }
                });
                if icon_button(ui, "🗙", &t("settings-remove")).clicked() {
                    remove = Some(i);
                }
                ui.end_row();
            }
        });
        if let Some((a, b)) = swap {
            c.fixed_presets.swap(a, b);
        }
        if let Some(i) = remove {
            c.fixed_presets.remove(i);
        }
        c.fixed_index = c.fixed_index.min(c.fixed_presets.len().saturating_sub(1));
        if ui.button(t("settings-add-preset")).clicked() {
            c.fixed_presets.push(Size { w: 800, h: 600 });
        }
    });
    more(ui, "capture", |ui| {
        form(ui, "capture-more", |ui| {
            row(ui, &t("settings-engine"), |ui| {
                combo(
                    ui,
                    "engine",
                    &mut c.engine,
                    &[CaptureEngine::Auto, CaptureEngine::Wgc, CaptureEngine::Dxgi, CaptureEngine::Gdi],
                    |v| {
                        t(match v {
                            CaptureEngine::Auto => "settings-engine-auto",
                            CaptureEngine::Wgc => "settings-engine-wgc",
                            CaptureEngine::Dxgi => "settings-engine-dxgi",
                            CaptureEngine::Gdi => "settings-engine-gdi",
                        })
                    },
                )
            });
            row(ui, &t("settings-scroll-direction"), |ui| {
                combo(
                    ui,
                    "scroll-dir",
                    &mut c.scroll.direction,
                    &[ScrollDirection::Vertical, ScrollDirection::Horizontal, ScrollDirection::Both],
                    |v| {
                        t(match v {
                            ScrollDirection::Vertical => "settings-scroll-vertical",
                            ScrollDirection::Horizontal => "settings-scroll-horizontal",
                            ScrollDirection::Both => "settings-scroll-both",
                        })
                    },
                )
            });
            ui.checkbox(&mut c.scroll.start_at_top, t("settings-scroll-from-top"));
            row(ui, &t("settings-scroll-max"), |ui| {
                ui.add(egui::DragValue::new(&mut c.scroll.max_screens).range(1..=500))
            });
            row(ui, &t("settings-scroll-delay"), |ui| {
                ui.add(
                    egui::DragValue::new(&mut c.scroll.step_delay_ms).range(0..=5000).suffix(unit("settings-unit-ms")),
                )
            });
        });
    });
}

// ---------------------------------------------------------------- Output

fn format_label(f: ImageFormat) -> String {
    t(&format!("settings-format-{}", f.extension()))
}

fn output(ui: &mut Ui, s: &mut Session) {
    let p = s.palette;
    card(ui, &p, &t("output-to"), |ui| {
        let o = &mut s.copy.output;
        form(ui, "output", |ui| {
            row(ui, &t("settings-destination"), |ui| {
                combo(ui, "dest", &mut o.destination, &Destination::ALL, |d| t(d.label_key()))
            });
        });
        ui.checkbox(&mut o.preview_in_editor, t("opt-preview"));
        ui.checkbox(&mut o.auto_copy, t("opt-autocopy"));
        ui.checkbox(&mut o.auto_save, t("opt-autosave")).on_hover_text(t("opt-autosave-tip"));
        ui.checkbox(&mut o.timestamp, t("opt-timestamp"));
    });
    ui.add_space(8.0);
    card(ui, &p, &t("settings-save-files"), |ui| {
        let mut browse = false;
        let o = &mut s.copy.output;
        form(ui, "output-files", |ui| {
            row(ui, &t("settings-folder"), |ui| {
                let r = ui.add(
                    egui::TextEdit::singleline(&mut o.folder)
                        .hint_text(t("settings-folder-default"))
                        .desired_width(300.0),
                );
                browse = ui.button(t("settings-browse")).clicked();
                r
            });
            if !o.folder_history.is_empty() {
                row(ui, &t("settings-recent-folders"), |ui| {
                    let mut chosen = None;
                    let r = egui::ComboBox::from_id_salt("folder-history")
                        .selected_text(t("settings-choose"))
                        .width(300.0)
                        .show_ui(ui, |ui| {
                            for h in &o.folder_history {
                                if ui.selectable_label(false, h).clicked() {
                                    chosen = Some(h.clone());
                                }
                            }
                        })
                        .response;
                    if let Some(h) = chosen {
                        o.folder = h;
                    }
                    r
                });
            }
            row(ui, &t("settings-format"), |ui| combo(ui, "format", &mut o.format, &ImageFormat::ALL, format_label));
            match o.format {
                ImageFormat::Jpeg => {
                    row(ui, &t("settings-quality"), |ui| ui.add(egui::Slider::new(&mut o.jpeg_quality, 1..=100)));
                }
                ImageFormat::Webp => {
                    row(ui, &t("settings-quality"), |ui| ui.add(egui::Slider::new(&mut o.webp_quality, 1..=100)));
                }
                _ => {}
            }
        });
        if browse {
            let mut folder = s.copy.output.folder.clone();
            if folder.trim().is_empty() {
                folder = s.copy.output_folder().display().to_string();
            }
            if pick_folder_into(s, &mut folder) {
                push_history(&mut s.copy.output.folder_history, &folder);
                s.copy.output.folder = folder;
            }
        }
        hint(ui, &p, &tf("settings-folder-resolved", &[("path", &s.copy.output_folder().display().to_string())]));
    });
    more(ui, "output", |ui| {
        let mut browse_prog = false;
        {
            let o = &mut s.copy.output;
            form(ui, "output-more", |ui| {
                row(ui, &t("settings-timestamp-format"), |ui| {
                    ui.add(egui::TextEdit::singleline(&mut o.timestamp_format).desired_width(300.0))
                });
                row(ui, &t("dest-program"), |ui| {
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut o.external_program)
                            .hint_text(t("settings-program-hint"))
                            .desired_width(300.0),
                    );
                    browse_prog = ui.button(t("settings-browse")).clicked();
                    r
                });
            });
        }
        if browse_prog {
            if let Some(prog) = pick_program(s) {
                s.copy.output.external_program = format!("\"{prog}\" \"%1\"");
            }
        }
        hint(ui, &p, &t("settings-program-explain"));
        ui.add_space(6.0);
        upload(ui, s);
    });
}

fn upload(ui: &mut Ui, s: &mut Session) {
    let p = s.palette;
    let mut browse_key = false;
    let mut pw_changed = false;
    let mut pw_clear = false;
    card(ui, &p, &t("dest-upload"), |ui| {
        let u = &mut s.copy.output.upload;
        let stored = !u.password_protected.is_empty();
        form(ui, "upload", |ui| {
            row(ui, &t("settings-upload-protocol"), |ui| {
                combo(ui, "protocol", &mut u.protocol, &[UploadProtocol::Ftp, UploadProtocol::Sftp], |v| {
                    t(match v {
                        UploadProtocol::Ftp => "settings-upload-ftp",
                        UploadProtocol::Sftp => "settings-upload-sftp",
                    })
                })
            });
            row(ui, &t("settings-upload-host"), |ui| {
                ui.add(egui::TextEdit::singleline(&mut u.host).desired_width(260.0))
            });
            row(ui, &t("settings-upload-port"), |ui| {
                let r = ui.add(egui::DragValue::new(&mut u.port).range(0..=65535));
                hint(ui, &p, &t("settings-upload-port-hint"));
                r
            });
            row(ui, &t("settings-upload-user"), |ui| {
                ui.add(egui::TextEdit::singleline(&mut u.username).desired_width(260.0))
            });
            match u.protocol {
                UploadProtocol::Ftp => {
                    row(ui, &t("settings-upload-password"), |ui| {
                        let hint_text = if stored { t("settings-password-saved") } else { String::new() };
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut s.password)
                                .password(true)
                                .hint_text(hint_text)
                                .desired_width(260.0),
                        );
                        pw_changed = r.changed();
                        if stored && ui.button(t("settings-password-clear")).clicked() {
                            pw_clear = true;
                        }
                        r
                    });
                }
                UploadProtocol::Sftp => {
                    row(ui, &t("settings-upload-key"), |ui| {
                        let r = ui.add(egui::TextEdit::singleline(&mut u.sftp_key_path).desired_width(260.0));
                        browse_key = ui.button(t("settings-browse")).clicked();
                        r
                    });
                }
            }
            row(ui, &t("settings-upload-dir"), |ui| {
                ui.add(egui::TextEdit::singleline(&mut u.remote_dir).desired_width(260.0))
            });
            row(ui, &t("settings-upload-url"), |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut u.url_prefix)
                        .hint_text(t("settings-upload-url-hint"))
                        .desired_width(260.0),
                )
            });
        });
        if u.protocol == UploadProtocol::Ftp {
            ui.checkbox(&mut u.passive, t("settings-upload-passive"));
        } else {
            hint(ui, &p, &t("settings-upload-sftp-hint"));
        }
        ui.checkbox(&mut u.keep_local_copy, t("settings-upload-keep-local"));
    });
    if pw_clear {
        s.password.clear();
        s.copy.output.upload.password_protected.clear();
    } else if pw_changed {
        let enc = crate::settings::secret::protect(&s.password);
        if enc.is_empty() && !s.password.is_empty() {
            s.set_status(t("settings-password-protect-failed"), true);
        }
        s.copy.output.upload.password_protected = enc;
    }
    if browse_key {
        let filters = [(t("settings-filter-all"), "*.*".to_string())];
        match dialogs::open_file(&t("settings-upload-key"), &filters, None) {
            Ok(Some(path)) => s.copy.output.upload.sftp_key_path = path.display().to_string(),
            Ok(None) => {}
            Err(e) => s.set_status(tf("settings-dialog-failed", &[("error", &format!("{e:#}"))]), true),
        }
    }
}

// ---------------------------------------------------------------- Hotkeys

fn hotkeys(ui: &mut Ui, s: &mut Session) {
    let p = s.palette;
    card(ui, &p, &t("settings-hotkeys-assign"), |ui| {
        hint(ui, &p, &t("settings-hotkeys-hint"));
        egui::Grid::new("hotkey-rows").num_columns(4).spacing([12.0, 8.0]).min_col_width(24.0).show(ui, |ui| {
            for i in 0..hotkey_input::ROWS {
                let label = t(hotkey_input::row_label_key(i));
                let l = ui.label(&label);
                let capturing = s.capturing == Some(i);
                let text = hotkey_input::row_text(&s.copy.hotkeys, i).to_string();
                let r = widgets::hotkey_button(ui, &p, &text, capturing, &label).labelled_by(l.id);
                if r.clicked() {
                    if capturing {
                        s.capturing = None;
                    } else {
                        let _ = prtsc_pressed(); // clear the "pressed since last call" bit
                        s.capturing = Some(i);
                    }
                }
                ui.horizontal(|ui| {
                    if capturing {
                        if ui
                            .button(t("settings-hotkey-use-prtsc"))
                            .on_hover_text(t("settings-hotkey-use-prtsc-tip"))
                            .clicked()
                        {
                            let mods = ui.input(|i| i.modifiers);
                            *hotkey_input::row_text_mut(&mut s.copy.hotkeys, i) =
                                hotkey_input::combo_text_vk(mods, false, 0x2C);
                            s.capturing = None;
                        }
                    } else if !text.is_empty()
                        && icon_button(ui, "🗙", &tf("settings-hotkey-clear", &[("action", &label)])).clicked()
                    {
                        hotkey_input::row_text_mut(&mut s.copy.hotkeys, i).clear();
                    }
                });
                if hotkey_input::row_has_warning(&s.copy.hotkeys, i) {
                    ui.label(RichText::new("⚠").color(p.warn)).on_hover_text(t("settings-hotkey-row-warning"));
                } else {
                    ui.label("");
                }
                ui.end_row();
            }
        });
        ui.add_space(4.0);
        ui.checkbox(&mut s.copy.hotkeys.prtsc_override, t("settings-prtsc-override"));
        hint(ui, &p, &t("settings-prtsc-override-hint"));
    });
    let warnings = hotkey_input::warnings(&s.copy.hotkeys);
    ui.add_space(8.0);
    card(ui, &p, &t("settings-hotkey-check"), |ui| {
        if warnings.is_empty() {
            ui.label(RichText::new(t("settings-hotkey-no-conflicts")).color(p.ok));
        }
        for w in &warnings {
            ui.label(RichText::new(format!("⚠  {w}")).color(p.warn));
        }
    });
    more(ui, "hotkeys", |ui| {
        hint(ui, &p, &t("settings-hotkeys-text-hint"));
        form(ui, "hotkeys-text", |ui| {
            for i in 0..hotkey_input::ROWS {
                let label = t(hotkey_input::row_label_key(i));
                row(ui, &label, |ui| {
                    ui.add(
                        egui::TextEdit::singleline(hotkey_input::row_text_mut(&mut s.copy.hotkeys, i))
                            .desired_width(220.0),
                    )
                });
            }
        });
    });
}

// ---------------------------------------------------------------- File name

/// File-name tokens offered as buttons: (token, Fluent key of its description).
pub const TOKENS: &[(&str, &str)] = &[
    ("$Y", "settings-token-year"),
    ("$M", "settings-token-month"),
    ("$D", "settings-token-day"),
    ("$H", "settings-token-hour"),
    ("$N", "settings-token-minute"),
    ("$S", "settings-token-second"),
    ("###", "settings-token-counter"),
    ("@", "settings-token-random"),
    ("%USERNAME%", "settings-token-user"),
    ("%COMPUTERNAME%", "settings-token-computer"),
    ("%DESKTOP%", "settings-token-desktop"),
    ("%PICTURES%", "settings-token-pictures"),
    ("%DOCUMENTS%", "settings-token-documents"),
];

/// The path the next capture would get (no existence check; counter state not advanced).
pub fn preview_path(
    fs: &FileNameSettings,
    folder: &Path,
    ext: &str,
    now: &DateTime,
    seed: u64,
    env: &dyn Fn(&str) -> Option<String>,
) -> PathBuf {
    let mut rng = Rng::new(seed);
    filename::build_path(fs, folder, ext, now, &mut rng, env, &|_| false).0
}

/// Insert `token` at the text cursor of the TextEdit `id` (or at the end).
fn insert_at_cursor(ctx: &egui::Context, id: egui::Id, text: &mut String, token: &str) {
    let state = egui::TextEdit::load_state(ctx, id);
    let char_idx = state
        .as_ref()
        .and_then(|st| st.cursor.char_range())
        .map(|r| r.primary.index)
        .unwrap_or_else(|| text.chars().count());
    let byte = text.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(text.len());
    text.insert_str(byte, token);
    if let Some(mut st) = state {
        let c = egui::text::CCursor::new(char_idx + token.chars().count());
        st.cursor.set_char_range(Some(egui::text::CCursorRange::one(c)));
        st.store(ctx, id);
    }
}

fn file_name(ui: &mut Ui, s: &mut Session) {
    let p = s.palette;
    let edit_id = egui::Id::new("settings-filename-template");
    card(ui, &p, &t("settings-filename-template"), |ui| {
        let f = &mut s.copy.filename;
        let l = ui.label(t("settings-template"));
        ui.add(
            egui::TextEdit::singleline(&mut f.template)
                .id(edit_id)
                .font(egui::TextStyle::Monospace)
                .desired_width(f32::INFINITY),
        )
        .labelled_by(l.id);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
            for (tok, key) in TOKENS {
                let desc = t(key);
                let r = ui.add(egui::Button::new(RichText::new(*tok).monospace()).small()).on_hover_text(&desc);
                let name = tf("settings-token-insert", &[("token", tok), ("what", &desc)]);
                r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name.clone()));
                if r.clicked() {
                    insert_at_cursor(ui.ctx(), edit_id, &mut f.template, tok);
                }
            }
        });
        ui.add_space(4.0);
        let folder = s.copy.output_folder();
        let ext = s.copy.output.format.extension();
        let path = preview_path(
            &s.copy.filename,
            &folder,
            ext,
            &DateTime::now_local(),
            s.preview_seed,
            &filename::default_env,
        );
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(t("settings-preview")).strong());
            ui.label(RichText::new(path.display().to_string()).monospace().color(p.accent_text));
        });
    });
    ui.add_space(8.0);
    card(ui, &p, &t("settings-counter"), |ui| {
        let f = &mut s.copy.filename;
        form(ui, "counter", |ui| {
            row(ui, &t("settings-counter-next"), |ui| ui.add(egui::DragValue::new(&mut f.counter).range(0..=u32::MAX)));
        });
        ui.checkbox(&mut f.reset_daily, t("settings-counter-daily"));
        hint(ui, &p, &t("settings-counter-hint"));
    });
}

// ---------------------------------------------------------------- Auto save (auto capture)

fn auto_save(ui: &mut Ui, s: &mut Session) {
    let p = s.palette;
    let mut browse = false;
    card(ui, &p, &t("auto-capture"), |ui| {
        let c = &mut s.copy.capture;
        hint(ui, &p, &t("settings-autosave-hint"));
        ui.checkbox(&mut c.auto_capture_enabled, t("settings-autosave-enabled"));
        form(ui, "autosave", |ui| {
            row(ui, &t("settings-autosave-interval"), |ui| {
                ui.add(
                    egui::DragValue::new(&mut c.auto_capture_interval_secs)
                        .range(1..=86_400)
                        .suffix(unit("settings-unit-seconds")),
                )
            });
            row(ui, &t("settings-autosave-mode"), |ui| {
                combo(ui, "autosave-mode", &mut c.auto_capture_mode, &CaptureMode::ALL, |m| t(m.label_key()))
            });
            row(ui, &t("settings-folder"), |ui| {
                let r = ui.add(
                    egui::TextEdit::singleline(&mut c.auto_capture_folder)
                        .hint_text(t("settings-autosave-folder-default"))
                        .desired_width(300.0),
                );
                browse = ui.button(t("settings-browse")).clicked();
                r
            });
        });
    });
    if browse {
        let mut folder = s.copy.auto_capture_folder().display().to_string();
        if pick_folder_into(s, &mut folder) {
            s.copy.capture.auto_capture_folder = folder;
        }
    }
    hint(ui, &p, &tf("settings-folder-resolved", &[("path", &s.copy.auto_capture_folder().display().to_string())]));
}

// ---------------------------------------------------------------- Editor

fn editor(ui: &mut Ui, s: &mut Session) {
    let p = s.palette;
    let mut browse_row = None;
    let mut remove = None;
    card(ui, &p, &t("settings-external-editors"), |ui| {
        hint(ui, &p, &t("settings-external-editors-hint"));
        let list = &mut s.copy.editor.external_editors;
        egui::Grid::new("ext-editors").num_columns(4).spacing([10.0, 6.0]).show(ui, |ui| {
            ui.label(RichText::new(t("settings-editor-name")).strong());
            ui.label(RichText::new(t("settings-editor-path")).strong());
            ui.end_row();
            for (i, e) in list.iter_mut().enumerate() {
                ui.add(egui::TextEdit::singleline(&mut e.name).desired_width(150.0)).widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, t("settings-editor-name"))
                });
                ui.add(egui::TextEdit::singleline(&mut e.path).desired_width(280.0)).widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, t("settings-editor-path"))
                });
                if ui.button(t("settings-browse")).clicked() {
                    browse_row = Some(i);
                }
                if icon_button(ui, "🗙", &t("settings-remove")).clicked() {
                    remove = Some(i);
                }
                ui.end_row();
            }
        });
        if list.is_empty() {
            hint(ui, &p, &t("settings-none"));
        }
        if ui.button(t("settings-editor-add")).clicked() {
            list.push(ExternalEditor { name: t("settings-editor-new"), path: String::new() });
        }
    });
    if let Some(i) = remove {
        s.copy.editor.external_editors.remove(i);
    }
    if let Some(i) = browse_row {
        if let Some(prog) = pick_program(s) {
            if let Some(e) = s.copy.editor.external_editors.get_mut(i) {
                if e.name.trim().is_empty() || e.name == t("settings-editor-new") {
                    if let Some(stem) = Path::new(&prog).file_stem() {
                        e.name = stem.to_string_lossy().into_owned();
                    }
                }
                e.path = prog;
            }
        }
    }
}

// ---------------------------------------------------------------- Toolbar

/// Every toolbar button id: capture modes, then output, delay and menu.
pub fn toolbar_button_ids() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = CaptureMode::ALL.iter().map(|m| m.id()).collect();
    v.extend(["output", "delay", "menu"]);
    v
}

fn toolbar_label(id: &str) -> String {
    match CaptureMode::from_id(id) {
        Some(m) => t(m.label_key()),
        None => t(match id {
            "output" => "output-to",
            "delay" => "delay",
            "menu" => "toolbar-menu",
            _ => return id.to_string(),
        }),
    }
}

fn toolbar(ui: &mut Ui, s: &mut Session) {
    let p = s.palette;
    let tb = &mut s.copy.toolbar;
    card(ui, &p, &t("settings-toolbar-behaviour"), |ui| {
        ui.checkbox(&mut tb.visible, t("tray-show-toolbar"));
        ui.checkbox(&mut tb.big_buttons, t("toolbar-big-buttons"));
        ui.checkbox(&mut tb.locked, t("toolbar-lock"));
        let mut always = !tb.auto_hide;
        if ui
            .checkbox(&mut always, t("toolbar-always-visible"))
            .on_hover_text(t("toolbar-always-visible-tip"))
            .changed()
        {
            tb.auto_hide = !always;
        }
    });
    ui.add_space(8.0);
    card(ui, &p, &t("toolbar-customize"), |ui| {
        hint(ui, &p, &t("settings-toolbar-buttons-hint"));
        let mut swap = None;
        let mut toggle: Option<(String, bool)> = None;
        let n = tb.buttons.len();
        egui::Grid::new("toolbar-buttons").num_columns(3).spacing([10.0, 4.0]).show(ui, |ui| {
            for (i, id) in tb.buttons.iter().enumerate() {
                let mut on = true;
                if ui.checkbox(&mut on, toolbar_label(id)).changed() {
                    toggle = Some((id.clone(), false));
                }
                ui.add_enabled_ui(i > 0, |ui| {
                    if icon_button(ui, "⬆", &tf("settings-move-up-named", &[("name", &toolbar_label(id))])).clicked()
                    {
                        swap = Some((i, i - 1));
                    }
                });
                ui.add_enabled_ui(i + 1 < n, |ui| {
                    if icon_button(ui, "⬇", &tf("settings-move-down-named", &[("name", &toolbar_label(id))])).clicked()
                    {
                        swap = Some((i, i + 1));
                    }
                });
                ui.end_row();
            }
            for id in toolbar_button_ids() {
                if tb.buttons.iter().any(|b| b == id) {
                    continue;
                }
                let mut on = false;
                if ui.checkbox(&mut on, toolbar_label(id)).changed() {
                    toggle = Some((id.to_string(), true));
                }
                ui.end_row();
            }
        });
        if let Some((a, b)) = swap {
            tb.buttons.swap(a, b);
        }
        match toggle {
            Some((id, true)) => tb.buttons.push(id),
            Some((id, false)) => tb.buttons.retain(|b| *b != id),
            None => {}
        }
    });
}

// ---------------------------------------------------------------- Recorder

/// Recorder sub-tabs: (Fluent key, field-name prefixes). Fields that match no prefix are
/// shown on the first sub-tab. A dedicated sub-page can replace the generic one in
/// [`recorder_sub_page`].
pub const RECORDER_SUBTABS: &[(&str, &[&str])] = &[
    ("settings-rec-video", &["video", "fps", "codec", "bitrate", "quality", "region", "cursor", "click"]),
    ("settings-rec-audio", &["audio", "mic", "speaker", "system_audio", "loopback"]),
    ("settings-rec-webcam", &["webcam", "camera"]),
    ("settings-rec-notifications", &["notif", "notify", "countdown", "sound", "toast"]),
    ("settings-rec-hotkeys", &["hotkey", "key_"]),
];

/// Which recorder sub-tab shows a field.
pub fn recorder_sub_for(field: &str) -> usize {
    let f = field.to_ascii_lowercase();
    RECORDER_SUBTABS.iter().position(|(_, prefixes)| prefixes.iter().any(|p| f.starts_with(p))).unwrap_or(0)
}

fn recorder(ui: &mut Ui, s: &mut Session) {
    let p = s.palette;
    ui.horizontal(|ui| {
        for (i, (key, _)) in RECORDER_SUBTABS.iter().enumerate() {
            ui.selectable_value(&mut s.recorder_sub, i, t(key));
        }
    });
    ui.add_space(6.0);
    let sub = s.recorder_sub.min(RECORDER_SUBTABS.len() - 1);
    card(ui, &p, &t(RECORDER_SUBTABS[sub].0), |ui| recorder_sub_page(ui, s, sub));
}

/// Extension point for the recorder agent: return a dedicated page per sub-tab here.
fn recorder_sub_page(ui: &mut Ui, s: &mut Session, sub: usize) {
    let p = s.palette;
    let names = generic::field_names(&s.copy.recorder);
    if !names.iter().any(|n| recorder_sub_for(n) == sub) {
        hint(ui, &p, &t("settings-no-options-yet"));
        return;
    }
    if sub == 1 || sub == 2 {
        device_picker(ui, s, sub == 2);
        ui.add_space(6.0);
    }
    generic::section(ui, "recorder", &mut s.copy.recorder, &|k| recorder_sub_for(k) == sub && k != "mic_device_id");
}

/// Microphone (audio sub-tab) or camera (webcam sub-tab) by name; the device lists are read
/// once per Settings session.
fn device_picker(ui: &mut Ui, s: &mut Session, camera: bool) {
    let devices =
        s.devices.get_or_insert_with(|| (crate::recorder::list_audio_inputs(), crate::recorder::list_cameras()));
    let list: Vec<(String, String)> = if camera {
        devices.1.iter().map(|c| (c.id.clone(), c.name.clone())).collect()
    } else {
        devices.0.iter().map(|d| (d.id.clone(), d.name.clone())).collect()
    };
    let (label, default) = if camera {
        (t("settings-rec-camera-device"), t("settings-rec-camera-default"))
    } else {
        (t("settings-rec-mic-device"), t("settings-rec-mic-default"))
    };
    let current = if camera { &mut s.copy.recorder.webcam.device_id } else { &mut s.copy.recorder.mic_device_id };
    let shown = list.iter().find(|(id, _)| id == current).map(|(_, n)| n.clone()).unwrap_or_else(|| {
        if current.is_empty() {
            default.clone()
        } else {
            t("settings-rec-device-missing")
        }
    });
    form(ui, if camera { "rec-camera" } else { "rec-mic" }, |ui| {
        row(ui, &label, |ui| {
            egui::ComboBox::from_id_salt(("rec-device", camera))
                .selected_text(shown)
                .width(320.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(current, String::new(), default.clone());
                    for (id, name) in &list {
                        ui.selectable_value(current, id.clone(), name);
                    }
                })
                .response
        });
    });
}

/// A settings section edited generically (until it gets a dedicated page).
fn generic_tab<T: serde::Serialize + serde::de::DeserializeOwned>(ui: &mut Ui, p: Palette, name: &str, v: &mut T) {
    card(ui, &p, "", |ui| {
        if generic::field_names(v).is_empty() {
            hint(ui, &p, &t("settings-no-options-yet"));
        } else {
            generic::section(ui, name, v, &|_| true);
        }
    });
}

// ---------------------------------------------------------------- Language

#[derive(Debug, Clone, PartialEq)]
pub struct Locale {
    pub code: String,
    pub name: String,
}

/// en-US (built in) plus every `<exe dir>\locales\<code>\main.ftl`.
pub fn available_locales() -> Vec<Locale> {
    let mut v = vec![Locale { code: "en-US".into(), name: t("language-name") }];
    let dir = crate::common::win::exe_dir().join("locales");
    v.extend(scan_locales(&dir));
    v
}

pub fn scan_locales(dir: &Path) -> Vec<Locale> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        let code = e.file_name().to_string_lossy().into_owned();
        if code == "en-US" {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(e.path().join("main.ftl")) else { continue };
        let name = i18n::parse_ftl(&text).remove("language-name").unwrap_or_else(|| code.clone());
        out.push(Locale { code, name });
    }
    out.sort_by(|a, b| a.code.cmp(&b.code));
    out
}

fn language(ui: &mut Ui, s: &mut Session) {
    let p = s.palette;
    card(ui, &p, &t("settings-language-choose"), |ui| {
        let lang = &mut s.copy.general.language;
        if !s.locales.iter().any(|l| l.code == *lang) {
            ui.radio_value(lang, lang.clone(), tf("settings-language-missing", &[("code", lang)]));
        }
        for l in &s.locales {
            ui.radio_value(lang, l.code.clone(), format!("{}  ({})", l.name, l.code));
        }
        ui.add_space(4.0);
        hint(ui, &p, &t("settings-language-restart"));
        hint(ui, &p, &t("settings-language-add-hint"));
    });
}

// ---------------------------------------------------------------- About

pub struct IconRgba {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// The app icon (largest entry of the build-generated .ico).
pub fn app_icon() -> Option<&'static IconRgba> {
    static ICON: OnceLock<Option<IconRgba>> = OnceLock::new();
    ICON.get_or_init(|| {
        let bytes: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/snapforge.ico"));
        let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Ico).ok()?.to_rgba8();
        let (width, height) = img.dimensions();
        Some(IconRgba { rgba: img.into_raw(), width, height })
    })
    .as_ref()
}

pub fn icon_texture(ctx: &egui::Context, slot: &mut Option<egui::TextureHandle>) -> Option<egui::TextureHandle> {
    if slot.is_none() {
        let icon = app_icon()?;
        let img = egui::ColorImage::from_rgba_unmultiplied([icon.width as usize, icon.height as usize], &icon.rgba);
        *slot = Some(ctx.load_texture("snapforge-settings-icon", img, egui::TextureOptions::LINEAR));
    }
    slot.clone()
}

pub const WEBSITE: &str = "https://zheer.dev";
pub const REPO: &str = "https://github.com/zheerkhalid/snapforge";

fn about(ui: &mut Ui, s: &mut Session) {
    let p = s.palette;
    card(ui, &p, "", |ui| {
        ui.horizontal(|ui| {
            if let Some(tex) = icon_texture(ui.ctx(), &mut s.icon) {
                ui.add(egui::Image::new(&tex).fit_to_exact_size(egui::vec2(96.0, 96.0)))
                    .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Image, true, t("settings-about-icon")));
            }
            ui.add_space(12.0);
            ui.vertical(|ui| {
                ui.add_space(8.0);
                ui.label(RichText::new(t("app-name")).size(26.0).strong());
                ui.label(tf("settings-about-version", &[("version", env!("CARGO_PKG_VERSION"))]));
                ui.label(RichText::new(t("app-tagline")).color(p.weak));
            });
        });
        ui.add_space(10.0);
        ui.label(t("settings-about-developer"));
        ui.horizontal(|ui| {
            widgets::link(ui, &t("settings-about-website"), WEBSITE);
            ui.label("·");
            widgets::link(ui, &t("settings-about-github"), REPO);
        });
        ui.add_space(6.0);
        hint(ui, &p, &t("settings-about-license"));
    });
    ui.add_space(8.0);
    card(ui, &p, &t("settings-about-files"), |ui| {
        let dir = crate::settings::dir();
        let mode =
            if crate::settings::is_portable() { t("settings-about-portable") } else { t("settings-about-installed") };
        ui.label(mode);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(dir.display().to_string()).monospace());
            if ui.button(t("settings-about-open-folder")).clicked() {
                crate::common::win::shell_open(&dir.display().to_string());
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(n: &str) -> Option<String> {
        match n {
            "USERNAME" => Some("alice".into()),
            _ => None,
        }
    }

    #[test]
    fn template_preview() {
        let now = DateTime { year: 2026, month: 11, day: 14, hour: 8, minute: 3, second: 9, millis: 0 };
        let fs = FileNameSettings {
            template: "%USERNAME%_$Y-$M-$D_$H$N$S_###".into(),
            counter: 7,
            reset_daily: true,
            last_date: "2026-11-14".into(),
        };
        let p = preview_path(&fs, Path::new("C:\\out"), "png", &now, 1, &env);
        assert_eq!(p, PathBuf::from("C:\\out\\alice_2026-11-14_080309_007.png"));
        // Daily reset is reflected in the preview.
        let old = FileNameSettings { last_date: "2026-11-13".into(), ..fs.clone() };
        let p = preview_path(&old, Path::new("C:\\out"), "jpg", &now, 1, &env);
        assert_eq!(p, PathBuf::from("C:\\out\\alice_2026-11-14_080309_001.jpg"));
        // `@` is stable for one seed (no flicker while typing).
        let r = FileNameSettings { template: "x@@@".into(), ..fs };
        assert_eq!(
            preview_path(&r, Path::new("C:\\o"), "png", &now, 5, &env),
            preview_path(&r, Path::new("C:\\o"), "png", &now, 5, &env)
        );
    }

    #[test]
    fn history_is_mru() {
        let mut h = vec!["C:\\a".to_string(), "C:\\b".to_string()];
        push_history(&mut h, "c:\\B");
        assert_eq!(h, vec!["c:\\B".to_string(), "C:\\a".to_string()]);
        for i in 0..20 {
            push_history(&mut h, &format!("D:\\{i}"));
        }
        assert_eq!(h.len(), 10);
        push_history(&mut h, "  ");
        assert_eq!(h.len(), 10);
    }

    #[test]
    fn recorder_subtab_routing() {
        assert_eq!(recorder_sub_for("video_fps"), 0);
        assert_eq!(recorder_sub_for("mic_device"), 1);
        assert_eq!(recorder_sub_for("webcam_size"), 2);
        assert_eq!(recorder_sub_for("countdown_secs"), 3);
        assert_eq!(recorder_sub_for("hotkey_stop"), 4);
        assert_eq!(recorder_sub_for("something_else"), 0);
    }

    #[test]
    fn toolbar_ids_match_defaults() {
        let d = crate::settings::ToolbarSettings::default();
        assert_eq!(d.buttons, toolbar_button_ids().into_iter().map(String::from).collect::<Vec<_>>());
        for id in toolbar_button_ids() {
            assert_ne!(toolbar_label(id), id);
        }
    }

    #[test]
    fn icon_decodes() {
        let i = app_icon().expect("icon");
        assert!(i.width >= 64 && i.rgba.len() == (i.width * i.height * 4) as usize);
    }

    #[test]
    fn locales_scan() {
        let dir = std::env::temp_dir().join(format!("snapforge-locales-{}", std::process::id()));
        let de = dir.join("de-DE");
        let _ = std::fs::create_dir_all(&de);
        let _ = std::fs::create_dir_all(dir.join("empty"));
        std::fs::write(de.join("main.ftl"), "language-name = Deutsch\n").expect("write");
        let l = scan_locales(&dir);
        assert_eq!(l, vec![Locale { code: "de-DE".into(), name: "Deutsch".into() }]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
