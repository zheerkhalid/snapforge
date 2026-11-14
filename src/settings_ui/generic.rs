//! Generic editor for settings sections that do not have a dedicated page yet (recorder,
//! print, draw, pin, OCR, tools). The section is serialized to a TOML table, each field is
//! shown with a widget matching its type, and an edit is written back by deserializing.
//! An edit that does not deserialize (e.g. an unknown enum name) is not applied.
//!
//! Labels come from the Fluent key `settings-field-<section>-<field>` (underscores become
//! dashes); without one, the field name is shown in a readable form.

use crate::i18n;
use eframe::egui::{self, Ui};
use serde::de::DeserializeOwned;
use serde::Serialize;

/// Fluent key for a field label.
pub fn label_key(section: &str, field: &str) -> String {
    format!("settings-field-{section}-{field}").replace('_', "-")
}

/// Localized label, or the field name made readable ("video_fps" → "Video fps").
pub fn field_label(section: &str, field: &str) -> String {
    let key = label_key(section, field);
    let s = i18n::t(&key);
    if s != key {
        return s;
    }
    let mut out = field.replace('_', " ");
    if let Some(first) = out.get(0..1) {
        let up = first.to_uppercase();
        out.replace_range(0..1, &up);
    }
    out
}

/// Fields of a section, in serialization order.
pub fn field_names<T: Serialize>(v: &T) -> Vec<String> {
    match toml::Value::try_from(v) {
        Ok(toml::Value::Table(t)) => t.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

/// Edit the fields of `value` accepted by `filter`. Returns true when `value` changed.
pub fn section<T: Serialize + DeserializeOwned>(
    ui: &mut Ui,
    section: &str,
    value: &mut T,
    filter: &dyn Fn(&str) -> bool,
) -> bool {
    let Ok(toml::Value::Table(mut table)) = toml::Value::try_from(&*value) else {
        return false;
    };
    let mut changed = false;
    egui::Grid::new(("settings-generic", section)).num_columns(2).spacing([18.0, 10.0]).min_col_width(170.0).show(
        ui,
        |ui| {
            for (k, v) in table.iter_mut() {
                if filter(k) {
                    changed |= field(ui, section, k, v);
                }
            }
        },
    );
    if changed {
        match toml::Value::Table(table).try_into::<T>() {
            Ok(new) => *value = new,
            Err(e) => {
                log::debug!("settings window: edit of [{section}] not applied: {e}");
                return false;
            }
        }
    }
    changed
}

fn field(ui: &mut Ui, section: &str, key: &str, v: &mut toml::Value) -> bool {
    let label = field_label(section, key);
    match v {
        toml::Value::Boolean(b) => {
            ui.label("");
            let r = ui.checkbox(b, label);
            ui.end_row();
            r.changed()
        }
        toml::Value::Table(t) => {
            let l = ui.label(egui::RichText::new(label).strong());
            ui.end_row();
            let _ = l;
            let mut changed = false;
            let sub = format!("{section}-{key}");
            for (k, v) in t.iter_mut() {
                changed |= field(ui, &sub, k, v);
            }
            changed
        }
        _ => {
            let l = ui.label(label);
            let r = match v {
                toml::Value::Integer(i) => ui.add(egui::DragValue::new(i)),
                toml::Value::Float(f) => ui.add(egui::DragValue::new(f).speed(0.1)),
                toml::Value::String(s) => ui.add(egui::TextEdit::singleline(s).desired_width(260.0)),
                other => ui.label(other.to_string()),
            };
            let r = r.labelled_by(l.id);
            ui.end_row();
            r.changed()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readable_fallback_label() {
        assert_eq!(field_label("zz", "video_fps"), "Video fps");
        assert_eq!(label_key("recorder", "video_fps"), "settings-field-recorder-video-fps");
    }

    #[test]
    fn names_follow_fields() {
        #[derive(Serialize)]
        struct S {
            a: bool,
            b_c: u32,
        }
        assert_eq!(field_names(&S { a: true, b_c: 1 }), vec!["a".to_string(), "b_c".to_string()]);
    }
}
