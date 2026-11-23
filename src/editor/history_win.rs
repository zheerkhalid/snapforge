//! Tools ▸ Capture History: a searchable grid of the last captures (`crate::caplog`).
//!
//! The UI thread never touches the disk. While the window is open it has three helpers:
//! * the list is (re)loaded on a short-lived thread whenever `caplog::generation()` moved;
//! * a thumbnail worker decodes and downscales the cards that are visible and have no
//!   texture yet, in the order they are shown (newest first);
//! * a text worker runs OCR on the captures that have no recognized text yet, newest
//!   first, one at a time (Settings ▸ Capture History ▸ index_text), so the search finds
//!   captures by their contents.
//!
//! Both workers stop when the window closes.

use super::bidi;
use super::style::{self, Palette};
use crate::caplog::{self, Entry};
use crate::i18n::{t, tf};
use eframe::egui::{
    self, pos2, vec2, Align, Color32, ColorImage, FontId, Id, Key, Layout, Rect, RichText, Sense, Stroke,
    TextureHandle, TextureOptions, Ui, Vec2,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// What the editor does for the window.
pub enum HistoryEvent {
    /// Open the capture's file in a tab.
    Open(PathBuf),
    Toast {
        text: String,
        error: bool,
    },
}

const CARD: Vec2 = vec2(196.0, 188.0);
const GAP: f32 = 10.0;
const PAD: f32 = 8.0;
const THUMB_H: f32 = 112.0;
/// Longest side of a thumbnail in pixels (sharp up to 150 % scaling).
const THUMB_PX: u32 = 280;
/// Thumbnail textures kept before the ones that are not on screen are released.
const MAX_THUMBS: usize = 240;
/// Height reserved under the grid for the hint and the button row.
const FOOTER_H: f32 = 64.0;

enum Msg {
    /// The list, read when `caplog::generation()` had this value.
    List(u64, Vec<Entry>),
    Thumb(u64, Option<ColorImage>),
    Copied(Result<(), String>),
}

#[derive(Default)]
struct ThumbQueue {
    /// Visible cards without a thumbnail, in display order.
    wanted: Vec<(u64, PathBuf)>,
    busy: Option<u64>,
    /// Decoded (or failed) ids, so a card is not queued again before its result arrived.
    done: HashSet<u64>,
}

/// State shared with the workers.
#[derive(Default)]
struct Work {
    stop: AtomicBool,
    queue: Mutex<ThumbQueue>,
    wake: Condvar,
    /// The text worker is recognizing a capture right now.
    reading: AtomicBool,
}

impl Work {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
}

struct Thumb {
    /// `None` = the file could not be decoded.
    tex: Option<TextureHandle>,
    last_seen: u64,
}

struct Session {
    query: String,
    focus_search: bool,
    entries: Vec<Entry>,
    loaded: bool,
    loading: bool,
    seen_gen: u64,
    /// Indices into `entries` matching `filtered_for`.
    filtered: Vec<usize>,
    filtered_for: Option<String>,
    selected: usize,
    scroll_to_selected: bool,
    confirm_clear: bool,
    thumbs: HashMap<u64, Thumb>,
    frame: u64,
    /// Shown in the hint line (resolved once: it asks the disk about portable mode).
    folder: String,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    work: Arc<Work>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.work.stop.store(true, Ordering::Release);
        self.work.wake.notify_all();
    }
}

#[derive(Clone, Copy)]
enum CardAction {
    Open(usize, bool),
    Copy(usize),
    Reveal(usize),
    Remove(usize),
    Select(usize),
}

/// The Capture History window (an egui window inside the editor).
#[derive(Default)]
pub struct HistoryWindow {
    session: Option<Session>,
}

impl HistoryWindow {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open the window, or just focus its search box when it is open.
    pub fn open(&mut self, ctx: &egui::Context) {
        match self.session.as_mut() {
            Some(s) => s.focus_search = true,
            None => self.session = Some(Session::start(ctx)),
        }
    }

    /// Ctrl+H: open, or close when open.
    pub fn toggle(&mut self, ctx: &egui::Context) {
        if self.session.is_some() {
            self.session = None;
        } else {
            self.open(ctx);
        }
    }

    pub fn ui(&mut self, ctx: &egui::Context, pal: &Palette) -> Vec<HistoryEvent> {
        let mut events = Vec::new();
        let Some(s) = self.session.as_mut() else { return events };
        let mut keep = true;
        s.poll(ctx, &mut events);
        let mut open = true;
        let screen = ctx.screen_rect();
        let size = vec2((screen.width() - 80.0).clamp(360.0, 860.0), (screen.height() - 120.0).clamp(300.0, 600.0));
        egui::Window::new(t("history-title"))
            .id(Id::new("capture-history"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size(size)
            .default_pos(screen.center() - size / 2.0)
            .min_width(340.0)
            .min_height(280.0)
            .show(ctx, |ui| keep = s.contents(ui, pal, &mut events));
        if !(open && keep) {
            self.session = None;
        }
        events
    }
}

impl Session {
    fn start(ctx: &egui::Context) -> Session {
        let (tx, rx) = channel();
        let work = Arc::new(Work::default());
        {
            let (work, tx, ctx) = (work.clone(), tx.clone(), ctx.clone());
            spawn("caplog-thumbs", move || thumb_worker(&work, &tx, &ctx));
        }
        {
            let (work, ctx) = (work.clone(), ctx.clone());
            spawn("caplog-text", move || text_worker(&work, &ctx));
        }
        Session {
            query: String::new(),
            focus_search: true,
            entries: Vec::new(),
            loaded: false,
            loading: false,
            seen_gen: 0,
            filtered: Vec::new(),
            filtered_for: None,
            selected: 0,
            scroll_to_selected: false,
            confirm_clear: false,
            thumbs: HashMap::new(),
            frame: 0,
            folder: caplog::dir().display().to_string(),
            tx,
            rx,
            work,
        }
    }

    /// Results of the workers, and a reload of the list when the history changed.
    fn poll(&mut self, ctx: &egui::Context, events: &mut Vec<HistoryEvent>) {
        self.frame += 1;
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::List(gen, list) => {
                    self.loading = false;
                    self.loaded = true;
                    self.seen_gen = gen;
                    let ids: HashSet<u64> = list.iter().map(|e| e.id).collect();
                    self.thumbs.retain(|id, _| ids.contains(id));
                    let selected_id = self.selected_entry().map(|e| e.id);
                    self.entries = list;
                    self.filtered_for = None;
                    self.refilter();
                    if let Some(pos) =
                        selected_id.and_then(|id| self.filtered.iter().position(|&i| self.entries[i].id == id))
                    {
                        self.selected = pos;
                    }
                }
                Msg::Thumb(id, image) => {
                    let tex = image.map(|img| ctx.load_texture("caplog-thumb", img, TextureOptions::LINEAR));
                    self.thumbs.insert(id, Thumb { tex, last_seen: self.frame });
                }
                Msg::Copied(Ok(())) => events.push(HistoryEvent::Toast { text: t("notify-copied"), error: false }),
                Msg::Copied(Err(e)) => {
                    events.push(HistoryEvent::Toast { text: tf("editor-copy-failed", &[("error", &e)]), error: true })
                }
            }
        }
        if !self.loading && caplog::generation() != self.seen_gen {
            self.loading = true;
            let (tx, ctx) = (self.tx.clone(), ctx.clone());
            spawn("caplog-list", move || {
                let gen = caplog::generation();
                let list = caplog::entries();
                let _ = tx.send(Msg::List(gen, list));
                ctx.request_repaint();
            });
        }
        // Captures taken while the window is open show up without any input.
        ctx.request_repaint_after(Duration::from_millis(700));
    }

    fn selected_entry(&self) -> Option<&Entry> {
        self.filtered.get(self.selected).and_then(|&i| self.entries.get(i))
    }

    fn refilter(&mut self) {
        if self.filtered_for.as_deref() != Some(self.query.as_str()) {
            self.filtered = caplog::search(&self.entries, &self.query);
            self.filtered_for = Some(self.query.clone());
        }
        self.selected = self.selected.min(self.filtered.len().saturating_sub(1));
    }

    fn status(&self) -> String {
        if !self.loaded {
            return t("history-loading");
        }
        let total = self.entries.len();
        let count = total.to_string();
        let pending = self.entries.iter().filter(|e| e.text.is_none()).count();
        if pending > 0 && self.work.reading.load(Ordering::Acquire) {
            let done = (total - pending + 1).to_string();
            return tf("history-status-reading", &[("done", &done), ("count", &count)]);
        }
        if !self.query.trim().is_empty() {
            let shown = self.filtered.len().to_string();
            return tf("history-status-filtered", &[("shown", &shown), ("count", &count)]);
        }
        match total {
            1 => t("history-status-one"),
            _ => tf("history-status-count", &[("count", &count)]),
        }
    }

    /// Returns false when the window should close.
    fn contents(&mut self, ui: &mut Ui, pal: &Palette, events: &mut Vec<HistoryEvent>) -> bool {
        let mut keep = true;
        let edit = egui::TextEdit::singleline(&mut self.query)
            .hint_text(t("history-search-hint"))
            .desired_width(f32::INFINITY)
            .font(FontId::proportional(15.0))
            .margin(egui::Margin::symmetric(8, 5));
        let search = ui.add(edit);
        if self.focus_search {
            search.request_focus();
            self.focus_search = false;
        }
        if search.changed() {
            self.selected = 0;
            self.scroll_to_selected = true;
        }
        self.refilter();

        let (enabled, _) = history_settings();
        ui.horizontal(|ui| {
            ui.label(RichText::new(self.status()).color(pal.muted_text));
            if !enabled {
                ui.label(RichText::new(t("history-status-off")).color(pal.dirty));
            }
        });
        ui.add_space(2.0);

        let mut actions = Vec::new();
        let grid_h = (ui.available_height() - FOOTER_H).max(100.0);
        let avail_w = ui.available_width();
        let cols = (((avail_w + GAP) / (CARD.x + GAP)).floor() as usize).max(1);

        // Keyboard, while the search box has the focus (Enter makes a single-line box give
        // it up in that very frame): arrows move the selection by a row, Enter opens it.
        let keys_here = search.has_focus() || search.lost_focus();
        if keys_here && !self.filtered.is_empty() {
            let (down, up, enter) =
                ui.input(|i| (i.key_pressed(Key::ArrowDown), i.key_pressed(Key::ArrowUp), i.key_pressed(Key::Enter)));
            let last = self.filtered.len() - 1;
            if down {
                self.selected = (self.selected + cols).min(last);
                self.scroll_to_selected = true;
            }
            if up {
                self.selected = self.selected.saturating_sub(cols);
                self.scroll_to_selected = true;
            }
            if enter {
                actions.push(CardAction::Open(self.selected, ui.input(|i| i.modifiers.ctrl)));
            }
        }

        let mut want: Vec<(u64, PathBuf)> = Vec::new();
        egui::ScrollArea::vertical()
            .id_salt("caplog-grid")
            .auto_shrink([false, false])
            .max_height(grid_h)
            .show_viewport(ui, |ui, viewport| {
                if self.filtered.is_empty() {
                    if self.loaded {
                        let key = if self.entries.is_empty() { "history-empty" } else { "history-no-match" };
                        ui.add_space(40.0);
                        ui.vertical_centered(|ui| ui.label(RichText::new(t(key)).color(pal.muted_text)));
                    }
                    return;
                }
                let rows = self.filtered.len().div_ceil(cols);
                let pitch = CARD.y + GAP;
                let total_h = rows as f32 * pitch - GAP;
                let (area, _) = ui.allocate_exact_size(vec2(ui.available_width(), total_h), Sense::hover());
                let grid_w = cols as f32 * (CARD.x + GAP) - GAP;
                let origin = area.min + vec2(((area.width() - grid_w) / 2.0).max(0.0).floor(), 0.0);
                let card_rect = |pos: usize| {
                    let (row, col) = (pos / cols, pos % cols);
                    Rect::from_min_size(origin + vec2(col as f32 * (CARD.x + GAP), row as f32 * pitch), CARD)
                };
                if std::mem::take(&mut self.scroll_to_selected) {
                    ui.scroll_to_rect(card_rect(self.selected).expand(GAP), None);
                }
                let first = ((viewport.min.y / pitch).floor().max(0.0) as usize).min(rows - 1);
                let last = ((viewport.max.y / pitch).ceil().max(1.0) as usize).min(rows);
                for pos in first * cols..(last * cols).min(self.filtered.len()) {
                    let entry = &self.entries[self.filtered[pos]];
                    let thumb = self.thumbs.get_mut(&entry.id).map(|th| {
                        th.last_seen = self.frame;
                        th.tex.clone()
                    });
                    if thumb.is_none() {
                        want.push((entry.id, entry.path.clone()));
                    }
                    card(ui, pal, card_rect(pos), pos, entry, thumb, pos == self.selected, &mut actions);
                }
            });
        self.request_thumbs(want);

        ui.separator();
        ui.allocate_ui_with_layout(vec2(avail_w, 18.0), Layout::left_to_right(Align::Center), |ui| {
            let hint = tf("history-hint", &[("folder", &self.folder)]);
            ui.add(egui::Label::new(RichText::new(&hint).small().color(pal.muted_text)).truncate()).on_hover_text(hint);
        });
        ui.allocate_ui_with_layout(vec2(avail_w, 26.0), Layout::right_to_left(Align::Center), |ui| {
            if self.confirm_clear {
                if ui.button(t("editor-cancel")).clicked() {
                    self.confirm_clear = false;
                }
                let yes =
                    egui::Button::new(RichText::new(t("history-clear-yes")).color(Color32::WHITE)).fill(pal.error);
                if ui.add(yes).clicked() {
                    self.confirm_clear = false;
                    self.entries.clear();
                    self.filtered.clear();
                    self.filtered_for = None;
                    self.thumbs.clear();
                    spawn("caplog-clear", caplog::clear);
                }
                ui.add(egui::Label::new(t("history-clear-confirm")).truncate());
            } else if ui.add_enabled(!self.entries.is_empty(), egui::Button::new(t("history-clear"))).clicked() {
                self.confirm_clear = true;
            }
        });

        for action in actions {
            keep &= self.apply(action, events);
        }
        keep
    }

    /// Returns false when the window should close (a capture was opened).
    fn apply(&mut self, action: CardAction, events: &mut Vec<HistoryEvent>) -> bool {
        let pos = match action {
            CardAction::Open(p, _)
            | CardAction::Copy(p)
            | CardAction::Reveal(p)
            | CardAction::Remove(p)
            | CardAction::Select(p) => p,
        };
        let Some(&index) = self.filtered.get(pos) else { return true };
        let Some(entry) = self.entries.get(index) else { return true };
        self.selected = pos;
        match action {
            CardAction::Select(_) => {}
            CardAction::Open(_, stay) => {
                events.push(HistoryEvent::Open(entry.path.clone()));
                // Ctrl keeps the window open to pick more captures.
                return stay;
            }
            CardAction::Copy(_) => {
                let (path, tx) = (entry.path.clone(), self.tx.clone());
                spawn("caplog-copy", move || {
                    let result = crate::output::save::load_image(&path)
                        .and_then(|img| crate::output::clipboard::copy_image(&img))
                        .map_err(|e| format!("{e:#}"));
                    let _ = tx.send(Msg::Copied(result));
                });
            }
            CardAction::Reveal(_) => crate::common::win::reveal_in_explorer(&entry.path),
            CardAction::Remove(_) => {
                let id = entry.id;
                self.entries.remove(index);
                self.thumbs.remove(&id);
                self.filtered_for = None;
                self.refilter();
                spawn("caplog-remove", move || caplog::remove(id));
            }
        }
        true
    }

    /// Tell the thumbnail worker which cards are visible and still without a picture.
    fn request_thumbs(&mut self, want: Vec<(u64, PathBuf)>) {
        let frame = self.frame;
        let mut released = Vec::new();
        if self.thumbs.len() > MAX_THUMBS {
            self.thumbs.retain(|id, th| {
                let keep = th.last_seen == frame;
                if !keep {
                    released.push(*id);
                }
                keep
            });
        }
        let Ok(mut q) = self.work.queue.lock() else { return };
        for id in released {
            q.done.remove(&id);
        }
        let want: Vec<(u64, PathBuf)> =
            want.into_iter().filter(|(id, _)| q.busy != Some(*id) && !q.done.contains(id)).collect();
        if q.wanted != want {
            q.wanted = want;
            self.work.wake.notify_all();
        }
    }
}

fn history_settings() -> (bool, bool) {
    let s = crate::settings::get();
    (s.history.enabled, s.history.index_text)
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    if let Err(e) = std::thread::Builder::new().name(name.into()).spawn(f) {
        log::warn!("history: could not start {name}: {e}");
    }
}

// ---------------------------------------------------------------- cards

#[allow(clippy::too_many_arguments)]
fn card(
    ui: &mut Ui,
    pal: &Palette,
    rect: Rect,
    pos: usize,
    entry: &Entry,
    thumb: Option<Option<TextureHandle>>,
    selected: bool,
    actions: &mut Vec<CardAction>,
) {
    let resp = ui.interact(rect, ui.id().with(("caplog-card", entry.id)), Sense::click());
    let fill = if selected {
        style::ACCENT.gamma_multiply(if pal.dark { 0.35 } else { 0.14 })
    } else if resp.hovered() {
        pal.tab_hover
    } else {
        pal.tab_active
    };
    let border = if selected { style::ACCENT } else { pal.border };
    let painter = ui.painter();
    painter.rect_filled(rect, 8.0, fill);
    painter.rect_stroke(rect, 8.0, Stroke::new(1.0_f32, border), egui::StrokeKind::Inside);

    let frame = Rect::from_min_size(rect.min + vec2(PAD, PAD), vec2(rect.width() - 2.0 * PAD, THUMB_H));
    painter.rect_filled(frame, 4.0, pal.canvas);
    match &thumb {
        Some(Some(tex)) => {
            let fitted = fit(tex.size_vec2(), frame.shrink(2.0));
            painter.image(tex.id(), fitted, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        Some(None) => {
            painter.text(frame.center(), egui::Align2::CENTER_CENTER, "⚠", FontId::proportional(22.0), pal.muted_text);
        }
        None => {
            painter.text(frame.center(), egui::Align2::CENTER_CENTER, "…", FontId::proportional(22.0), pal.muted_text);
        }
    }

    let strong = ui.visuals().strong_text_color();
    let line =
        |n: f32| Rect::from_min_size(pos2(frame.left(), frame.bottom() + 6.0 + n * 19.0), vec2(frame.width(), 18.0));
    let when = entry.when();
    let details = format!("{} × {}  ·  {}", entry.width, entry.height, entry.mode_label());
    let name = if entry.owned { t("history-unsaved") } else { entry.file_name() };
    text_line(ui, line(0.0), &when, 13.5, strong);
    text_line(ui, line(1.0), &details, 12.0, pal.muted_text);
    text_line(ui, line(2.0), &name, 12.0, if entry.owned { pal.muted_text } else { ui.visuals().text_color() });

    let resp = resp.on_hover_ui(|ui| {
        ui.set_max_width(360.0);
        bidi::label(ui, &name, 13.0, true, strong);
        ui.label(RichText::new(format!("{when}  ·  {details}")).color(pal.muted_text));
        if matches!(thumb, Some(None)) {
            ui.label(RichText::new(t("history-missing")).color(pal.error));
        }
        // The first lines of the recognized text show why a search found the capture.
        for text in entry.text.iter().flat_map(|s| s.lines()).filter(|l| !l.trim().is_empty()).take(3) {
            let short: String = text.chars().take(90).collect();
            bidi::label(ui, &short, 12.0, false, pal.muted_text);
        }
    });
    if resp.clicked() {
        actions.push(CardAction::Open(pos, ui.input(|i| i.modifiers.ctrl)));
    }
    if resp.secondary_clicked() {
        actions.push(CardAction::Select(pos));
    }
    resp.context_menu(|ui| {
        let mut item = |ui: &mut Ui, key: &str, action: CardAction| {
            if ui.button(t(key)).clicked() {
                actions.push(action);
                ui.close_menu();
            }
        };
        item(ui, "history-open", CardAction::Open(pos, false));
        item(ui, "history-copy", CardAction::Copy(pos));
        item(ui, "history-show", CardAction::Reveal(pos));
        ui.separator();
        item(ui, "history-remove", CardAction::Remove(pos));
    });
}

/// One line of text cut to `rect`; right-to-left names are drawn by Windows.
fn text_line(ui: &Ui, rect: Rect, text: &str, size: f32, color: Color32) {
    if bidi::needs_native(text) && bidi::paint(ui, rect, text, size, false, color, false).is_some() {
        return;
    }
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), FontId::proportional(size), color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(rect.width());
    let galley = ui.fonts(|f| f.layout_job(job));
    ui.painter().galley(pos2(rect.left(), rect.center().y - galley.size().y / 2.0), galley, color);
}

/// The largest rectangle with the proportions of `size` centred in `frame` (never enlarged
/// beyond the texture's own size).
fn fit(size: Vec2, frame: Rect) -> Rect {
    if size.x <= 0.0 || size.y <= 0.0 {
        return Rect::from_center_size(frame.center(), Vec2::ZERO);
    }
    let k = (frame.width() / size.x).min(frame.height() / size.y).min(1.0);
    Rect::from_center_size(frame.center(), size * k)
}

// ---------------------------------------------------------------- workers

/// The picture of a card: the top of very tall captures (scrolling captures), downscaled.
fn thumbnail(img: &crate::common::Image) -> ColorImage {
    let (w, h) = (img.width, img.height);
    let cropped;
    let src = if h > w.saturating_mul(2) {
        cropped = img.crop(crate::common::Rect::new(0, 0, w as i32, (w.saturating_mul(2)) as i32));
        &cropped
    } else if w > h.saturating_mul(4) {
        cropped = img.crop(crate::common::Rect::new(0, 0, (h.saturating_mul(4)) as i32, h as i32));
        &cropped
    } else {
        img
    };
    let (small, _) = super::effects::downscale_proxy(src, THUMB_PX);
    let (tw, th) = (small.width as usize, small.height as usize);
    let mut pixels = vec![Color32::TRANSPARENT; tw * th];
    super::texture::swizzle_row(&small.data, &mut pixels);
    ColorImage { size: [tw, th], pixels }
}

fn load_thumbnail(path: &Path) -> Option<ColorImage> {
    let img = crate::output::save::load_image(path).ok()?;
    (!img.is_empty()).then(|| thumbnail(&img))
}

fn thumb_worker(work: &Work, tx: &Sender<Msg>, ctx: &egui::Context) {
    loop {
        let (id, path) = {
            let Ok(mut q) = work.queue.lock() else { return };
            loop {
                if work.stopped() {
                    return;
                }
                if !q.wanted.is_empty() {
                    let next = q.wanted.remove(0);
                    q.busy = Some(next.0);
                    break next;
                }
                q = match work.wake.wait(q) {
                    Ok(q) => q,
                    Err(_) => return,
                };
            }
        };
        let image = load_thumbnail(&path);
        if let Ok(mut q) = work.queue.lock() {
            q.busy = None;
            q.done.insert(id);
        }
        if tx.send(Msg::Thumb(id, image)).is_err() {
            return;
        }
        ctx.request_repaint();
    }
}

/// Recognize the text of the captures that have none yet, newest first, one at a time.
fn text_worker(work: &Work, ctx: &egui::Context) {
    // Captures the recognizer failed on are not retried in this session.
    let mut failed: HashSet<u64> = HashSet::new();
    let mut failures_in_a_row = 0;
    while !work.stopped() {
        let gen = caplog::generation();
        let next = if history_settings().1 {
            caplog::entries().into_iter().find(|e| e.text.is_none() && !failed.contains(&e.id))
        } else {
            None
        };
        let Some(entry) = next else {
            // Nothing to do until the history or the setting changes.
            let on = history_settings().1;
            while !work.stopped() && caplog::generation() == gen && history_settings().1 == on {
                std::thread::sleep(Duration::from_millis(150));
            }
            continue;
        };
        work.reading.store(true, Ordering::Release);
        ctx.request_repaint();
        match crate::output::save::load_image(&entry.path) {
            // A file that cannot be decoded has no text; do not look at it again.
            Err(_) => caplog::set_text(entry.id, ""),
            Ok(img) => match crate::ocr::recognize(&img, None) {
                Ok(result) => {
                    failures_in_a_row = 0;
                    caplog::set_text(entry.id, result.text(true).trim());
                }
                Err(e) => {
                    log::debug!("history: text recognition failed: {e:#}");
                    failed.insert(entry.id);
                    failures_in_a_row += 1;
                }
            },
        }
        work.reading.store(false, Ordering::Release);
        ctx.request_repaint();
        if failures_in_a_row >= 3 {
            // No recognizer for the user's languages, most likely: stop trying.
            log::info!("history: text recognition is not available; captures are not indexed by text");
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::Image;

    #[test]
    fn thumbnails_are_small_and_keep_the_colours() {
        // Premultiplied BGRA: opaque blue = [255, 0, 0, 255].
        let img = Image::filled(1400, 700, [255, 0, 0, 255]);
        let th = thumbnail(&img);
        assert_eq!(th.size, [280, 140]);
        assert_eq!(th.pixels[0], Color32::from_rgb(0, 0, 255));
        let small = thumbnail(&Image::filled(40, 30, [0, 255, 0, 255]));
        assert_eq!(small.size, [40, 30], "small captures are not enlarged");
        assert_eq!(small.pixels[5], Color32::from_rgb(0, 255, 0));
    }

    #[test]
    fn very_long_captures_show_their_start() {
        let tall = thumbnail(&Image::filled(400, 12_000, [0, 0, 0, 255]));
        assert_eq!(tall.size, [140, 280]);
        let wide = thumbnail(&Image::filled(9000, 100, [0, 0, 0, 255]));
        assert_eq!(wide.size, [280, 70]);
    }

    #[test]
    fn thumbnails_fit_inside_the_card() {
        let frame = Rect::from_min_size(pos2(10.0, 10.0), vec2(180.0, 112.0));
        let wide = fit(vec2(280.0, 70.0), frame);
        assert!((wide.width() - 180.0).abs() < 0.01 && (wide.height() - 45.0).abs() < 0.01);
        assert_eq!(wide.center(), frame.center());
        let tall = fit(vec2(140.0, 280.0), frame);
        assert!((tall.height() - 112.0).abs() < 0.01 && (tall.width() - 56.0).abs() < 0.01);
        let tiny = fit(vec2(40.0, 30.0), frame);
        assert_eq!(tiny.size(), vec2(40.0, 30.0));
        assert_eq!(fit(vec2(0.0, 0.0), frame).size(), Vec2::ZERO);
    }

    #[test]
    fn the_thumbnail_worker_serves_the_queue_in_order_and_stops() {
        let dir = std::env::temp_dir().join(format!("snapforge-history-win-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let good = dir.join("a.png");
        crate::output::save::save_image_as(
            &Image::filled(64, 48, [0, 0, 255, 255]),
            &good,
            crate::settings::ImageFormat::Png,
            100,
        )
        .expect("save");
        let work = Arc::new(Work::default());
        let (tx, rx) = channel();
        let ctx = egui::Context::default();
        let handle = {
            let (work, ctx) = (work.clone(), ctx.clone());
            std::thread::spawn(move || thumb_worker(&work, &tx, &ctx))
        };
        {
            let mut q = work.queue.lock().expect("lock");
            q.wanted = vec![(7, good.clone()), (8, dir.join("missing.png"))];
        }
        work.wake.notify_all();
        let wait = Duration::from_secs(20);
        match rx.recv_timeout(wait).expect("first result") {
            Msg::Thumb(7, Some(img)) => assert_eq!(img.size, [64, 48]),
            _ => panic!("expected the thumbnail of the first card"),
        }
        match rx.recv_timeout(wait).expect("second result") {
            Msg::Thumb(8, None) => {}
            _ => panic!("expected a failure for the missing file"),
        }
        {
            let q = work.queue.lock().expect("lock");
            assert!(q.wanted.is_empty() && q.busy.is_none());
            assert!(q.done.contains(&7) && q.done.contains(&8));
        }
        work.stop.store(true, Ordering::Release);
        work.wake.notify_all();
        handle.join().expect("worker stops");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every Fluent key this window uses exists in the English file.
    #[test]
    fn all_strings_are_localized() {
        let ftl = crate::i18n::parse_ftl(include_str!("../../locales/en-US/main.ftl"));
        let src = include_str!("history_win.rs");
        let mut keys: Vec<String> = Vec::new();
        for prefix in ["\"history-", "\"editor-", "\"notify-"] {
            for (i, _) in src.match_indices(prefix) {
                let rest = &src[i + 1..];
                let k = &rest[..rest.find('"').unwrap_or(rest.len())];
                if !(k.ends_with('-') || k.contains('{')) {
                    keys.push(k.to_string());
                }
            }
        }
        keys.extend(["history-title", "editor-tool-history", "menu-history", "settings-tab-history"].map(String::from));
        keys.extend(crate::settings::CaptureMode::ALL.iter().map(|m| format!("history-mode-{}", m.id())));
        // The Settings page is generic: its labels are looked up from the field names.
        match toml::Value::try_from(crate::settings::history::HistorySettings::default()) {
            Ok(toml::Value::Table(fields)) => {
                assert_eq!(fields.len(), 3);
                keys.extend(fields.keys().map(|f| format!("settings-field-history-{}", f.replace('_', "-"))));
            }
            _ => panic!("the history settings are a table"),
        }
        let missing: Vec<&String> = keys.iter().filter(|k| !ftl.contains_key(k.as_str())).collect();
        assert!(missing.is_empty(), "missing in main.ftl: {missing:?}");
        assert!(keys.len() > 20);
    }
}
