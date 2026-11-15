//! The Settings window (egui), shown as its own native viewport of the editor's eframe app.
//!
//! Hosting:
//! ```ignore
//! let settings = settings_ui::SettingsWindow::new();   // keep it in the eframe app
//! settings.open();                                     // on AppCommand::OpenSettings
//! settings.ui(ctx);                                    // every frame of the host app
//! ```
//! The window edits a working copy of the settings. Apply / OK merge the edits into the live
//! settings (`settings::replace`) and post `AppCommand::SettingsChanged`. The window size,
//! position and last tab are remembered in `ui_state.toml`.
//!
//! It is a *deferred* viewport: once open it repaints on its own, even while the host's root
//! window is hidden. The host must keep calling [`SettingsWindow::ui`] on its frames so egui
//! keeps the viewport alive (and drops it after closing).

mod dialogs;
mod generic;
mod hotkey_input;
mod model;
mod tabs;
mod ui_state;
mod widgets;

pub use model::Tab;

use crate::app::{self, AppCommand};
use crate::i18n::{t, tf};
use crate::settings::{self, Settings};
use eframe::egui::{self, Key, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId};
use std::sync::{Arc, Mutex, MutexGuard};
use widgets::Palette;

const VIEWPORT_ID: &str = "snapforge-settings";
const DEFAULT_SIZE: [f32; 2] = [860.0, 620.0];
const MIN_SIZE: [f32; 2] = [680.0, 480.0];

/// Handle to the Settings window. Cheap to clone; all clones share one window.
#[derive(Clone, Default)]
pub struct SettingsWindow {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    session: Option<Session>,
    /// Bring the existing window to the front on the next host frame.
    raise: bool,
}

/// A status line under the page (result of Apply, Import, Export…).
pub(crate) struct Status {
    pub text: String,
    pub error: bool,
}

/// Everything that lives while the window is open.
pub(crate) struct Session {
    /// Live settings when the window opened (or at the last Apply): the merge base.
    pub base: Settings,
    /// The working copy every control edits.
    pub copy: Settings,
    pub tab: Tab,
    pub recorder_sub: usize,
    /// Hotkey row waiting for a key combination.
    pub capturing: Option<usize>,
    pub status: Option<Status>,
    /// FTP password typed in this session (stored only DPAPI-protected).
    pub password: String,
    pub icon: Option<egui::TextureHandle>,
    pub locales: Vec<tabs::Locale>,
    pub palette: Palette,
    pub builder: ViewportBuilder,
    pub ui_state: ui_state::UiState,
    /// Outer position and inner size seen on the last frame.
    pub rect: Option<[f32; 4]>,
    /// Seed for `@` in the file-name preview (stable while the window is open).
    pub preview_seed: u64,
    /// Microphones and cameras, listed when the recorder's audio or webcam page first shows.
    pub devices: Option<(Vec<crate::recorder::AudioDevice>, Vec<crate::recorder::Camera>)>,
    close: bool,
}

impl SettingsWindow {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn viewport_id() -> ViewportId {
        ViewportId::from_hash_of(VIEWPORT_ID)
    }

    /// Open the window on the last used tab (or bring it to the front).
    pub fn open(&self) {
        self.open_at(None);
    }

    /// Open the window on a given tab (e.g. [`Tab::About`] for "About SnapForge").
    pub fn open_tab(&self, tab: Tab) {
        self.open_at(Some(tab));
    }

    fn open_at(&self, tab: Option<Tab>) {
        let mut g = self.lock();
        match g.session.as_mut() {
            Some(s) => {
                if let Some(tab) = tab.filter(|t| t.visible(&s.copy)) {
                    s.tab = tab;
                    s.capturing = None;
                }
            }
            None => g.session = Some(Session::new(tab)),
        }
        g.raise = true;
    }

    pub fn is_open(&self) -> bool {
        self.lock().session.is_some()
    }

    /// Close without applying (like Cancel).
    #[cfg(test)]
    pub fn close(&self) {
        if let Some(s) = self.lock().session.as_mut() {
            s.close = true;
        }
    }

    /// Call on every frame of the host app (any viewport's frame is fine).
    pub fn ui(&mut self, ctx: &egui::Context) {
        let (builder, raise) = {
            let mut g = self.lock();
            let raise = std::mem::take(&mut g.raise);
            match g.session.as_ref() {
                Some(s) => (s.builder.clone(), raise),
                None => return,
            }
        };
        let id = Self::viewport_id();
        if raise {
            ctx.send_viewport_cmd_to(id, ViewportCommand::Visible(true));
            ctx.send_viewport_cmd_to(id, ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd_to(id, ViewportCommand::Focus);
        }
        let inner = self.inner.clone();
        ctx.show_viewport_deferred(id, builder, move |ctx, class| {
            let mut g = inner.lock().unwrap_or_else(|e| e.into_inner());
            g.frame(ctx, class);
        });
    }
}

impl Inner {
    fn frame(&mut self, ctx: &egui::Context, class: ViewportClass) {
        let Some(s) = self.session.as_mut() else {
            // Closed but not yet dropped by the host: keep it hidden.
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            return;
        };
        let saved_style = ctx.style();
        ctx.set_style(widgets::style(&saved_style, &s.palette));

        if class == ViewportClass::Embedded {
            let mut open = true;
            egui::Window::new(t("settings-title"))
                .id(egui::Id::new(VIEWPORT_ID))
                .open(&mut open)
                .default_size(DEFAULT_SIZE)
                .collapsible(false)
                .show(ctx, |ui| s.contents(ui));
            if !open {
                s.close = true;
            }
        } else {
            s.track_rect(ctx);
            if ctx.input(|i| i.viewport().close_requested()) {
                s.close = true;
            }
            egui::CentralPanel::default().frame(egui::Frame::NONE).show(ctx, |ui| s.contents(ui));
        }
        ctx.set_style(saved_style);

        if s.close {
            s.save_ui_state();
            self.session = None;
            if class != ViewportClass::Embedded {
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            }
            // Let the host run a frame so egui drops the viewport.
            ctx.request_repaint_of(ViewportId::ROOT);
        }
    }
}

impl Session {
    fn new(tab: Option<Tab>) -> Self {
        let live = settings::snapshot();
        let mut copy = live.clone();
        // The registry is the truth for "Start with Windows".
        copy.general.start_with_windows = crate::tray::autostart::is_enabled();
        let ui_state = ui_state::load();
        let tab = tab
            .or_else(|| Tab::from_id(&ui_state.settings_window.tab))
            .filter(|t| t.visible(&copy))
            .unwrap_or(Tab::General);
        let dark = match live.general.theme {
            settings::Theme::Dark => true,
            settings::Theme::Light => false,
            settings::Theme::System => crate::common::win::system_dark_mode(),
        };
        let mut builder = ViewportBuilder::default()
            .with_title(t("settings-title"))
            .with_min_inner_size(MIN_SIZE)
            .with_inner_size(DEFAULT_SIZE);
        if let Some([x, y, w, h]) = ui_state.settings_window.rect.and_then(|r| ui_state::sane_rect(r, MIN_SIZE)) {
            builder = builder.with_position([x, y]).with_inner_size([w, h]);
        }
        if let Some(icon) = tabs::app_icon() {
            builder = builder.with_icon(Arc::new(egui::IconData {
                rgba: icon.rgba.clone(),
                width: icon.width,
                height: icon.height,
            }));
        }
        Session {
            base: live,
            copy,
            tab,
            recorder_sub: 0,
            capturing: None,
            status: None,
            password: String::new(),
            icon: None,
            locales: tabs::available_locales(),
            palette: Palette::new(dark),
            builder,
            ui_state,
            rect: None,
            preview_seed: crate::common::rng::Rng::from_entropy().next_u64(),
            devices: None,
            close: false,
        }
    }

    fn track_rect(&mut self, ctx: &egui::Context) {
        let (outer, inner, minimized) =
            ctx.input(|i| (i.viewport().outer_rect, i.viewport().inner_rect, i.viewport().minimized));
        if minimized == Some(true) {
            return;
        }
        if let (Some(o), Some(i)) = (outer, inner) {
            self.rect = Some([o.min.x, o.min.y, i.width(), i.height()]);
        }
    }

    fn save_ui_state(&mut self) {
        let w = &mut self.ui_state.settings_window;
        if self.rect.is_some() {
            w.rect = self.rect;
        }
        w.tab = self.tab.id().to_string();
        ui_state::save(&self.ui_state);
    }

    pub fn set_status(&mut self, text: String, error: bool) {
        self.status = Some(Status { text, error });
    }

    fn dirty(&self) -> bool {
        self.copy != self.base
    }

    /// Apply: merge the edits into the live settings and tell the app.
    fn apply(&mut self) {
        let mut edited = self.copy.clone();
        let want = edited.general.start_with_windows;
        if want != crate::tray::autostart::is_enabled() {
            if let Err(e) = crate::tray::autostart::set_enabled(want) {
                self.set_status(tf("settings-autostart-failed", &[("error", &format!("{e:#}"))]), true);
                edited.general.start_with_windows = !want;
                self.copy.general.start_with_windows = !want;
            }
        }
        let live = settings::snapshot();
        let merged = model::merge3(&self.base, &edited, &live);
        settings::replace(merged.clone());
        app::post(AppCommand::SettingsChanged);
        let lang_changed = merged.general.language != self.base.general.language;
        self.base = merged.clone();
        self.copy = merged;
        if !self.status.as_ref().is_some_and(|s| s.error) {
            let key = if lang_changed { "settings-applied-restart" } else { "settings-applied" };
            self.set_status(t(key), false);
        }
    }

    /// Enter = OK and Escape = Cancel, unless a text field, popup or hotkey capture has them.
    fn handle_keys(&mut self, ctx: &egui::Context) {
        if let Some(row) = self.capturing {
            let win = tabs::win_key_held();
            let events = ctx.input(|i| i.events.clone());
            let mut result = hotkey_input::capture_from_events(&events, win);
            if result.is_none() && tabs::prtsc_pressed() {
                let mods = ctx.input(|i| i.modifiers);
                result = Some(hotkey_input::Captured::Set(hotkey_input::combo_text_vk(mods, win, 0x2C)));
            }
            // The keys belong to the capture: no widget, Tab focus or OK/Cancel may see them.
            ctx.input_mut(|i| {
                i.events.retain(|e| {
                    !matches!(e, egui::Event::Key { .. } | egui::Event::Copy | egui::Event::Cut | egui::Event::Paste(_))
                })
            });
            match result {
                Some(hotkey_input::Captured::Set(text)) => {
                    *hotkey_input::row_text_mut(&mut self.copy.hotkeys, row) = text;
                    self.capturing = None;
                }
                Some(hotkey_input::Captured::Clear) => {
                    hotkey_input::row_text_mut(&mut self.copy.hotkeys, row).clear();
                    self.capturing = None;
                }
                Some(hotkey_input::Captured::Cancel) => self.capturing = None,
                None => ctx.request_repaint_after(std::time::Duration::from_millis(30)),
            }
            return;
        }
        let typing = ctx.wants_keyboard_input();
        let popup = ctx.memory(|m| m.any_popup_open());
        let focused = ctx.memory(|m| m.focused().is_some());
        if !typing && !popup && !focused && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Enter)) {
            self.apply();
            self.close = true;
        } else if !typing && !popup && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)) {
            self.close = true;
        }
    }

    /// The whole window: navigation, page and button bar.
    fn contents(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.handle_keys(&ctx);
        let p = self.palette;

        egui::SidePanel::left("settings-nav")
            .resizable(false)
            .exact_width(190.0)
            .frame(egui::Frame::new().fill(p.nav).inner_margin(egui::Margin {
                left: 10,
                right: 10,
                top: 14,
                bottom: 10,
            }))
            .show_inside(ui, |ui| self.nav(ui));

        egui::TopBottomPanel::bottom("settings-buttons")
            .frame(
                egui::Frame::new()
                    .fill(p.bg)
                    .inner_margin(egui::Margin { left: 18, right: 18, top: 10, bottom: 12 })
                    .stroke(egui::Stroke::new(1.0_f32, p.border)),
            )
            .show_inside(ui, |ui| self.button_bar(ui));

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg).inner_margin(egui::Margin::same(0)))
            .show_inside(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    egui::Frame::new().inner_margin(egui::Margin { left: 22, right: 22, top: 18, bottom: 18 }).show(
                        ui,
                        |ui| {
                            ui.heading(egui::RichText::new(t(self.tab.label_key())).strong());
                            ui.add_space(6.0);
                            tabs::show(ui, self);
                        },
                    );
                });
            });
    }

    fn nav(&mut self, ui: &mut egui::Ui) {
        let p = self.palette;
        ui.horizontal(|ui| {
            if let Some(tex) = tabs::icon_texture(ui.ctx(), &mut self.icon) {
                ui.add(egui::Image::new(&tex).fit_to_exact_size(egui::vec2(24.0, 24.0)));
            }
            ui.label(egui::RichText::new(t("settings-nav-title")).strong().size(16.0));
        });
        ui.add_space(10.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for tab in Tab::ALL {
                if !tab.visible(&self.copy) {
                    continue;
                }
                if widgets::nav_item(ui, &p, self.tab == tab, &t(tab.label_key())).clicked() && self.tab != tab {
                    self.tab = tab;
                    self.capturing = None;
                }
            }
        });
    }

    fn button_bar(&mut self, ui: &mut egui::Ui) {
        let p = self.palette;
        if let Some(st) = &self.status {
            let color = if st.error { p.error } else { p.ok };
            ui.label(egui::RichText::new(&st.text).color(color));
        }
        ui.horizontal(|ui| {
            if self.tab.resettable() {
                let r = ui.button(t("settings-reset-tab")).on_hover_text(t("settings-reset-tab-tip"));
                if r.clicked() {
                    model::reset_tab(self.tab, &mut self.copy);
                    self.set_status(tf("settings-reset-done", &[("tab", &t(self.tab.label_key()))]), false);
                }
            }
            if ui.button(t("settings-reset-all")).on_hover_text(t("settings-reset-all-tip")).clicked() {
                model::reset_all(&mut self.copy);
                self.set_status(t("settings-reset-all-done"), false);
            }
            ui.separator();
            if ui.button(t("settings-import")).clicked() {
                self.import();
            }
            if ui.button(t("settings-export")).clicked() {
                self.export();
            }

            let ok = t("settings-ok");
            let cancel = t("settings-cancel");
            let apply = t("settings-apply");
            let widths: f32 = [&ok, &cancel, &apply].iter().map(|s| widgets::button_width(ui, s, 84.0)).sum();
            let group = widths + 2.0 * ui.spacing().item_spacing.x;
            let gap = ui.available_width() - group;
            if gap > 0.0 {
                ui.add_space(gap);
            }
            if ui.add(widgets::primary_button(&ok, 84.0)).clicked() {
                self.apply();
                self.close = true;
            }
            if ui.add(egui::Button::new(&cancel).min_size(egui::vec2(84.0, 28.0))).clicked() {
                self.close = true;
            }
            let dirty = self.dirty();
            if ui.add_enabled(dirty, egui::Button::new(&apply).min_size(egui::vec2(84.0, 28.0))).clicked() {
                self.apply();
            }
        });
    }

    fn import(&mut self) {
        let filters = [(t("settings-filter-toml"), "*.toml".to_string())];
        match dialogs::open_file(&t("settings-import-title"), &filters, None) {
            Ok(Some(path)) => match settings::import_export::import_from(&path, &self.copy) {
                Ok(s) => {
                    self.copy = s;
                    self.password.clear();
                    self.set_status(tf("settings-imported", &[("path", &path.display().to_string())]), false);
                }
                Err(e) => self.set_status(tf("settings-import-failed", &[("error", &format!("{e:#}"))]), true),
            },
            Ok(None) => {}
            Err(e) => self.set_status(tf("settings-import-failed", &[("error", &format!("{e:#}"))]), true),
        }
    }

    fn export(&mut self) {
        let filters = [(t("settings-filter-toml"), "*.toml".to_string())];
        let docs = crate::common::win::documents_dir();
        match dialogs::save_file(&t("settings-export-title"), &filters, "SnapForge-settings.toml", "toml", Some(&docs))
        {
            Ok(Some(path)) => match settings::import_export::export_to(&path, &self.copy) {
                Ok(()) => self.set_status(tf("settings-exported", &[("path", &path.display().to_string())]), false),
                Err(e) => self.set_status(tf("settings-export-failed", &[("error", &format!("{e:#}"))]), true),
            },
            Ok(None) => {}
            Err(e) => self.set_status(tf("settings-export-failed", &[("error", &format!("{e:#}"))]), true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every Fluent key the window uses exists in the English file.
    #[test]
    fn all_strings_are_localized() {
        let ftl = crate::i18n::parse_ftl(include_str!("../../locales/en-US/main.ftl"));
        // egui id salts that look like keys.
        let ids = ["settings-combo", "settings-form", "settings-generic", "settings-nav", "settings-buttons"];
        let sources = [
            include_str!("mod.rs"),
            include_str!("tabs.rs"),
            include_str!("widgets.rs"),
            include_str!("hotkey_input.rs"),
            include_str!("model.rs"),
            include_str!("generic.rs"),
        ];
        let mut keys: Vec<String> = Vec::new();
        for src in sources {
            for (i, _) in src.match_indices("\"settings-") {
                let rest = &src[i + 1..];
                let end = rest.find('"').unwrap_or(rest.len());
                let k = &rest[..end];
                if k.ends_with('-') || k.contains('{') || k.starts_with("settings-field-") || ids.contains(&k) {
                    continue;
                }
                keys.push(k.to_string());
            }
        }
        keys.extend(Tab::ALL.iter().map(|t| t.label_key().to_string()));
        keys.extend(crate::settings::ImageFormat::ALL.iter().map(|f| format!("settings-format-{}", f.extension())));
        keys.extend(["error", "warn", "info", "debug", "trace"].iter().map(|l| format!("settings-log-{l}")));
        keys.extend(hotkey_input::SYSTEM_HOTKEYS.iter().map(|(_, k)| k.to_string()));
        keys.extend(tabs::RECORDER_SUBTABS.iter().map(|(k, _)| k.to_string()));
        keys.extend(tabs::TOKENS.iter().map(|(_, k)| k.to_string()));
        keys.push("language-name".into());
        let missing: Vec<&String> = keys.iter().filter(|k| !ftl.contains_key(k.as_str())).collect();
        assert!(missing.is_empty(), "missing in main.ftl: {missing:?}");
    }

    #[test]
    fn open_close_lifecycle() {
        let w = SettingsWindow::new();
        assert!(!w.is_open());
        w.open_tab(Tab::About);
        assert!(w.is_open());
        assert_eq!(w.lock().session.as_ref().map(|s| s.tab), Some(Tab::About));
        w.open_tab(Tab::Hotkeys);
        assert_eq!(w.lock().session.as_ref().map(|s| s.tab), Some(Tab::Hotkeys));
        w.close();
        assert!(w.lock().session.as_ref().is_some_and(|s| s.close));
    }
}
