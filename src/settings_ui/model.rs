//! Pure model code for the Settings window: tabs, per-tab reset, and the three-way merge
//! used by Apply (so values that changed elsewhere while the window was open survive).

use crate::settings::{self, Settings};
use serde::de::DeserializeOwned;
use serde::Serialize;

/// Left-navigation tabs, in display order.
///
/// To add a dedicated page for a section (e.g. Draw), keep the variant here, write a
/// `fn draw(ui, &mut Session)` in `tabs.rs` and route it in `tabs::show`. Sections without a
/// dedicated page are edited generically (`generic.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab {
    General,
    Capture,
    Output,
    Hotkeys,
    FileName,
    AutoSave,
    Editor,
    Toolbar,
    Recorder,
    Draw,
    Pin,
    Ocr,
    Tools,
    Print,
    History,
    Language,
    About,
}

impl Tab {
    pub const ALL: [Tab; 17] = [
        Tab::General,
        Tab::Capture,
        Tab::Output,
        Tab::Hotkeys,
        Tab::FileName,
        Tab::AutoSave,
        Tab::Editor,
        Tab::Toolbar,
        Tab::Recorder,
        Tab::Draw,
        Tab::Pin,
        Tab::Ocr,
        Tab::Tools,
        Tab::Print,
        Tab::History,
        Tab::Language,
        Tab::About,
    ];

    /// Stable id (remembered in `ui_state.toml`).
    pub fn id(self) -> &'static str {
        match self {
            Tab::General => "general",
            Tab::Capture => "capture",
            Tab::Output => "output",
            Tab::Hotkeys => "hotkeys",
            Tab::FileName => "filename",
            Tab::AutoSave => "autosave",
            Tab::Editor => "editor",
            Tab::Toolbar => "toolbar",
            Tab::Recorder => "recorder",
            Tab::Draw => "draw",
            Tab::Pin => "pin",
            Tab::Ocr => "ocr",
            Tab::Tools => "tools",
            Tab::Print => "print",
            Tab::History => "history",
            Tab::Language => "language",
            Tab::About => "about",
        }
    }

    pub fn from_id(s: &str) -> Option<Tab> {
        Tab::ALL.into_iter().find(|t| t.id() == s)
    }

    /// Fluent key of the tab label.
    pub fn label_key(self) -> &'static str {
        match self {
            Tab::General => "settings-tab-general",
            Tab::Capture => "settings-tab-capture",
            Tab::Output => "settings-tab-output",
            Tab::Hotkeys => "settings-tab-hotkeys",
            Tab::FileName => "settings-tab-filename",
            Tab::AutoSave => "settings-tab-autosave",
            Tab::Editor => "settings-tab-editor",
            Tab::Toolbar => "settings-tab-toolbar",
            Tab::Recorder => "settings-tab-recorder",
            Tab::Draw => "settings-tab-draw",
            Tab::Pin => "settings-tab-pin",
            Tab::Ocr => "settings-tab-ocr",
            Tab::Tools => "settings-tab-tools",
            Tab::Print => "settings-tab-print",
            Tab::History => "settings-tab-history",
            Tab::Language => "settings-tab-language",
            Tab::About => "settings-tab-about",
        }
    }

    /// Draw, Pin, OCR and Tools appear once their settings sections have fields.
    pub fn visible(self, s: &Settings) -> bool {
        match self {
            Tab::Draw => has_fields(&s.draw),
            Tab::Pin => has_fields(&s.pin),
            Tab::Ocr => has_fields(&s.ocr),
            Tab::Tools => has_fields(&s.tools),
            _ => true,
        }
    }

    /// Whether the tab has anything to reset.
    pub fn resettable(self) -> bool {
        self != Tab::About
    }
}

/// True when a settings section serializes to a non-empty table.
pub fn has_fields<T: Serialize>(v: &T) -> bool {
    match toml::Value::try_from(v) {
        Ok(toml::Value::Table(t)) => !t.is_empty(),
        _ => false,
    }
}

/// Reset the values a tab owns to their defaults. State that is not shown on the tab
/// (toolbar position, folder history, recent files…) is kept.
pub fn reset_tab(tab: Tab, s: &mut Settings) {
    match tab {
        Tab::General => {
            let language = std::mem::take(&mut s.general.language);
            s.general = settings::General { language, ..Default::default() };
        }
        Tab::Capture => {
            let d = settings::CaptureSettings::default();
            let c = &mut s.capture;
            c.engine = d.engine;
            c.fullscreen_all_monitors = d.fullscreen_all_monitors;
            c.include_cursor = d.include_cursor;
            c.delay_secs = d.delay_secs;
            c.capture_on_release = d.capture_on_release;
            c.show_magnifier = d.show_magnifier;
            c.show_crosshair = d.show_crosshair;
            c.fixed_presets = d.fixed_presets;
            c.fixed_index = d.fixed_index;
            c.scroll = d.scroll;
        }
        Tab::Output => {
            let history = std::mem::take(&mut s.output.folder_history);
            s.output = settings::OutputSettings { folder_history: history, ..Default::default() };
        }
        Tab::Hotkeys => s.hotkeys = Default::default(),
        Tab::FileName => {
            let d = settings::FileNameSettings::default();
            s.filename.template = d.template;
            s.filename.counter = d.counter;
            s.filename.reset_daily = d.reset_daily;
        }
        Tab::AutoSave => {
            let d = settings::CaptureSettings::default();
            let c = &mut s.capture;
            c.auto_capture_enabled = d.auto_capture_enabled;
            c.auto_capture_interval_secs = d.auto_capture_interval_secs;
            c.auto_capture_mode = d.auto_capture_mode;
            c.auto_capture_folder = d.auto_capture_folder;
        }
        Tab::Editor => s.editor.external_editors = settings::EditorSettings::default().external_editors,
        Tab::Toolbar => {
            let d = settings::ToolbarSettings::default();
            let t = &mut s.toolbar;
            t.visible = d.visible;
            t.big_buttons = d.big_buttons;
            t.locked = d.locked;
            t.auto_hide = d.auto_hide;
            t.buttons = d.buttons;
        }
        Tab::Recorder => s.recorder = Default::default(),
        Tab::Draw => s.draw = Default::default(),
        Tab::Pin => s.pin = Default::default(),
        Tab::Ocr => s.ocr = Default::default(),
        Tab::Tools => s.tools = Default::default(),
        Tab::Print => s.print = Default::default(),
        Tab::History => s.history = Default::default(),
        Tab::Language => s.general.language = settings::General::default().language,
        Tab::About => {}
    }
}

/// "Reset All": every tab's reset, in order.
pub fn reset_all(s: &mut Settings) {
    for t in Tab::ALL {
        reset_tab(t, s);
    }
}

/// Three-way merge: values the user changed (`edited` differs from `base`) win; all other
/// values come from `live`, which may have moved on while the window was open (toolbar
/// position, file-name counter, fixed-size index…). Arrays are treated as single values.
pub fn merge3(base: &Settings, edited: &Settings, live: &Settings) -> Settings {
    fn to_value<T: Serialize>(v: &T) -> Option<toml::Value> {
        toml::Value::try_from(v).ok()
    }
    fn merge(base: Option<&toml::Value>, edited: &toml::Value, live: Option<&toml::Value>) -> toml::Value {
        match (edited, live) {
            (toml::Value::Table(e), Some(toml::Value::Table(l))) => {
                let b = base.and_then(|b| b.as_table());
                let mut out = l.clone();
                for (k, ev) in e {
                    let bv = b.and_then(|b| b.get(k));
                    out.insert(k.clone(), merge(bv, ev, l.get(k)));
                }
                // A value the user cleared (an `Option` set to `None` has no TOML key).
                if let Some(b) = b {
                    for k in b.keys() {
                        if !e.contains_key(k) {
                            out.remove(k);
                        }
                    }
                }
                toml::Value::Table(out)
            }
            (_, Some(l)) if base == Some(edited) => l.clone(),
            _ => edited.clone(),
        }
    }
    fn from_value<T: DeserializeOwned>(v: toml::Value) -> Option<T> {
        v.try_into().ok()
    }
    let (Some(b), Some(e), Some(l)) = (to_value(base), to_value(edited), to_value(live)) else {
        return edited.clone();
    };
    from_value(merge(Some(&b), &e, Some(&l))).unwrap_or_else(|| edited.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{Destination, Size, Theme};

    #[test]
    fn ids_roundtrip() {
        for t in Tab::ALL {
            assert_eq!(Tab::from_id(t.id()), Some(t));
        }
    }

    /// Change something on every tab, then reset tab by tab: each reset restores only its own values.
    #[test]
    fn reset_per_tab() {
        let d = Settings::default();
        let mut s = Settings::default();
        s.general.theme = Theme::Dark;
        s.general.language = "de-DE".into();
        s.capture.delay_secs = 9;
        s.capture.fixed_presets.push(Size { w: 1, h: 2 });
        s.capture.auto_capture_interval_secs = 5;
        s.output.destination = Destination::Clipboard;
        s.output.folder_history = vec!["C:\\x".into()];
        s.hotkeys.region = "F9".into();
        s.filename.template = "x".into();
        s.filename.last_date = "2026-01-01".into();
        s.editor.external_editors.clear();
        s.editor.recent_files = vec!["a.png".into()];
        s.toolbar.big_buttons = true;
        s.toolbar.x = Some(10);

        reset_tab(Tab::General, &mut s);
        assert_eq!(s.general.theme, d.general.theme);
        assert_eq!(s.general.language, "de-DE", "language belongs to the Language tab");

        reset_tab(Tab::Capture, &mut s);
        assert_eq!(s.capture.delay_secs, 0);
        assert_eq!(s.capture.fixed_presets, d.capture.fixed_presets);
        assert_eq!(s.capture.auto_capture_interval_secs, 5, "auto capture belongs to Auto Save");

        reset_tab(Tab::AutoSave, &mut s);
        assert_eq!(s.capture, d.capture);

        reset_tab(Tab::Output, &mut s);
        assert_eq!(s.output.destination, Destination::Editor);
        assert_eq!(s.output.folder_history, vec!["C:\\x".to_string()]);

        reset_tab(Tab::Hotkeys, &mut s);
        assert_eq!(s.hotkeys, d.hotkeys);

        reset_tab(Tab::FileName, &mut s);
        assert_eq!(s.filename.template, d.filename.template);
        assert_eq!(s.filename.last_date, "2026-01-01");

        reset_tab(Tab::Editor, &mut s);
        assert_eq!(s.editor.external_editors, d.editor.external_editors);
        assert_eq!(s.editor.recent_files.len(), 1);

        reset_tab(Tab::Toolbar, &mut s);
        assert!(!s.toolbar.big_buttons);
        assert_eq!(s.toolbar.x, Some(10), "position is not a tab value");

        reset_tab(Tab::Language, &mut s);
        assert_eq!(s.general, d.general);

        let mut all = s.clone();
        all.general.theme = Theme::Light;
        reset_all(&mut all);
        assert_eq!(all, s);
    }

    #[test]
    fn merge_keeps_external_changes() {
        let base = Settings::default();
        let mut edited = base.clone();
        edited.output.destination = Destination::File;
        edited.capture.fixed_presets = vec![Size { w: 5, h: 5 }];
        let mut live = base.clone();
        live.toolbar.x = Some(42);
        live.filename.counter = 17;
        live.capture.fixed_index = 1;

        let m = merge3(&base, &edited, &live);
        assert_eq!(m.output.destination, Destination::File);
        assert_eq!(m.capture.fixed_presets, vec![Size { w: 5, h: 5 }]);
        assert_eq!(m.toolbar.x, Some(42));
        assert_eq!(m.filename.counter, 17);
        assert_eq!(m.capture.fixed_index, 1);

        // An Option the user cleared stays cleared; an unchanged one follows live.
        let mut base2 = base.clone();
        base2.toolbar.y = Some(3);
        let edited2 =
            Settings { toolbar: settings::ToolbarSettings { y: None, ..base2.toolbar.clone() }, ..base2.clone() };
        let m = merge3(&base2, &edited2, &base2);
        assert_eq!(m.toolbar.y, None);
    }
}
