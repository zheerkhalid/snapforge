//! Tools ▸ Hide Private Details (OCR finds e-mail addresses, phone numbers and card
//! numbers; each one is covered by an editable annotation) and Tools ▸ Read QR Code.
//! Recognition and decoding run on worker threads; this file holds the two dialogs and
//! what happens with the results.

use super::annot;
use super::app::EditorApp;
use super::document::DocId;
use super::modals::Modal;
use super::style;
use crate::common::Rect;
use crate::draw::{self, Blur, BlurMode, Kind, Object, ObjectId, RectF, Shape, ShapeKind, Style};
use crate::i18n::{t, tf};
use crate::redact::{self, Hit, Kinds};
use crate::settings::editor_ext::{RedactSettings, RedactStyle};
use crate::settings::{self};
use eframe::egui::{self, vec2, Align, Id, Key, Layout, Modifiers, RichText};

/// Height of a dialog's button row (a fixed height: a right-to-left layout in a modal
/// would otherwise take all the height it can get).
const BUTTON_ROW: f32 = 30.0;

/// The annotations that cover `hits` in an image of `width` × `height`, with ids from
/// `first_id` upwards. Boxes are clipped to the image; ones that fall outside are dropped.
pub fn cover_objects(hits: &[Hit], style: RedactStyle, first_id: ObjectId, width: u32, height: u32) -> Vec<Object> {
    let (iw, ih) = (width as f32, height as f32);
    let mut out = Vec::new();
    for h in hits {
        let x0 = h.rect.x.max(0.0).floor();
        let y0 = h.rect.y.max(0.0).floor();
        let x1 = (h.rect.x + h.rect.w).min(iw).ceil();
        let y1 = (h.rect.y + h.rect.h).min(ih).ceil();
        if x1 - x0 < 2.0 || y1 - y0 < 2.0 {
            continue;
        }
        let rect = RectF::new(x0, y0, x1 - x0, y1 - y0);
        let id = first_id + out.len() as ObjectId;
        let obj = match style {
            RedactStyle::Bar => Object::new(
                id,
                Kind::Shape(Shape { kind: ShapeKind::Rect, rect, ..Default::default() }),
                Style { color: draw::Color::BLACK, fill: draw::Color::BLACK, width: 1.0, ..Default::default() },
            ),
            RedactStyle::Pixelate | RedactStyle::Blur => {
                let pixelate = style == RedactStyle::Pixelate;
                // Strong enough that the text cannot be read back: blocks of a third of
                // the box height, or a blur radius of over half of it.
                let strength = if pixelate { (rect.h / 3.0).clamp(6.0, 48.0) } else { (rect.h * 0.6).clamp(8.0, 64.0) };
                Object::new(
                    id,
                    Kind::Blur(Blur {
                        rect,
                        ellipse: false,
                        mode: if pixelate { BlurMode::Pixelate } else { BlurMode::Blur },
                        strength: strength.round(),
                    }),
                    Style::default(),
                )
            }
        };
        out.push(obj);
    }
    out
}

/// "2 e-mail addresses, 1 phone number" for the result message.
fn summary(hits: &[Hit]) -> String {
    let (mail, phone, card) = redact::counts(hits);
    let mut parts = Vec::new();
    for (n, key) in [(mail, "redact-count-email"), (phone, "redact-count-phone"), (card, "redact-count-card")] {
        if n > 0 {
            // The Fluent subset used here has no plural selectors: one key per form.
            let key = format!("{key}-{}", if n == 1 { "one" } else { "many" });
            parts.push(tf(&key, &[("n", &n.to_string())]));
        }
    }
    parts.join(", ")
}

impl EditorApp {
    /// Tools ▸ Hide Private Details: ask what to look for.
    pub(super) fn open_redact(&mut self) {
        if self.tabs.active().is_some() {
            self.modal = Some(Modal::Redact(settings::get().editor_ext.redact.clone()));
        }
    }

    pub(super) fn redact_modal(&mut self, ctx: &egui::Context, mut form: RedactSettings) -> Option<Modal> {
        let (mut go, mut cancel) = (false, false);
        let kinds = Kinds { email: form.email, phone: form.phone, card: form.card };
        let resp = egui::Modal::new(Id::new("redact")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.label(RichText::new(t("redact-title")).size(17.0).strong());
            ui.add_space(6.0);
            ui.label(t("redact-intro"));
            ui.add_space(8.0);
            ui.checkbox(&mut form.email, t("redact-email"));
            ui.checkbox(&mut form.phone, t("redact-phone"));
            ui.checkbox(&mut form.card, t("redact-card"));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(t("redact-style"));
                for (style, key) in [
                    (RedactStyle::Pixelate, "redact-style-pixelate"),
                    (RedactStyle::Blur, "redact-style-blur"),
                    (RedactStyle::Bar, "redact-style-bar"),
                ] {
                    ui.radio_value(&mut form.style, style, t(key));
                }
            });
            ui.add_space(8.0);
            ui.label(RichText::new(t("redact-note")).small().weak());
            ui.add_space(12.0);
            ui.allocate_ui_with_layout(
                vec2(ui.available_width(), BUTTON_ROW),
                Layout::right_to_left(Align::Center),
                |ui| {
                    let ok = style::primary_button(t("redact-go")).min_size(vec2(120.0, 28.0));
                    go = ui.add_enabled(kinds.any(), ok).on_hover_text("Enter").clicked();
                    cancel = ui
                        .add(egui::Button::new(t("editor-cancel")).min_size(vec2(84.0, 28.0)))
                        .on_hover_text("Esc")
                        .clicked();
                },
            );
        });
        if !go && kinds.any() && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
            go = true;
        }
        if go {
            let kinds = Kinds { email: form.email, phone: form.phone, card: form.card };
            let keep = form.clone();
            settings::update(move |s| s.editor_ext.redact = keep);
            self.start_redact(kinds);
            return None;
        }
        (!(cancel || resp.should_close())).then_some(Modal::Redact(form))
    }

    fn start_redact(&mut self, kinds: Kinds) {
        self.finish_draw();
        let Some(d) = self.tabs.active() else { return };
        // With a selection, only what lies inside it is covered.
        let within = d.selection.as_ref().map(|s| s.bounds);
        let lang = settings::get().ocr.language.clone();
        self.jobs.find_private(d.id, d.export_job(false), kinds, within, lang);
        self.toast(t("redact-working"), false);
    }

    /// The worker found `hits` in a picture of `size`: cover them in one undo step.
    pub(super) fn on_redact_found(&mut self, doc: DocId, size: (u32, u32), result: Result<Vec<Hit>, String>) {
        let hits = match result {
            Ok(h) => h,
            Err(e) => return self.toast(tf("redact-failed", &[("error", &e)]), true),
        };
        let style = settings::get().editor_ext.redact.style;
        let Some(d) = self.tabs.get_mut(doc) else { return };
        if (d.image.width, d.image.height) != size {
            // Cropped or resized while the text was being read: the boxes no longer fit.
            return self.toast(t("redact-changed"), true);
        }
        if hits.is_empty() {
            return self.toast(t("redact-none"), false);
        }
        let mut added = 0;
        annot::layer::edit(d, &t("redact-undo-name"), |objs| {
            let new = cover_objects(&hits, style, draw::edit::next_id(objs), size.0, size.1);
            added = new.len();
            objs.extend(new);
            added > 0
        });
        if added == 0 {
            return self.toast(t("redact-none"), false);
        }
        self.toast(tf("redact-done", &[("what", &summary(&hits))]), false);
    }

    /// Tools ▸ Read QR Code: the selection when there is one, else the whole picture.
    pub(super) fn start_read_qr(&mut self) {
        self.finish_draw();
        let Some(d) = self.tabs.active() else { return };
        self.jobs.read_qr(d.export_job(d.selection.is_some()));
    }

    pub(super) fn on_qr_read(&mut self, codes: Vec<String>) {
        if codes.is_empty() {
            self.toast(t("qr-none"), false);
        } else {
            self.modal = Some(Modal::QrCodes(codes));
        }
    }

    pub(super) fn qr_modal(&mut self, ctx: &egui::Context, codes: Vec<String>) -> Option<Modal> {
        let mut close = false;
        let mut copied = false;
        let resp = egui::Modal::new(Id::new("qr-codes")).show(ctx, |ui| {
            ui.set_width(460.0);
            let title =
                if codes.len() == 1 { t("qr-title") } else { tf("qr-title-many", &[("n", &codes.len().to_string())]) };
            ui.label(RichText::new(title).size(17.0).strong());
            ui.add_space(6.0);
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                for (i, code) in codes.iter().enumerate() {
                    if i > 0 {
                        ui.separator();
                    }
                    // Read-only but selectable, so part of the text can be copied.
                    let mut text = code.as_str();
                    ui.add(
                        egui::TextEdit::multiline(&mut text)
                            .desired_width(f32::INFINITY)
                            .desired_rows(code.lines().count().clamp(1, 6)),
                    );
                    ui.horizontal(|ui| {
                        if ui.button(t("qr-copy")).clicked() {
                            match crate::output::clipboard::copy_text(code) {
                                Ok(()) => copied = true,
                                Err(e) => log::warn!("editor: copying the QR text failed: {e:#}"),
                            }
                        }
                        if crate::qr::is_link(code) && ui.button(t("qr-open")).on_hover_text(code.trim()).clicked() {
                            crate::common::win::shell_open(code.trim());
                        }
                    });
                }
            });
            ui.add_space(10.0);
            ui.allocate_ui_with_layout(
                vec2(ui.available_width(), BUTTON_ROW),
                Layout::right_to_left(Align::Center),
                |ui| {
                    close = ui.add(style::primary_button(t("editor-close")).min_size(vec2(88.0, 28.0))).clicked();
                },
            );
        });
        if copied {
            self.toast(t("ocr-status-copied"), false);
        }
        let enter = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
        (!(close || enter || resp.should_close())).then_some(Modal::QrCodes(codes))
    }
}

/// Keep the hits whose centre lies inside `within` (image pixels); all of them without one.
pub fn inside(hits: Vec<Hit>, within: Option<Rect>) -> Vec<Hit> {
    let Some(r) = within else { return hits };
    hits.into_iter()
        .filter(|h| {
            let (cx, cy) = h.rect.center();
            cx >= r.x as f32 && cx < r.right() as f32 && cy >= r.y as f32 && cy < r.bottom() as f32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr::BoxF;
    use crate::redact::Kind as K;

    fn hit(kind: K, x: f32, y: f32, w: f32, h: f32) -> Hit {
        Hit { kind, rect: BoxF { x, y, w, h } }
    }

    #[test]
    fn covers_are_clipped_numbered_and_strong_enough() {
        let hits = [
            hit(K::Email, 10.4, 20.6, 100.0, 24.0),
            hit(K::Phone, -8.0, 90.0, 50.0, 30.0),  // sticks out on the left
            hit(K::Card, 400.0, 10.0, 50.0, 20.0),  // outside the picture
            hit(K::Card, 150.0, 190.0, 80.0, 30.0), // sticks out at the bottom
        ];
        let objs = cover_objects(&hits, RedactStyle::Pixelate, 7, 200, 200);
        assert_eq!(objs.iter().map(|o| o.id).collect::<Vec<_>>(), [7, 8, 9]);
        let rects: Vec<RectF> = objs
            .iter()
            .map(|o| match &o.kind {
                Kind::Blur(b) => {
                    assert_eq!(b.mode, BlurMode::Pixelate);
                    assert!(b.strength >= 6.0 && !b.ellipse);
                    b.rect
                }
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(rects[0], RectF::new(10.0, 20.0, 101.0, 25.0));
        assert_eq!(rects[1], RectF::new(0.0, 90.0, 42.0, 30.0));
        assert_eq!(rects[2], RectF::new(150.0, 190.0, 50.0, 10.0));
        assert!(objs.iter().all(|o| o.is_raster()));
    }

    #[test]
    fn styles_map_to_blur_and_solid_bars() {
        let hits = [hit(K::Email, 10.0, 10.0, 100.0, 40.0)];
        let blur = cover_objects(&hits, RedactStyle::Blur, 1, 300, 300);
        assert!(matches!(&blur[0].kind, Kind::Blur(b) if b.mode == BlurMode::Blur && b.strength == 24.0));
        let bar = cover_objects(&hits, RedactStyle::Bar, 1, 300, 300);
        assert!(matches!(&bar[0].kind, Kind::Shape(s) if s.kind == ShapeKind::Rect && s.rect.w == 100.0));
        assert_eq!(bar[0].style.fill, draw::Color::BLACK);
        assert!(!bar[0].is_raster());
        assert!(cover_objects(&[], RedactStyle::Bar, 1, 300, 300).is_empty());
    }

    #[test]
    fn a_covered_address_is_really_gone_from_the_pixels() {
        // Black "text" stripes on white; after flattening with the cover, no pixel in
        // the box is as dark as the text was (pixelate and blur average it away, a bar
        // replaces it), and pixels outside the box are untouched.
        let mut img = crate::common::Image::filled(200, 80, [255, 255, 255, 255]);
        for x in (20..180).step_by(4) {
            for y in 30..50 {
                img.set_pixel(x, y, [0, 0, 0, 255]);
            }
        }
        img.set_pixel(5, 5, [0, 0, 0, 255]);
        let hits = [hit(K::Email, 16.0, 26.0, 168.0, 28.0)];
        for style in [RedactStyle::Pixelate, RedactStyle::Blur] {
            let objs = cover_objects(&hits, style, 1, 200, 80);
            let out = draw::render(&img, &objs, &draw::RenderOptions::default());
            for y in 34..46 {
                for x in 24..176 {
                    let p = out.pixel(x, y);
                    assert!(p[0] > 120 && p[0] < 250, "{style:?} left text at {x},{y}: {p:?}");
                }
            }
            assert_eq!(out.pixel(5, 5), [0, 0, 0, 255]);
            assert_eq!(out.pixel(190, 70), [255, 255, 255, 255]);
        }
        let objs = cover_objects(&hits, RedactStyle::Bar, 1, 200, 80);
        let out = draw::render(&img, &objs, &draw::RenderOptions::default());
        assert_eq!(out.pixel(100, 40), [0, 0, 0, 255]);
        assert_eq!(out.pixel(22, 40), [0, 0, 0, 255]);
        assert_eq!(out.pixel(190, 70), [255, 255, 255, 255]);
    }

    /// End to end with the real Windows text recognition (skipped when no OCR language is
    /// installed): the details are found in rendered text, and after covering them the
    /// same recognition can no longer read them.
    #[test]
    fn real_ocr_finds_the_details_and_cannot_read_them_once_covered() {
        let langs = std::thread::spawn(crate::ocr::languages).join().unwrap_or_default();
        if langs.is_empty() {
            eprintln!("redact: no recognizer language installed; skipping");
            return;
        }
        let text = "Contact jane.doe@example.com today
or call +44 20 7946 0958 after nine
Card 4242 4242 4242 4242 expires soon";
        let img = crate::pin::gdi::text_image(text, 36, [0, 0, 0], [255, 255, 255], 24).expect("render text");
        let read = |img: crate::common::Image| {
            std::thread::spawn(move || crate::ocr::recognize(&img, None)).join().expect("thread").expect("ocr")
        };
        let ocr = read(img.clone());
        let hits = redact::find(&ocr, Kinds::ALL);
        assert_eq!(redact::counts(&hits), (1, 1, 1), "recognized: {:?}", ocr.text(true));
        for style in [RedactStyle::Pixelate, RedactStyle::Blur, RedactStyle::Bar] {
            let objs = cover_objects(&hits, style, 1, img.width, img.height);
            assert_eq!(objs.len(), 3);
            let covered = draw::render(&img, &objs, &draw::RenderOptions::default());
            let again = read(covered);
            let seen = again.text(true).to_lowercase();
            assert!(redact::find(&again, Kinds::ALL).is_empty(), "{style:?} still readable: {seen:?}");
            for gone in ["example", "jane", "7946", "0958", "4242"] {
                assert!(!seen.contains(gone), "{style:?} left {gone:?} readable: {seen:?}");
            }
            // Most words around the covers are still read (a word right next to a
            // black bar is sometimes dropped by the recognizer).
            let kept =
                ["contact", "today", "call", "nine", "card", "soon"].iter().filter(|w| seen.contains(**w)).count();
            assert!(kept >= 4, "{style:?} hid too much: {seen:?}");
        }
    }

    /// Regenerates the two example pictures in `docs/screenshots` from the app's own
    /// render paths: `cargo test docs_example_pictures -- --ignored`.
    #[test]
    #[ignore = "writes docs/screenshots/beautify.png and hide-private.png"]
    fn docs_example_pictures() {
        use crate::effects::{beautify, Backdrop, BeautifyParams};
        use crate::output::save::{load_image, save_image_as};
        use crate::settings::ImageFormat;
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs").join("screenshots");
        // Beautify: an existing screenshot of the app on the "Ocean" gradient.
        let shot = load_image(&dir.join("pin.png")).expect("pin.png");
        let p =
            BeautifyParams { backdrop: Backdrop::Gradient(1), padding: 56, corner_radius: 12, shadow: 60, ratio: None };
        save_image_as(&beautify(&shot, &p), &dir.join("beautify.png"), ImageFormat::Png, 100).expect("save");

        // Hide Private Details: made-up contact details, before and after.
        if std::thread::spawn(crate::ocr::languages).join().unwrap_or_default().is_empty() {
            eprintln!("no recognizer language installed; hide-private.png not written");
            return;
        }
        let text = "Order 20418 is ready
Mail: jane.doe@example.com
Phone: +44 20 7946 0958
Card: 4242 4242 4242 4242";
        let before = crate::pin::gdi::text_image(text, 30, [40, 36, 32], [255, 255, 255], 28).expect("render text");
        let src = before.clone();
        let ocr = std::thread::spawn(move || crate::ocr::recognize(&src, None)).join().expect("thread").expect("ocr");
        let hits = redact::find(&ocr, Kinds::ALL);
        assert_eq!(redact::counts(&hits), (1, 1, 1), "recognized: {:?}", ocr.text(true));
        let objs = cover_objects(&hits, RedactStyle::Pixelate, 1, before.width, before.height);
        let after = draw::render(&before, &objs, &draw::RenderOptions::default());
        let gap = 40;
        let mut pair = crate::common::Image::filled(before.width * 2 + gap, before.height, [236, 232, 228, 255]);
        pair.blit(&before, 0, 0);
        pair.blit(&after, (before.width + gap) as i32, 0);
        let p = BeautifyParams {
            backdrop: Backdrop::Solid(crate::effects::Color::rgb(228, 232, 236)),
            padding: 36,
            corner_radius: 0,
            shadow: 0,
            ratio: None,
        };
        save_image_as(&beautify(&pair, &p), &dir.join("hide-private.png"), ImageFormat::Png, 100).expect("save");
    }

    #[test]
    fn selection_limits_what_is_covered() {
        let hits = vec![hit(K::Email, 10.0, 10.0, 40.0, 10.0), hit(K::Phone, 110.0, 10.0, 40.0, 10.0)];
        assert_eq!(inside(hits.clone(), None).len(), 2);
        let left = inside(hits.clone(), Some(Rect::new(0, 0, 100, 100)));
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].kind, K::Email);
        assert!(inside(hits, Some(Rect::new(300, 300, 10, 10))).is_empty());
    }

    #[test]
    fn summary_lists_only_what_was_found() {
        let hits =
            [hit(K::Email, 0.0, 0.0, 1.0, 1.0), hit(K::Email, 0.0, 0.0, 1.0, 1.0), hit(K::Card, 0.0, 0.0, 1.0, 1.0)];
        let s = summary(&hits);
        assert!(s.contains('2') && s.contains('1') && s.contains(", "), "{s}");
        assert!(!s.contains("redact-count"), "missing translation: {s}");
        assert_eq!(summary(&[]), "");
    }
}
