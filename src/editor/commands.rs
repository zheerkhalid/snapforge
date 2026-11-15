//! The command registry: every menu item, toolbar button, shortcut and command-palette
//! entry is one [`Command`] (id, label, shortcut, category, enabled predicate, action),
//! so they can never disagree. Also shortcut parsing and the fuzzy matcher.

use super::document::DocId;
use eframe::egui::{Key, Modifiers};
use std::collections::HashMap;

// ---------------------------------------------------------------- actions

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoomCmd {
    In,
    Out,
    Actual,
    FitWindow,
    FitWidth,
    Selection,
    /// Percent (1–5000).
    Set(u32),
}

/// The built-in canvas tools (Phase 3 tools are addressed by id).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolId {
    Pan,
    SelectRect,
    SelectFree,
    External(String),
}

/// Everything the UI can ask for; executed at the end of the frame. Variants without a
/// document id act on the active tab.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    // file
    Open,
    Save,
    SaveDoc(DocId),
    SaveAs,
    SaveAll,
    SaveAllSequence,
    CloseTab,
    Close(DocId),
    /// Close without asking (after "Don't Save").
    Discard(DocId),
    CloseSaved,
    DeleteFile,
    Properties,
    OpenFolder,
    OpenRecent(Vec<String>),
    ClearRecent,
    EditWith(usize),
    ConfigureEditors,
    Scan,
    Print,
    Continue,
    Hide,
    // edit
    Undo,
    Redo,
    /// Undo / redo several steps at once (Edit menu history list).
    UndoMany(usize),
    RedoMany(usize),
    Copy,
    Cut,
    Paste,
    PasteNewTab,
    SelectAll,
    Deselect,
    DeleteSelection,
    CropToSelection,
    SendToNewTab,
    PinToScreen,
    Tool(ToolId),
    // image / effects
    Effect(String),
    // tools
    CombineImages,
    ImagesToPdf,
    BatchPrint,
    Palette,
    // view
    Zoom(ZoomCmd),
    Activate(DocId),
    NextTab,
    PrevTab,
    ToggleWorkspaces,
    NewWorkspace,
    SwitchWorkspace(u64),
    CloseWorkspace(u64),
    RenameWorkspace(u64),
    // help
    About,
    Shortcuts,
}

// ---------------------------------------------------------------- shortcuts

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Shortcut {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub key: Key,
}

impl Shortcut {
    /// Parse `"Ctrl+Shift+Z"`, `"Alt+S"`, `"Ctrl+Del"`, `"Z"`, `"Ctrl+0"`, `"F1"`.
    /// Case-insensitive modifiers; `Cmd` is accepted for Ctrl.
    pub fn parse(s: &str) -> Option<Shortcut> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        // Split on '+' but allow a literal "+" key at the end ("Ctrl++").
        let (mods_part, key_part) = if let Some(stripped) = s.strip_suffix("++") {
            (stripped, "+")
        } else if s == "+" {
            ("", "+")
        } else {
            match s.rfind('+') {
                Some(i) => (&s[..i], &s[i + 1..]),
                None => ("", s),
            }
        };
        let mut sc = Shortcut { ctrl: false, shift: false, alt: false, key: parse_key(key_part.trim())? };
        for m in mods_part.split('+').map(str::trim).filter(|m| !m.is_empty()) {
            match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "cmd" | "command" => sc.ctrl = true,
                "shift" => sc.shift = true,
                "alt" | "option" => sc.alt = true,
                _ => return None,
            }
        }
        Some(sc)
    }

    pub fn modifiers(&self) -> Modifiers {
        let mut m = Modifiers::NONE;
        if self.ctrl {
            m |= Modifiers::COMMAND;
        }
        if self.shift {
            m |= Modifiers::SHIFT;
        }
        if self.alt {
            m |= Modifiers::ALT;
        }
        m
    }

    /// Exact match against an event's modifiers (no extra modifiers allowed).
    pub fn matches(&self, key: Key, m: Modifiers) -> bool {
        key == self.key && (m.ctrl || m.command) == self.ctrl && m.shift == self.shift && m.alt == self.alt
    }

    /// Display text, e.g. `Ctrl+Shift+Z`.
    pub fn label(&self) -> String {
        let mut s = String::new();
        if self.ctrl {
            s.push_str("Ctrl+");
        }
        if self.shift {
            s.push_str("Shift+");
        }
        if self.alt {
            s.push_str("Alt+");
        }
        s.push_str(key_label(self.key));
        s
    }
}

fn parse_key(k: &str) -> Option<Key> {
    let alias = match k.to_ascii_lowercase().as_str() {
        "del" => "Delete",
        "ins" => "Insert",
        "pgup" => "PageUp",
        "pgdn" | "pgdown" => "PageDown",
        "esc" => "Escape",
        "space" => "Space",
        "enter" | "return" => "Enter",
        "tab" => "Tab",
        "home" => "Home",
        "end" => "End",
        "backspace" => "Backspace",
        "plus" => "Plus",
        "minus" => "Minus",
        _ => "",
    };
    if !alias.is_empty() {
        return Key::from_name(alias);
    }
    if k.len() == 1 {
        let c = k.chars().next()?;
        if c.is_ascii_alphabetic() {
            return Key::from_name(&c.to_ascii_uppercase().to_string());
        }
    }
    let upper = k.to_ascii_uppercase();
    Key::from_name(&upper).or_else(|| Key::from_name(k))
}

fn key_label(k: Key) -> &'static str {
    match k {
        Key::Delete => "Del",
        Key::Escape => "Esc",
        Key::Plus => "+",
        Key::Minus => "-",
        Key::Equals => "=",
        Key::PageUp => "PgUp",
        Key::PageDown => "PgDn",
        other => other.name(),
    }
}

// ---------------------------------------------------------------- registry

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Category {
    File,
    Edit,
    Select,
    Image,
    Effects,
    Tools,
    View,
    Help,
    Recent,
}

impl Category {
    pub fn label_key(self) -> &'static str {
        match self {
            Category::File => "editor-file",
            Category::Edit => "editor-menu-edit",
            Category::Select => "editor-cat-select",
            Category::Image => "editor-menu-image",
            Category::Effects => "editor-menu-effects",
            Category::Tools => "editor-menu-tools",
            Category::View => "editor-menu-view",
            Category::Help => "editor-menu-help",
            Category::Recent => "editor-recent",
        }
    }
}

/// What the enabled predicates can look at, rebuilt once per frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CmdState {
    pub has_doc: bool,
    pub has_selection: bool,
    pub can_undo: bool,
    pub can_redo: bool,
    pub has_path: bool,
    pub any_dirty: bool,
    pub multiple_docs: bool,
    pub busy: bool,
    pub can_continue: bool,
    pub workspaces: bool,
}

pub type Enabled = fn(&CmdState) -> bool;

pub fn always(_: &CmdState) -> bool {
    true
}
pub fn has_doc(s: &CmdState) -> bool {
    s.has_doc
}
pub fn editable(s: &CmdState) -> bool {
    s.has_doc && !s.busy
}
pub fn has_selection(s: &CmdState) -> bool {
    s.has_doc && s.has_selection && !s.busy
}
pub fn can_undo(s: &CmdState) -> bool {
    s.can_undo
}
pub fn can_redo(s: &CmdState) -> bool {
    s.can_redo
}
pub fn has_path(s: &CmdState) -> bool {
    s.has_doc && s.has_path
}
pub fn any_dirty(s: &CmdState) -> bool {
    s.any_dirty
}
pub fn multiple_docs(s: &CmdState) -> bool {
    s.multiple_docs
}
pub fn can_continue(s: &CmdState) -> bool {
    s.can_continue
}
pub fn workspaces_on(s: &CmdState) -> bool {
    s.workspaces
}

#[derive(Clone)]
pub struct Command {
    pub id: String,
    pub label: String,
    pub category: Category,
    pub shortcut: Option<Shortcut>,
    /// Extra bindings (e.g. Ctrl+Shift+Z for redo); not shown in menus.
    pub alt_shortcuts: Vec<Shortcut>,
    pub icon: &'static str,
    pub enabled: Enabled,
    pub action: Action,
    /// Extra words the palette matches (not shown).
    pub keywords: String,
    /// Shown in the palette (recent files, effects); false hides internal commands.
    pub in_palette: bool,
}

impl Command {
    pub fn new(id: impl Into<String>, label: impl Into<String>, category: Category, action: Action) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            category,
            shortcut: None,
            alt_shortcuts: Vec::new(),
            icon: "",
            enabled: always,
            action,
            keywords: String::new(),
            in_palette: true,
        }
    }
    pub fn key(mut self, s: &str) -> Self {
        self.shortcut = Shortcut::parse(s);
        debug_assert!(self.shortcut.is_some(), "bad shortcut {s}");
        self
    }
    pub fn alt_key(mut self, s: &str) -> Self {
        self.alt_shortcuts.extend(Shortcut::parse(s));
        self
    }
    pub fn icon(mut self, i: &'static str) -> Self {
        self.icon = i;
        self
    }
    pub fn when(mut self, f: Enabled) -> Self {
        self.enabled = f;
        self
    }
    pub fn keywords(mut self, k: &str) -> Self {
        self.keywords = k.to_string();
        self
    }
    pub fn hidden(mut self) -> Self {
        self.in_palette = false;
        self
    }
    pub fn shortcut_text(&self) -> String {
        self.shortcut.map(|s| s.label()).unwrap_or_default()
    }
    /// Tooltip: "Label (Shortcut)".
    pub fn tooltip(&self) -> String {
        match self.shortcut {
            Some(s) => format!("{} ({})", self.label.trim_end_matches('…'), s.label()),
            None => self.label.trim_end_matches('…').to_string(),
        }
    }
}

#[derive(Clone, Default)]
pub struct CommandRegistry {
    cmds: Vec<Command>,
    index: HashMap<String, usize>,
}

impl CommandRegistry {
    /// Add or replace (same id) a command.
    pub fn register(&mut self, c: Command) {
        match self.index.get(&c.id) {
            Some(&i) => self.cmds[i] = c,
            None => {
                self.index.insert(c.id.clone(), self.cmds.len());
                self.cmds.push(c);
            }
        }
    }

    pub fn get(&self, id: &str) -> Option<&Command> {
        self.index.get(id).map(|&i| &self.cmds[i])
    }

    pub fn iter(&self) -> impl Iterator<Item = &Command> {
        self.cmds.iter()
    }

    pub fn len(&self) -> usize {
        self.cmds.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cmds.is_empty()
    }

    /// The command bound to this key press (exact modifiers), if any.
    pub fn for_key(&self, key: Key, m: Modifiers) -> Option<&Command> {
        self.cmds.iter().find(|c| c.shortcut.iter().chain(&c.alt_shortcuts).any(|s| s.matches(key, m)))
    }

    /// Pairs of commands that share a shortcut (a registration bug).
    pub fn conflicts(&self) -> Vec<(String, String)> {
        let mut seen: HashMap<Shortcut, &str> = HashMap::new();
        let mut out = Vec::new();
        for c in &self.cmds {
            for s in c.shortcut.iter().chain(&c.alt_shortcuts) {
                if let Some(prev) = seen.insert(*s, &c.id) {
                    out.push((prev.to_string(), c.id.clone()));
                }
            }
        }
        out
    }

    /// Palette search: enabled-or-not commands ranked by fuzzy score on the label
    /// (and keywords / category, weaker).
    pub fn search(&self, query: &str, limit: usize) -> Vec<&Command> {
        let q = query.trim();
        let mut scored: Vec<(i32, usize)> = self
            .cmds
            .iter()
            .enumerate()
            .filter(|(_, c)| c.in_palette)
            .filter_map(|(i, c)| {
                if q.is_empty() {
                    return Some((0, i));
                }
                let label = fuzzy_score(q, &c.label);
                let extra = fuzzy_score(q, &c.keywords).map(|s| s / 2);
                label.into_iter().chain(extra).max().map(|s| (s, i))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.into_iter().take(limit).map(|(_, i)| &self.cmds[i]).collect()
    }
}

// ---------------------------------------------------------------- fuzzy matching

/// Fuzzy subsequence score of `query` in `text` (case-insensitive), `None` if not all
/// query characters appear in order. Higher is better: matches at word starts and
/// consecutive runs score more, gaps and a late first match cost a little, and a
/// shorter text wins ties.
pub fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    let q: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_lowercase).collect();
    if q.is_empty() {
        return Some(0);
    }
    let t: Vec<char> = text.chars().collect();
    let tl: Vec<char> = t.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect();
    let n = t.len();
    if q.len() > n {
        return None;
    }
    let word_start = |i: usize| -> bool {
        if i == 0 {
            return true;
        }
        let (p, c) = (t[i - 1], t[i]);
        !p.is_alphanumeric() || (p.is_lowercase() && c.is_uppercase()) || (p.is_alphabetic() && c.is_ascii_digit())
    };
    // DP over (query index, text index): best score ending with q[qi] matched at t[ti].
    const NEG: i32 = i32::MIN / 2;
    let mut prev = vec![NEG; n];
    for (qi, &qc) in q.iter().enumerate() {
        let mut cur = vec![NEG; n];
        // Best of prev[k] - gap(k..ti) for k < ti, tracked incrementally.
        let mut best_before = NEG;
        for ti in 0..n {
            if ti > 0 && qi > 0 {
                // Extend the running best by one gap character.
                let cand = prev[ti - 1];
                best_before = if best_before == NEG { cand } else { (best_before - 1).max(cand) };
            }
            if tl[ti] != qc {
                continue;
            }
            let mut s = 16;
            if word_start(ti) {
                s += 24;
            }
            if qi == 0 {
                // Earlier first match is better; a match at the very start best.
                s -= (ti as i32).min(12);
                cur[ti] = s;
            } else {
                let consecutive = if ti > 0 && prev[ti - 1] > NEG { prev[ti - 1] + 20 } else { NEG };
                let gapped = if best_before > NEG { best_before - 2 } else { NEG };
                let base = consecutive.max(gapped);
                if base > NEG {
                    cur[ti] = base + s;
                }
            }
        }
        prev = cur;
    }
    let best = prev.into_iter().max().filter(|&s| s > NEG)?;
    Some(best - (n as i32 - q.len() as i32).min(30) / 3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_shortcuts() {
        let s = Shortcut::parse("Ctrl+Shift+Z").expect("parse");
        assert!(s.ctrl && s.shift && !s.alt && s.key == Key::Z);
        assert_eq!(s.label(), "Ctrl+Shift+Z");
        assert_eq!(Shortcut::parse("alt+s").map(|s| s.label()), Some("Alt+S".into()));
        assert_eq!(Shortcut::parse("Ctrl+Del").map(|s| s.key), Some(Key::Delete));
        assert_eq!(Shortcut::parse("Ctrl+0").map(|s| s.key), Some(Key::Num0));
        assert_eq!(Shortcut::parse("Z").map(|s| (s.ctrl, s.key)), Some((false, Key::Z)));
        assert_eq!(Shortcut::parse("F1").map(|s| s.key), Some(Key::F1));
        assert_eq!(Shortcut::parse("Ctrl++").map(|s| (s.ctrl, s.key)), Some((true, Key::Plus)));
        assert_eq!(Shortcut::parse("Cmd+K").map(|s| s.ctrl), Some(true));
        assert_eq!(Shortcut::parse("Esc").map(|s| s.label()), Some("Esc".into()));
        assert!(Shortcut::parse("").is_none());
        assert!(Shortcut::parse("Hyper+Z").is_none());
        assert!(Shortcut::parse("Ctrl+NotAKey").is_none());
    }

    #[test]
    fn shortcut_matching_is_exact() {
        let undo = Shortcut::parse("Ctrl+Z").expect("parse");
        let ctrl = Modifiers::COMMAND;
        assert!(undo.matches(Key::Z, ctrl));
        assert!(!undo.matches(Key::Z, ctrl | Modifiers::SHIFT));
        assert!(!undo.matches(Key::Z, Modifiers::NONE));
        // Windows reports Ctrl as both `ctrl` and `command`.
        assert!(undo.matches(Key::Z, Modifiers { ctrl: true, command: true, ..Default::default() }));
        let z = Shortcut::parse("Z").expect("parse");
        assert!(!z.matches(Key::Z, Modifiers::SHIFT));
    }

    fn reg() -> CommandRegistry {
        let mut r = CommandRegistry::default();
        r.register(Command::new("edit.undo", "Undo", Category::Edit, Action::Undo).key("Ctrl+Z").when(can_undo));
        r.register(
            Command::new("edit.redo", "Redo", Category::Edit, Action::Redo)
                .key("Ctrl+Y")
                .alt_key("Ctrl+Shift+Z")
                .when(can_redo),
        );
        r.register(Command::new("file.save", "Save", Category::File, Action::Save).key("Ctrl+S"));
        r.register(Command::new("file.save_as", "Save As…", Category::File, Action::SaveAs).key("Alt+S"));
        r.register(Command::new("file.save_all", "Save All", Category::File, Action::SaveAll).key("Ctrl+Shift+S"));
        r.register(
            Command::new("view.zoom_sel", "Zoom to Selection", Category::View, Action::Zoom(ZoomCmd::Selection))
                .key("Z"),
        );
        r.register(Command::new("edit.paste", "Paste", Category::Edit, Action::Paste).key("Ctrl+V"));
        r.register(Command::new("internal", "Hidden thing", Category::Edit, Action::Hide).hidden());
        r
    }

    #[test]
    fn registry_dispatch_and_replace() {
        let mut r = reg();
        let ctrl = Modifiers::COMMAND;
        assert_eq!(r.for_key(Key::Z, ctrl).map(|c| c.id.as_str()), Some("edit.undo"));
        assert_eq!(r.for_key(Key::Z, ctrl | Modifiers::SHIFT).map(|c| c.id.as_str()), Some("edit.redo"));
        assert_eq!(r.for_key(Key::Y, ctrl).map(|c| c.id.as_str()), Some("edit.redo"));
        assert_eq!(r.for_key(Key::S, Modifiers::ALT).map(|c| c.id.as_str()), Some("file.save_as"));
        assert_eq!(r.for_key(Key::S, ctrl | Modifiers::SHIFT).map(|c| c.id.as_str()), Some("file.save_all"));
        assert_eq!(r.for_key(Key::Z, Modifiers::NONE).map(|c| c.id.as_str()), Some("view.zoom_sel"));
        assert!(r.for_key(Key::Q, ctrl).is_none());
        assert!(r.conflicts().is_empty());
        // Enabled predicates.
        let st = CmdState { can_undo: true, ..Default::default() };
        assert!((r.get("edit.undo").expect("undo").enabled)(&st));
        assert!(!(r.get("edit.redo").expect("redo").enabled)(&st));
        // Replace keeps a single entry.
        let n = r.len();
        r.register(Command::new("file.save", "Save!", Category::File, Action::Save).key("Ctrl+S"));
        assert_eq!(r.len(), n);
        assert_eq!(r.get("file.save").map(|c| c.label.as_str()), Some("Save!"));
        // Conflict detection.
        r.register(Command::new("dup", "Dup", Category::File, Action::Open).key("Ctrl+Z"));
        assert_eq!(r.conflicts(), vec![("edit.undo".to_string(), "dup".to_string())]);
        assert_eq!(r.get("edit.redo").map(|c| c.tooltip()), Some("Redo (Ctrl+Y)".into()));
        assert_eq!(r.get("file.save_as").map(|c| c.tooltip()), Some("Save As (Alt+S)".into()));
    }

    #[test]
    fn palette_search_ranks_and_hides() {
        let r = reg();
        let ids = |q: &str| r.search(q, 10).iter().map(|c| c.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids("save").first().map(String::as_str), Some("file.save"));
        assert_eq!(ids("sa").len(), 3);
        assert_eq!(ids("zts").first().map(String::as_str), Some("view.zoom_sel"));
        assert_eq!(ids("redo"), ["edit.redo"]);
        assert!(ids("hidden").is_empty());
        // Empty query lists everything visible in registration order.
        assert_eq!(ids("").len(), 7);
        assert_eq!(ids("").first().map(String::as_str), Some("edit.undo"));
    }

    #[test]
    fn fuzzy_ranking() {
        let s = |q, t| fuzzy_score(q, t);
        assert!(s("xyz", "Save").is_none());
        assert!(s("sa", "Save").is_some());
        assert!(s("", "anything") == Some(0));
        // Prefix beats a later word start, which beats a mid-word match.
        assert!(s("sav", "Save") > s("sav", "Quick Save"));
        assert!(s("sav", "Quick Save") > s("sav", "Unsaved changes"));
        // Word-start initials.
        assert!(s("cts", "Crop to Selection") > s("cts", "Contrast"));
        assert!(s("zs", "Zoom to Selection") > s("zs", "Resize"));
        // Consecutive runs beat scattered matches.
        assert!(s("blur", "Blur") > s("blur", "Batch Lookup Rename"));
        // Shorter text wins ties.
        assert!(s("save", "Save") > s("save", "Save As…"));
        // Case-insensitive, camelCase word starts.
        assert!(s("SAVE", "save").is_some());
        assert!(s("ef", "EditFile") > s("ef", "Leftfield"));
        assert!(s("toolong", "too").is_none());
    }
}
