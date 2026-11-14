//! Pure overlay state machine: input events in, state changes and effects out.
//!
//! Everything here is free of Win32 so it can be unit-tested. Coordinates are
//! virtual-screen physical pixels.

use super::{OverlayMode, Selection};
use crate::common::{Point, Rect};

/// Mouse movement (Chebyshev distance) up to which a press counts as a click.
pub const CLICK_SLOP: i32 = 3;
/// Fallback when the settings have no fixed-size presets.
pub const DEFAULT_FIXED: (i32, i32) = (640, 480);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Left,
    Right,
    Up,
    Down,
    Tab,
    Enter,
    Escape,
    Space,
    Z,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Move(Point),
    LDown(Point),
    LUp(Point),
    LDouble(Point),
    /// Right button released (acting on release keeps the up-event away from the window below).
    RUp(Point),
    /// Wheel notches; positive = away from the user ("up").
    Wheel(i32),
    Key(Key),
}

/// What the window layer has to do after an input.
#[derive(Debug, Default, PartialEq)]
pub struct Effects {
    /// Move the system cursor (keyboard nudges, Tab corner switch).
    pub cursor: Option<Point>,
    /// The overlay is done: `Some(None)` = cancelled.
    pub finish: Option<Option<Selection>>,
    /// Persist `capture.fixed_index`.
    pub persist_fixed_index: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub magnifier: bool,
    pub crosshair: bool,
    pub capture_on_release: bool,
    pub presets: Vec<(i32, i32)>,
    pub fixed_index: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self { magnifier: true, crosshair: true, capture_on_release: true, presets: Vec::new(), fixed_index: 0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Monitor {
    pub rect: Rect,
    pub dpi: u32,
}

/// Nested element chain provider: (mode, top-level hwnd, its rect, point) → outermost..innermost.
pub type ChainFn = Box<dyn FnMut(OverlayMode, isize, Rect, Point) -> Vec<(isize, Rect)>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hover {
    pub top: isize,
    /// Outermost (the window) to innermost element; never empty.
    pub chain: Vec<(isize, Rect)>,
    pub level: usize,
}

impl Hover {
    pub fn current(&self) -> (isize, Rect) {
        self.chain[self.level.min(self.chain.len() - 1)]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handle {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
}

impl Handle {
    pub const ALL: [Handle; 8] = [
        Handle::TopLeft,
        Handle::Top,
        Handle::TopRight,
        Handle::Right,
        Handle::BottomRight,
        Handle::Bottom,
        Handle::BottomLeft,
        Handle::Left,
    ];

    /// Anchor point of the handle on `r` (right/bottom edges are exclusive coordinates).
    pub fn pos(self, r: &Rect) -> Point {
        let (l, t, rr, b) = (r.x, r.y, r.right(), r.bottom());
        let (cx, cy) = (l + r.w / 2, t + r.h / 2);
        match self {
            Handle::TopLeft => Point::new(l, t),
            Handle::Top => Point::new(cx, t),
            Handle::TopRight => Point::new(rr, t),
            Handle::Right => Point::new(rr, cy),
            Handle::BottomRight => Point::new(rr, b),
            Handle::Bottom => Point::new(cx, b),
            Handle::BottomLeft => Point::new(l, b),
            Handle::Left => Point::new(l, cy),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grab {
    Move,
    Handle(Handle),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdjustDrag {
    pub grab: Grab,
    pub start: Point,
    pub orig: Rect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// Hovering, nothing pressed.
    Idle,
    /// Button down, moved at most [`CLICK_SLOP`] px.
    Pressed { start: Point },
    /// Rubber-band drag: `anchor` is fixed, the moving corner is the cursor.
    Dragging { anchor: Point },
    /// Region mode after release when `capture_on_release` is off.
    Adjust { rect: Rect, drag: Option<AdjustDrag> },
    /// Freehand path being drawn.
    Drawing { points: Vec<Point> },
}

pub struct State {
    pub mode: OverlayMode,
    pub bounds: Rect,
    /// Top-level windows, front first.
    pub windows: Vec<(isize, Rect)>,
    pub monitors: Vec<Monitor>,
    pub cfg: Config,
    pub cursor: Point,
    pub phase: Phase,
    pub hover: Option<Hover>,
    /// Window/object multi-selection (Ctrl+click).
    pub multi: Vec<(isize, Rect)>,
    pub fixed_size: (i32, i32),
    pub fixed_index: usize,
    chain_fn: ChainFn,
}

// ---------------------------------------------------------------------------------------
// Pure helpers (unit-tested)
// ---------------------------------------------------------------------------------------

/// Index of the topmost window (front first) containing `p`.
pub fn hit_window(windows: &[(isize, Rect)], p: Point) -> Option<usize> {
    windows.iter().position(|(_, r)| !r.is_empty() && r.contains(p))
}

/// Tab during a drag: the next corner clockwise becomes the moving corner and its
/// opposite becomes the anchor. Returns (new anchor, new moving corner).
pub fn next_corner(anchor: Point, corner: Point) -> (Point, Point) {
    let (x0, x1) = (anchor.x.min(corner.x), anchor.x.max(corner.x));
    let (y0, y1) = (anchor.y.min(corner.y), anchor.y.max(corner.y));
    let right = corner.x >= anchor.x;
    let bottom = corner.y >= anchor.y;
    // Clockwise (y down): TL → TR → BR → BL → TL.
    let next = match (right, bottom) {
        (false, false) => Point::new(x1, y0),
        (true, false) => Point::new(x1, y1),
        (true, true) => Point::new(x0, y1),
        (false, true) => Point::new(x0, y0),
    };
    let opposite = Point::new(if next.x == x0 { x1 } else { x0 }, if next.y == y0 { y1 } else { y0 });
    (opposite, next)
}

/// Move `r` by (dx, dy), keeping it inside `bounds`.
pub fn nudge(r: Rect, dx: i32, dy: i32, bounds: &Rect) -> Rect {
    r.offset(dx, dy).clamp_inside(bounds)
}

/// Grow/shrink `r` from its right/bottom edge, at least 1 px, never past `bounds`.
pub fn resize(r: Rect, dw: i32, dh: i32, bounds: &Rect) -> Rect {
    let w = (r.w + dw).max(1).min((bounds.right() - r.x).max(1));
    let h = (r.h + dh).max(1).min((bounds.bottom() - r.y).max(1));
    Rect::new(r.x, r.y, w, h)
}

/// Wrap-around preset cycling.
pub fn cycle(index: usize, len: usize, delta: i32) -> usize {
    if len == 0 {
        return 0;
    }
    let len = len as i64;
    (((index as i64 + delta as i64) % len + len) % len) as usize
}

/// A rect of `size` centred on `c`, clamped inside `bounds`.
pub fn centered_rect(c: Point, size: (i32, i32), bounds: &Rect) -> Rect {
    Rect::new(c.x - size.0 / 2, c.y - size.1 / 2, size.0.max(1), size.1.max(1)).clamp_inside(bounds)
}

/// Which part of an adjustable selection is under `p` (`tol` = handle hit radius).
pub fn hit_grab(r: &Rect, p: Point, tol: i32) -> Option<Grab> {
    for h in Handle::ALL {
        let hp = h.pos(r);
        if (p.x - hp.x).abs() <= tol && (p.y - hp.y).abs() <= tol {
            return Some(Grab::Handle(h));
        }
    }
    r.contains(p).then_some(Grab::Move)
}

/// Apply a handle drag of (dx, dy) to `orig`; the result is normalised and at least 1x1.
pub fn apply_handle(orig: Rect, h: Handle, dx: i32, dy: i32) -> Rect {
    let (mut l, mut t, mut r, mut b) = (orig.x, orig.y, orig.right(), orig.bottom());
    match h {
        Handle::TopLeft => {
            l += dx;
            t += dy;
        }
        Handle::Top => t += dy,
        Handle::TopRight => {
            r += dx;
            t += dy;
        }
        Handle::Right => r += dx,
        Handle::BottomRight => {
            r += dx;
            b += dy;
        }
        Handle::Bottom => b += dy,
        Handle::BottomLeft => {
            l += dx;
            b += dy;
        }
        Handle::Left => l += dx,
    }
    if l > r {
        std::mem::swap(&mut l, &mut r);
    }
    if t > b {
        std::mem::swap(&mut t, &mut b);
    }
    Rect::new(l, t, (r - l).max(1), (b - t).max(1))
}

fn chebyshev(a: Point, b: Point) -> i32 {
    (a.x - b.x).abs().max((a.y - b.y).abs())
}

fn clamp_point(p: Point, b: &Rect) -> Point {
    Point::new(p.x.clamp(b.x, (b.right() - 1).max(b.x)), p.y.clamp(b.y, (b.bottom() - 1).max(b.y)))
}

fn arrow_delta(k: Key) -> Option<(i32, i32)> {
    match k {
        Key::Left => Some((-1, 0)),
        Key::Right => Some((1, 0)),
        Key::Up => Some((0, -1)),
        Key::Down => Some((0, 1)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------
// State machine
// ---------------------------------------------------------------------------------------

impl State {
    pub fn new(
        mode: OverlayMode,
        bounds: Rect,
        windows: Vec<(isize, Rect)>,
        monitors: Vec<Monitor>,
        cfg: Config,
        cursor: Point,
        chain_fn: ChainFn,
    ) -> Self {
        let fixed_index = if cfg.presets.is_empty() { 0 } else { cfg.fixed_index.min(cfg.presets.len() - 1) };
        let fixed_size = cfg.presets.get(fixed_index).copied().unwrap_or(DEFAULT_FIXED);
        let fixed_size = (fixed_size.0.clamp(1, bounds.w.max(1)), fixed_size.1.clamp(1, bounds.h.max(1)));
        let mut s = Self {
            mode,
            bounds,
            windows,
            monitors,
            cfg,
            cursor: clamp_point(cursor, &bounds),
            phase: Phase::Idle,
            hover: None,
            multi: Vec::new(),
            fixed_size,
            fixed_index,
            chain_fn,
        };
        s.update_hover();
        s
    }

    /// Monitor containing `p` (nearest-by-containment, else the first one).
    pub fn monitor_at(&self, p: Point) -> Monitor {
        self.monitors
            .iter()
            .find(|m| m.rect.contains(p))
            .or_else(|| self.monitors.first())
            .copied()
            .unwrap_or(Monitor { rect: self.bounds, dpi: 96 })
    }

    /// The rubber-band / adjusted / fixed rectangle, if any.
    pub fn selection_rect(&self) -> Option<Rect> {
        match &self.phase {
            Phase::Dragging { anchor } => Some(Rect::from_corners(*anchor, self.cursor).intersect(&self.bounds)),
            Phase::Adjust { rect, .. } => Some(*rect),
            _ if self.mode == OverlayMode::Fixed => Some(self.fixed_rect()),
            _ => None,
        }
    }

    pub fn fixed_rect(&self) -> Rect {
        centered_rect(self.cursor, self.fixed_size, &self.bounds)
    }

    /// Moving corner of the current drag (the cursor), for highlighting.
    pub fn moving_corner(&self) -> Option<Point> {
        matches!(self.phase, Phase::Dragging { .. }).then_some(self.cursor)
    }

    pub fn is_dragging(&self) -> bool {
        matches!(self.phase, Phase::Dragging { .. } | Phase::Adjust { drag: Some(_), .. } | Phase::Drawing { .. })
    }

    fn hover_rect_or_monitor(&self, p: Point) -> Rect {
        match &self.hover {
            Some(h) => h.current().1.intersect(&self.bounds),
            None => self.monitor_at(p).rect.intersect(&self.bounds),
        }
    }

    fn uses_chain(&self) -> bool {
        matches!(self.mode, OverlayMode::WindowObject | OverlayMode::ScrollPick)
    }

    fn update_hover(&mut self) {
        let p = self.cursor;
        let Some(i) = hit_window(&self.windows, p) else {
            self.hover = None;
            return;
        };
        let (hwnd, rect) = self.windows[i];
        if !self.uses_chain() {
            self.hover = Some(Hover { top: hwnd, chain: vec![(hwnd, rect)], level: 0 });
            return;
        }
        let mut chain = (self.chain_fn)(self.mode, hwnd, rect, p);
        chain.retain(|(_, r)| !r.is_empty());
        if chain.is_empty() {
            chain.push((hwnd, rect));
        }
        let level = match &self.hover {
            // Same element chain: keep the level the user chose with the wheel.
            Some(old) if old.top == hwnd && old.chain == chain => old.level,
            _ => chain.len() - 1,
        };
        self.hover = Some(Hover { top: hwnd, chain, level });
    }

    fn finish(sel: Option<Selection>) -> Effects {
        Effects { finish: Some(sel), ..Default::default() }
    }

    fn multi_selection(&self) -> Option<Selection> {
        match self.multi.as_slice() {
            [] => None,
            [(hwnd, rect)] => Some(Selection::Window { hwnd: *hwnd, rect: *rect }),
            many => Some(Selection::Multi(many.iter().map(|(_, r)| *r).collect())),
        }
    }

    fn hover_window_selection(&self) -> Option<Selection> {
        self.hover.as_ref().map(|h| {
            let (hwnd, rect) = h.current();
            // WindowObject reports the top-level window; ScrollPick the (child) control.
            let hwnd = if self.mode == OverlayMode::ScrollPick { hwnd } else { h.top };
            Selection::Window { hwnd, rect }
        })
    }

    /// Feed one input event.
    pub fn handle(&mut self, input: Input, mods: Mods) -> Effects {
        match input {
            Input::Move(p) => {
                self.on_move(p);
                Effects::default()
            }
            Input::LDown(p) => {
                self.on_move(p);
                self.on_ldown(p);
                Effects::default()
            }
            Input::LDouble(p) => {
                self.on_move(p);
                if let Phase::Adjust { rect, .. } = &self.phase {
                    if rect.contains(p) {
                        return Self::finish(Some(Selection::Rect(*rect)));
                    }
                }
                self.on_ldown(p);
                Effects::default()
            }
            Input::LUp(p) => {
                self.on_move(p);
                self.on_lup(p, mods)
            }
            Input::RUp(p) => {
                self.on_move(p);
                self.on_rup()
            }
            Input::Wheel(n) => self.on_wheel(n),
            Input::Key(k) => self.on_key(k, mods),
        }
    }

    fn on_move(&mut self, p: Point) {
        let p = clamp_point(p, &self.bounds);
        if p == self.cursor && !matches!(self.phase, Phase::Pressed { .. }) {
            return;
        }
        self.cursor = p;
        match &mut self.phase {
            Phase::Idle => self.update_hover(),
            Phase::Pressed { start } => {
                let start = *start;
                let can_drag = matches!(self.mode, OverlayMode::Region | OverlayMode::ScrollPick);
                if can_drag && chebyshev(start, p) > CLICK_SLOP {
                    self.phase = Phase::Dragging { anchor: start };
                }
            }
            Phase::Dragging { .. } => {}
            Phase::Adjust { rect, drag: Some(d) } => {
                let (dx, dy) = (p.x - d.start.x, p.y - d.start.y);
                *rect = match d.grab {
                    Grab::Move => nudge(d.orig, dx, dy, &self.bounds),
                    Grab::Handle(h) => apply_handle(d.orig, h, dx, dy).intersect(&self.bounds),
                };
                if rect.is_empty() {
                    *rect = Rect::new(p.x, p.y, 1, 1);
                }
            }
            Phase::Adjust { drag: None, .. } => {}
            Phase::Drawing { points } => {
                if points.last() != Some(&p) {
                    points.push(p);
                }
            }
        }
    }

    fn on_ldown(&mut self, p: Point) {
        match self.mode {
            OverlayMode::Freehand => self.phase = Phase::Drawing { points: vec![p] },
            OverlayMode::Region => {
                let tol = crate::common::win::scale(6, self.monitor_at(p).dpi).max(3);
                if let Phase::Adjust { rect, drag } = &mut self.phase {
                    if let Some(grab) = hit_grab(rect, p, tol) {
                        *drag = Some(AdjustDrag { grab, start: p, orig: *rect });
                        return;
                    }
                }
                self.phase = Phase::Pressed { start: p };
                self.update_hover();
            }
            _ => {
                if self.phase == Phase::Idle {
                    self.phase = Phase::Pressed { start: p };
                }
            }
        }
    }

    fn on_lup(&mut self, p: Point, mods: Mods) -> Effects {
        let phase = std::mem::replace(&mut self.phase, Phase::Idle);
        match (self.mode, phase) {
            (OverlayMode::Freehand, Phase::Drawing { points }) => {
                if points.len() >= 3 {
                    Self::finish(Some(Selection::Polygon(points)))
                } else {
                    Self::finish(None)
                }
            }
            (OverlayMode::Region, Phase::Pressed { .. }) => {
                let r = self.hover_rect_or_monitor(p);
                self.after_region_release(r)
            }
            (OverlayMode::Region, Phase::Dragging { anchor }) => {
                let r = Rect::from_corners(anchor, self.cursor).intersect(&self.bounds);
                self.after_region_release(r)
            }
            (OverlayMode::Region, Phase::Adjust { rect, .. }) => {
                self.phase = Phase::Adjust { rect, drag: None };
                Effects::default()
            }
            (OverlayMode::ScrollPick, Phase::Pressed { .. }) => match self.hover_window_selection() {
                Some(sel) => Self::finish(Some(sel)),
                None => Self::finish(Some(Selection::Rect(self.hover_rect_or_monitor(p)))),
            },
            (OverlayMode::ScrollPick, Phase::Dragging { anchor }) => {
                Self::finish(Some(Selection::Rect(Rect::from_corners(anchor, self.cursor).intersect(&self.bounds))))
            }
            (OverlayMode::WindowObject, Phase::Pressed { .. }) => {
                let Some(h) = &self.hover else {
                    return Effects::default();
                };
                let item = (h.top, h.current().1);
                if mods.ctrl {
                    if let Some(i) = self.multi.iter().position(|m| m.1 == item.1) {
                        self.multi.remove(i);
                    } else {
                        self.multi.push(item);
                    }
                    return Effects::default();
                }
                if self.multi.is_empty() {
                    return Self::finish(self.hover_window_selection());
                }
                if !self.multi.iter().any(|m| m.1 == item.1) {
                    self.multi.push(item);
                }
                Self::finish(self.multi_selection())
            }
            (OverlayMode::Fixed, Phase::Pressed { .. }) => Self::finish(Some(Selection::Rect(self.fixed_rect()))),
            (_, other) => {
                self.phase = other;
                Effects::default()
            }
        }
    }

    fn after_region_release(&mut self, r: Rect) -> Effects {
        if r.is_empty() {
            self.update_hover();
            return Effects::default();
        }
        if self.cfg.capture_on_release {
            Self::finish(Some(Selection::Rect(r)))
        } else {
            self.phase = Phase::Adjust { rect: r, drag: None };
            Effects::default()
        }
    }

    fn on_rup(&mut self) -> Effects {
        let has_selection = match self.mode {
            OverlayMode::Region => !matches!(self.phase, Phase::Idle),
            OverlayMode::Freehand => matches!(self.phase, Phase::Drawing { .. }),
            OverlayMode::ScrollPick => matches!(self.phase, Phase::Dragging { .. }),
            OverlayMode::WindowObject => !self.multi.is_empty(),
            OverlayMode::Fixed => false,
        };
        if !has_selection {
            return Self::finish(None);
        }
        self.phase = Phase::Idle;
        self.multi.clear();
        self.update_hover();
        Effects::default()
    }

    fn on_wheel(&mut self, notches: i32) -> Effects {
        if notches == 0 {
            return Effects::default();
        }
        match self.mode {
            OverlayMode::WindowObject | OverlayMode::ScrollPick => {
                if let Some(h) = &mut self.hover {
                    let max = h.chain.len() as i32 - 1;
                    // Wheel up = outer (towards the window), down = inner.
                    h.level = (h.level as i32 - notches).clamp(0, max) as usize;
                }
                Effects::default()
            }
            OverlayMode::Fixed => self.cycle_preset(-notches.signum()),
            _ => Effects::default(),
        }
    }

    fn cycle_preset(&mut self, delta: i32) -> Effects {
        if self.cfg.presets.is_empty() {
            return Effects::default();
        }
        self.fixed_index = cycle(self.fixed_index, self.cfg.presets.len(), delta);
        let (w, h) = self.cfg.presets[self.fixed_index];
        self.fixed_size = (w.clamp(1, self.bounds.w.max(1)), h.clamp(1, self.bounds.h.max(1)));
        Effects { persist_fixed_index: Some(self.fixed_index), ..Default::default() }
    }

    /// Move the cursor by (dx, dy) as if the mouse moved.
    fn move_cursor(&mut self, dx: i32, dy: i32) -> Effects {
        let p = clamp_point(Point::new(self.cursor.x + dx, self.cursor.y + dy), &self.bounds);
        if let Phase::Pressed { start } = self.phase {
            // Keyboard precision: arrows while the button is down start a drag at once.
            if matches!(self.mode, OverlayMode::Region | OverlayMode::ScrollPick) {
                self.phase = Phase::Dragging { anchor: start };
            }
        }
        self.on_move(p);
        Effects { cursor: Some(p), ..Default::default() }
    }

    fn on_key(&mut self, k: Key, mods: Mods) -> Effects {
        if k == Key::Escape {
            return Self::finish(None);
        }
        let step = if mods.ctrl { 10 } else { 1 };
        match self.mode {
            OverlayMode::Region | OverlayMode::ScrollPick => self.region_key(k, mods, step),
            OverlayMode::Freehand => match arrow_delta(k) {
                Some((dx, dy)) => self.move_cursor(dx, dy),
                None => Effects::default(),
            },
            OverlayMode::WindowObject => match k {
                Key::Enter => match self.multi_selection() {
                    Some(sel) => Self::finish(Some(sel)),
                    None => match self.hover_window_selection() {
                        Some(sel) => Self::finish(Some(sel)),
                        None => Effects::default(),
                    },
                },
                _ => match arrow_delta(k) {
                    Some((dx, dy)) => self.move_cursor(dx, dy),
                    None => Effects::default(),
                },
            },
            OverlayMode::Fixed => match k {
                Key::Enter => Self::finish(Some(Selection::Rect(self.fixed_rect()))),
                Key::Space => self.cycle_preset(1),
                _ => match arrow_delta(k) {
                    Some((dx, dy)) if mods.shift => {
                        let step = if mods.ctrl { 10 } else { 1 };
                        let (w, h) = self.fixed_size;
                        self.fixed_size = (
                            (w + dx * step).clamp(1, self.bounds.w.max(1)),
                            (h + dy * step).clamp(1, self.bounds.h.max(1)),
                        );
                        Effects::default()
                    }
                    Some((dx, dy)) => self.move_cursor(dx, dy),
                    None => Effects::default(),
                },
            },
        }
    }

    fn region_key(&mut self, k: Key, mods: Mods, step: i32) -> Effects {
        match (&mut self.phase, k) {
            (Phase::Dragging { anchor }, Key::Tab) => {
                let (a, c) = next_corner(*anchor, self.cursor);
                *anchor = a;
                self.cursor = c;
                Effects { cursor: Some(c), ..Default::default() }
            }
            (Phase::Dragging { .. } | Phase::Pressed { .. }, Key::Z) if mods.ctrl => {
                self.phase = Phase::Pressed { start: self.cursor };
                Effects::default()
            }
            (Phase::Adjust { .. }, Key::Z) if mods.ctrl => {
                self.phase = Phase::Idle;
                self.update_hover();
                Effects::default()
            }
            (Phase::Dragging { anchor }, Key::Enter) => {
                let r = Rect::from_corners(*anchor, self.cursor).intersect(&self.bounds);
                Self::finish(Some(Selection::Rect(r)))
            }
            (Phase::Adjust { rect, .. }, Key::Enter) => Self::finish(Some(Selection::Rect(*rect))),
            (Phase::Idle, Key::Enter) if self.mode == OverlayMode::ScrollPick => {
                Self::finish(self.hover_window_selection())
            }
            (Phase::Adjust { rect, drag: None }, _) => {
                if let Some((dx, dy)) = arrow_delta(k) {
                    *rect = if mods.shift {
                        resize(*rect, dx * step, dy * step, &self.bounds)
                    } else {
                        nudge(*rect, dx * step, dy * step, &self.bounds)
                    };
                }
                Effects::default()
            }
            _ => match arrow_delta(k) {
                Some((dx, dy)) => self.move_cursor(dx * step, dy * step),
                None => Effects::default(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_chain() -> ChainFn {
        Box::new(|_, _, _, _| Vec::new())
    }

    fn state(mode: OverlayMode, cfg: Config) -> State {
        let bounds = Rect::new(0, 0, 1000, 800);
        let windows = vec![(1, Rect::new(100, 100, 200, 200)), (2, Rect::new(0, 0, 1000, 800))];
        let monitors = vec![Monitor { rect: bounds, dpi: 96 }];
        State::new(mode, bounds, windows, monitors, cfg, Point::new(500, 500), no_chain())
    }

    const NONE: Mods = Mods { ctrl: false, shift: false, alt: false };
    const CTRL: Mods = Mods { ctrl: true, shift: false, alt: false };
    const SHIFT: Mods = Mods { ctrl: false, shift: true, alt: false };
    const CTRL_SHIFT: Mods = Mods { ctrl: true, shift: true, alt: false };

    #[test]
    fn hit_testing_prefers_front_window() {
        let w = vec![(1, Rect::new(10, 10, 10, 10)), (2, Rect::new(0, 0, 100, 100)), (3, Rect::new(0, 0, 0, 0))];
        assert_eq!(hit_window(&w, Point::new(15, 15)), Some(0));
        assert_eq!(hit_window(&w, Point::new(50, 50)), Some(1));
        assert_eq!(hit_window(&w, Point::new(20, 20)), Some(1), "right/bottom edges are exclusive");
        assert_eq!(hit_window(&w, Point::new(200, 5)), None);
    }

    #[test]
    fn corner_switching_cycles_clockwise() {
        let a = Point::new(0, 0);
        let c = Point::new(10, 10); // moving bottom-right
        let (a1, c1) = next_corner(a, c);
        assert_eq!((a1, c1), (Point::new(10, 0), Point::new(0, 10))); // now bottom-left
        let (a2, c2) = next_corner(a1, c1);
        assert_eq!((a2, c2), (Point::new(10, 10), Point::new(0, 0))); // top-left
        let (a3, c3) = next_corner(a2, c2);
        assert_eq!((a3, c3), (Point::new(0, 10), Point::new(10, 0))); // top-right
        let (a4, c4) = next_corner(a3, c3);
        assert_eq!((a4, c4), (a, c));
        // The selected rectangle never changes while cycling.
        for (x, y) in [(a1, c1), (a2, c2), (a3, c3)] {
            assert_eq!(Rect::from_corners(x, y), Rect::from_corners(a, c));
        }
    }

    #[test]
    fn tab_during_drag_moves_cursor_and_anchor() {
        let mut s = state(OverlayMode::Region, Config::default());
        s.handle(Input::LDown(Point::new(10, 10)), NONE);
        s.handle(Input::Move(Point::new(50, 40)), NONE);
        assert_eq!(s.selection_rect(), Some(Rect::new(10, 10, 41, 31)));
        let e = s.handle(Input::Key(Key::Tab), NONE);
        assert_eq!(e.cursor, Some(Point::new(10, 40)));
        assert_eq!(s.phase, Phase::Dragging { anchor: Point::new(50, 10) });
        assert_eq!(s.selection_rect(), Some(Rect::new(10, 10, 41, 31)));
        // Now dragging the bottom-left corner further left.
        s.handle(Input::Move(Point::new(5, 40)), NONE);
        assert_eq!(s.selection_rect(), Some(Rect::new(5, 10, 46, 31)));
    }

    #[test]
    fn nudge_and_resize_math() {
        let b = Rect::new(0, 0, 100, 100);
        assert_eq!(nudge(Rect::new(10, 10, 20, 20), 1, 0, &b), Rect::new(11, 10, 20, 20));
        assert_eq!(nudge(Rect::new(80, 10, 20, 20), 1, 0, &b), Rect::new(80, 10, 20, 20));
        assert_eq!(nudge(Rect::new(0, 0, 20, 20), -1, -1, &b), Rect::new(0, 0, 20, 20));
        assert_eq!(resize(Rect::new(10, 10, 20, 20), 1, 0, &b), Rect::new(10, 10, 21, 20));
        assert_eq!(resize(Rect::new(10, 10, 1, 1), -1, -1, &b), Rect::new(10, 10, 1, 1));
        assert_eq!(resize(Rect::new(90, 90, 10, 10), 5, 5, &b), Rect::new(90, 90, 10, 10));
        assert_eq!(apply_handle(Rect::new(10, 10, 10, 10), Handle::TopLeft, -5, -5), Rect::new(5, 5, 15, 15));
        assert_eq!(apply_handle(Rect::new(10, 10, 10, 10), Handle::Right, -15, 0), Rect::new(5, 10, 5, 10));
        assert_eq!(apply_handle(Rect::new(10, 10, 10, 10), Handle::Bottom, 0, -10), Rect::new(10, 10, 10, 1));
    }

    #[test]
    fn arrows_nudge_cursor_and_adjust_mode() {
        let cfg = Config { capture_on_release: false, ..Config::default() };
        let mut s = state(OverlayMode::Region, cfg);
        let e = s.handle(Input::Key(Key::Right), NONE);
        assert_eq!(e.cursor, Some(Point::new(501, 500)));
        s.handle(Input::LDown(Point::new(10, 10)), NONE);
        s.handle(Input::Move(Point::new(30, 30)), NONE);
        let e = s.handle(Input::LUp(Point::new(30, 30)), NONE);
        assert_eq!(e.finish, None);
        assert_eq!(s.phase, Phase::Adjust { rect: Rect::new(10, 10, 21, 21), drag: None });
        s.handle(Input::Key(Key::Left), NONE);
        assert_eq!(s.selection_rect(), Some(Rect::new(9, 10, 21, 21)));
        s.handle(Input::Key(Key::Down), SHIFT);
        assert_eq!(s.selection_rect(), Some(Rect::new(9, 10, 21, 22)));
        // Drag inside moves.
        s.handle(Input::LDown(Point::new(22, 22)), NONE);
        s.handle(Input::Move(Point::new(32, 27)), NONE);
        s.handle(Input::LUp(Point::new(32, 27)), NONE);
        assert_eq!(s.selection_rect(), Some(Rect::new(19, 15, 21, 22)));
        let e = s.handle(Input::Key(Key::Enter), NONE);
        assert_eq!(e.finish, Some(Some(Selection::Rect(Rect::new(19, 15, 21, 22)))));
    }

    #[test]
    fn tiny_selection_and_click_selects_window() {
        let mut s = state(OverlayMode::Region, Config::default());
        // Click without drag inside window 1 → its rect.
        s.handle(Input::LDown(Point::new(150, 150)), NONE);
        s.handle(Input::Move(Point::new(152, 151)), NONE);
        let e = s.handle(Input::LUp(Point::new(152, 151)), NONE);
        assert_eq!(e.finish, Some(Some(Selection::Rect(Rect::new(100, 100, 200, 200)))));
        // Drag away and back: a 1x1 selection.
        let mut s = state(OverlayMode::Region, Config::default());
        s.handle(Input::LDown(Point::new(400, 400)), NONE);
        s.handle(Input::Move(Point::new(420, 420)), NONE);
        s.handle(Input::Move(Point::new(400, 400)), NONE);
        let e = s.handle(Input::LUp(Point::new(400, 400)), NONE);
        assert_eq!(e.finish, Some(Some(Selection::Rect(Rect::new(400, 400, 1, 1)))));
    }

    #[test]
    fn ctrl_z_and_right_click() {
        let mut s = state(OverlayMode::Region, Config::default());
        s.handle(Input::LDown(Point::new(10, 10)), NONE);
        s.handle(Input::Move(Point::new(60, 60)), NONE);
        s.handle(Input::Key(Key::Z), CTRL);
        assert_eq!(s.phase, Phase::Pressed { start: Point::new(60, 60) });
        s.handle(Input::Move(Point::new(80, 70)), NONE);
        assert_eq!(s.selection_rect(), Some(Rect::new(60, 60, 21, 11)));
        // Right-click with a selection reselects, without one it cancels.
        assert_eq!(s.handle(Input::RUp(Point::new(80, 70)), NONE).finish, None);
        assert_eq!(s.phase, Phase::Idle);
        assert_eq!(s.handle(Input::RUp(Point::new(80, 70)), NONE).finish, Some(None));
        let mut s = state(OverlayMode::Region, Config::default());
        assert_eq!(s.handle(Input::Key(Key::Escape), NONE).finish, Some(None));
    }

    #[test]
    fn freehand_polygon_and_cancel() {
        let mut s = state(OverlayMode::Freehand, Config::default());
        s.handle(Input::LDown(Point::new(0, 0)), NONE);
        s.handle(Input::Move(Point::new(10, 0)), NONE);
        s.handle(Input::Move(Point::new(10, 0)), NONE);
        s.handle(Input::Move(Point::new(10, 10)), NONE);
        let e = s.handle(Input::LUp(Point::new(10, 10)), NONE);
        let pts = vec![Point::new(0, 0), Point::new(10, 0), Point::new(10, 10)];
        assert_eq!(e.finish, Some(Some(Selection::Polygon(pts))));
        let mut s = state(OverlayMode::Freehand, Config::default());
        s.handle(Input::LDown(Point::new(0, 0)), NONE);
        s.handle(Input::Move(Point::new(10, 0)), NONE);
        assert_eq!(s.handle(Input::LUp(Point::new(10, 0)), NONE).finish, Some(None));
    }

    #[test]
    fn preset_cycling() {
        assert_eq!(cycle(0, 3, 1), 1);
        assert_eq!(cycle(2, 3, 1), 0);
        assert_eq!(cycle(0, 3, -1), 2);
        assert_eq!(cycle(5, 0, 1), 0);
        let cfg = Config { presets: vec![(100, 50), (200, 100), (300, 150)], fixed_index: 2, ..Config::default() };
        let mut s = state(OverlayMode::Fixed, cfg);
        assert_eq!(s.fixed_rect(), Rect::new(350, 425, 300, 150));
        let e = s.handle(Input::Key(Key::Space), NONE);
        assert_eq!(e.persist_fixed_index, Some(0));
        assert_eq!(s.fixed_size, (100, 50));
        let e = s.handle(Input::Wheel(1), NONE);
        assert_eq!(e.persist_fixed_index, Some(2));
        let e = s.handle(Input::Wheel(-1), NONE);
        assert_eq!(e.persist_fixed_index, Some(0));
        s.handle(Input::Key(Key::Right), SHIFT);
        assert_eq!(s.fixed_size, (101, 50));
        s.handle(Input::Key(Key::Down), CTRL_SHIFT);
        assert_eq!(s.fixed_size, (101, 60));
        // Clamped to the screen near an edge.
        s.handle(Input::Move(Point::new(995, 795)), NONE);
        assert_eq!(s.fixed_rect(), Rect::new(899, 740, 101, 60));
        s.handle(Input::LDown(Point::new(995, 795)), NONE);
        let e = s.handle(Input::LUp(Point::new(995, 795)), NONE);
        assert_eq!(e.finish, Some(Some(Selection::Rect(Rect::new(899, 740, 101, 60)))));
    }

    #[test]
    fn window_object_levels_and_multi() {
        let chain: ChainFn = Box::new(|_, hwnd, rect, _| {
            vec![(hwnd, rect), (hwnd, Rect::new(110, 110, 100, 100)), (hwnd, Rect::new(120, 120, 10, 10))]
        });
        let bounds = Rect::new(0, 0, 1000, 800);
        let windows = vec![(1, Rect::new(100, 100, 200, 200)), (2, Rect::new(400, 400, 100, 100))];
        let mons = vec![Monitor { rect: bounds, dpi: 96 }];
        let mut s = State::new(
            OverlayMode::WindowObject,
            bounds,
            windows,
            mons,
            Config::default(),
            Point::new(125, 125),
            chain,
        );
        assert_eq!(s.hover.as_ref().map(|h| h.current().1), Some(Rect::new(120, 120, 10, 10)));
        s.handle(Input::Wheel(1), NONE);
        assert_eq!(s.hover.as_ref().map(|h| h.current().1), Some(Rect::new(110, 110, 100, 100)));
        s.handle(Input::Move(Point::new(126, 125)), NONE);
        assert_eq!(s.hover.as_ref().map(|h| h.level), Some(1), "level survives moves within the same chain");
        s.handle(Input::Wheel(5), NONE);
        assert_eq!(s.hover.as_ref().map(|h| h.level), Some(0));
        // Ctrl+click the window, then click another one → Multi.
        s.handle(Input::LDown(Point::new(126, 125)), CTRL);
        s.handle(Input::LUp(Point::new(126, 125)), CTRL);
        assert_eq!(s.multi.len(), 1);
        s.handle(Input::Move(Point::new(450, 450)), NONE);
        s.handle(Input::Wheel(10), NONE);
        s.handle(Input::LDown(Point::new(450, 450)), NONE);
        let e = s.handle(Input::LUp(Point::new(450, 450)), NONE);
        assert_eq!(
            e.finish,
            Some(Some(Selection::Multi(vec![Rect::new(100, 100, 200, 200), Rect::new(400, 400, 100, 100)])))
        );
    }

    #[test]
    fn window_object_single_and_toggle() {
        let mut s = state(OverlayMode::WindowObject, Config::default());
        s.handle(Input::Move(Point::new(150, 150)), NONE);
        s.handle(Input::LDown(Point::new(150, 150)), CTRL);
        s.handle(Input::LUp(Point::new(150, 150)), CTRL);
        s.handle(Input::LDown(Point::new(150, 150)), CTRL);
        s.handle(Input::LUp(Point::new(150, 150)), CTRL);
        assert!(s.multi.is_empty(), "ctrl+click twice toggles off");
        s.handle(Input::LDown(Point::new(150, 150)), CTRL);
        s.handle(Input::LUp(Point::new(150, 150)), CTRL);
        let e = s.handle(Input::Key(Key::Enter), NONE);
        assert_eq!(e.finish, Some(Some(Selection::Window { hwnd: 1, rect: Rect::new(100, 100, 200, 200) })));
    }

    #[test]
    fn scroll_pick_click_and_drag() {
        let mut s = state(OverlayMode::ScrollPick, Config::default());
        s.handle(Input::LDown(Point::new(150, 150)), NONE);
        let e = s.handle(Input::LUp(Point::new(150, 150)), NONE);
        assert_eq!(e.finish, Some(Some(Selection::Window { hwnd: 1, rect: Rect::new(100, 100, 200, 200) })));
        let mut s = state(OverlayMode::ScrollPick, Config::default());
        s.handle(Input::LDown(Point::new(150, 150)), NONE);
        s.handle(Input::Move(Point::new(250, 200)), NONE);
        let e = s.handle(Input::LUp(Point::new(250, 200)), NONE);
        assert_eq!(e.finish, Some(Some(Selection::Rect(Rect::new(150, 150, 101, 51)))));
    }
}
