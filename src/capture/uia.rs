//! Nested element rectangles under a point: Win32 child windows, then UI Automation.
//!
//! Called on every mouse move by the window/object overlay, so it must be fast:
//! * The Win32 chain (`ChildWindowFromPointEx`) is computed inline (microseconds).
//! * UI Automation queries are cross-process and can hang, so they run on a dedicated
//!   MTA worker thread. The caller waits at most [`UIA_WAIT`]; late results land in a
//!   small cache keyed by (window, point), so the next call at that point (e.g. the mouse
//!   wheel changing the level) gets them. While the worker is busy no new query is queued.
//! * Because the overlay covers the screen, `ElementFromPoint` would hit the overlay
//!   itself; instead we start at the innermost child window's element and descend through
//!   children whose bounding rectangle contains the point.

use crate::common::{win, Point, Rect};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation8, IUIAutomation, IUIAutomation2, IUIAutomationCacheRequest, IUIAutomationCondition,
    TreeScope_Children, UIA_BoundingRectanglePropertyId,
};
use windows::Win32::UI::WindowsAndMessaging::{
    ChildWindowFromPointEx, GetWindowRect, IsWindow, CWP_SKIPINVISIBLE, CWP_SKIPTRANSPARENT,
};

/// How long a caller waits for UI Automation before using the Win32 chain alone.
pub const UIA_WAIT: Duration = Duration::from_millis(30);
const CACHE_TTL: Duration = Duration::from_secs(5);
const CACHE_LEN: usize = 64;
const MAX_DEPTH: usize = 24;
const MAX_CHILDREN: i32 = 2000;
const IDLE_RELEASE: Duration = Duration::from_secs(10);

pub fn element_chain_at(top: isize, p: Point) -> Vec<Rect> {
    let top_h = win::hwnd_from_isize(top);
    // SAFETY: plain query.
    if !unsafe { IsWindow(top_h) }.as_bool() {
        return Vec::new();
    }
    let bounds = win::window_bounds(top_h);
    if bounds.is_empty() {
        return Vec::new();
    }
    if !bounds.contains(p) {
        return vec![bounds];
    }
    let (mut rects, innermost) = win32_chain(top_h, p);
    rects.extend(uia_lookup(top, innermost, p));
    normalize_chain(bounds, rects, p)
}

/// Child windows under `p`, outermost first, and the innermost window handle.
fn win32_chain(top: HWND, p: Point) -> (Vec<Rect>, isize) {
    let mut rects = Vec::new();
    let mut parent = top;
    for _ in 0..16 {
        let mut cp = POINT { x: p.x, y: p.y };
        // SAFETY: plain Win32 queries with valid pointers; stale handles just fail.
        let child = unsafe {
            if !ScreenToClient(parent, &mut cp).as_bool() {
                break;
            }
            ChildWindowFromPointEx(parent, cp, CWP_SKIPINVISIBLE | CWP_SKIPTRANSPARENT)
        };
        if child.is_invalid() || child == parent {
            break;
        }
        let mut r = Default::default();
        // SAFETY: valid out pointer.
        if unsafe { GetWindowRect(child, &mut r) }.is_err() {
            break;
        }
        rects.push(win::rect_from_win(&r));
        parent = child;
    }
    (rects, win::hwnd_to_isize(parent))
}

/// Clip to the window, keep rects containing `p`, order outermost → innermost, dedupe.
pub fn normalize_chain(window: Rect, rects: Vec<Rect>, p: Point) -> Vec<Rect> {
    let mut v: Vec<Rect> =
        rects.into_iter().map(|r| r.intersect(&window)).filter(|r| !r.is_empty() && r.contains(p)).collect();
    v.sort_by_key(|r| std::cmp::Reverse(r.area()));
    v.dedup();
    v.retain(|r| *r != window);
    v.insert(0, window);
    v
}

// ---- UI Automation worker --------------------------------------------------------------

struct Req {
    top: isize,
    start: isize,
    p: Point,
    reply: mpsc::SyncSender<Vec<Rect>>,
}

struct CacheEntry {
    top: isize,
    p: Point,
    at: Instant,
    rects: Vec<Rect>,
}

static BUSY: AtomicBool = AtomicBool::new(false);
static CACHE: Mutex<VecDeque<CacheEntry>> = Mutex::new(VecDeque::new());

fn cached(top: isize, p: Point) -> Option<Vec<Rect>> {
    let g = CACHE.lock().ok()?;
    g.iter().rev().find(|e| e.top == top && e.p == p && e.at.elapsed() < CACHE_TTL).map(|e| e.rects.clone())
}

fn store(top: isize, p: Point, rects: &[Rect]) {
    if let Ok(mut g) = CACHE.lock() {
        g.retain(|e| !(e.top == top && e.p == p) && e.at.elapsed() < CACHE_TTL);
        if g.len() >= CACHE_LEN {
            g.pop_front();
        }
        g.push_back(CacheEntry { top, p, at: Instant::now(), rects: rects.to_vec() });
    }
}

fn sender() -> Option<Sender<Req>> {
    static TX: OnceLock<Mutex<Option<Sender<Req>>>> = OnceLock::new();
    let slot = TX.get_or_init(|| Mutex::new(None));
    let mut g = slot.lock().ok()?;
    if let Some(tx) = g.as_ref() {
        return Some(tx.clone());
    }
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new().name("snapforge-uia".into()).spawn(move || run(rx));
    if let Err(e) = spawned {
        log::warn!("UI Automation worker failed to start: {e}");
        return None;
    }
    *g = Some(tx.clone());
    Some(tx)
}

fn uia_lookup(top: isize, start: isize, p: Point) -> Vec<Rect> {
    if let Some(r) = cached(top, p) {
        return r;
    }
    if BUSY.load(Ordering::Acquire) {
        return Vec::new();
    }
    let Some(tx) = sender() else { return Vec::new() };
    let (rtx, rrx) = mpsc::sync_channel(1);
    BUSY.store(true, Ordering::Release);
    if tx.send(Req { top, start, p, reply: rtx }).is_err() {
        BUSY.store(false, Ordering::Release);
        return Vec::new();
    }
    match rrx.recv_timeout(UIA_WAIT) {
        Ok(r) => r,
        Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => Vec::new(),
    }
}

struct Uia {
    auto: IUIAutomation,
    cache: IUIAutomationCacheRequest,
    cond: IUIAutomationCondition,
}

impl Uia {
    fn new() -> windows::core::Result<Self> {
        // SAFETY: COM is initialized (MTA) on this thread.
        unsafe {
            let auto: IUIAutomation = CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)?;
            if let Ok(a2) = windows::core::Interface::cast::<IUIAutomation2>(&auto) {
                // Bound how long an unresponsive provider can stall us.
                let _ = a2.SetConnectionTimeout(1000);
                let _ = a2.SetTransactionTimeout(1000);
            }
            let cache = auto.CreateCacheRequest()?;
            cache.AddProperty(UIA_BoundingRectanglePropertyId)?;
            let cond = auto.ControlViewCondition()?;
            Ok(Self { auto, cache, cond })
        }
    }

    /// Descend from the element of window `start` through children containing `p`.
    fn chain(&self, start: isize, p: Point) -> windows::core::Result<Vec<Rect>> {
        let mut out = Vec::new();
        // SAFETY: plain UIA calls on live objects created on this thread.
        unsafe {
            let mut el = self.auto.ElementFromHandleBuildCache(win::hwnd_from_isize(start), &self.cache)?;
            for _ in 0..MAX_DEPTH {
                let kids = el.FindAllBuildCache(TreeScope_Children, &self.cond, &self.cache)?;
                let n = kids.Length()?.min(MAX_CHILDREN);
                let mut best = None;
                for i in 0..n {
                    let Ok(k) = kids.GetElement(i) else { continue };
                    let Ok(r) = k.CachedBoundingRectangle() else { continue };
                    let r = win::rect_from_win(&r);
                    if r.contains(p) && best.as_ref().map(|(_, b): &(_, Rect)| r.area() < b.area()).unwrap_or(true) {
                        best = Some((k, r));
                    }
                }
                match best {
                    Some((k, r)) => {
                        out.push(r);
                        el = k;
                    }
                    None => break,
                }
            }
        }
        Ok(out)
    }
}

fn run(rx: Receiver<Req>) {
    // SAFETY: COM init for this thread only; balanced at exit.
    let inited = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
    let mut uia: Option<Uia> = None;
    loop {
        let first = if uia.is_some() {
            match rx.recv_timeout(IDLE_RELEASE) {
                Ok(r) => r,
                Err(RecvTimeoutError::Timeout) => {
                    uia = None;
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match rx.recv() {
                Ok(r) => r,
                Err(_) => break,
            }
        };
        // Only the newest request matters; older ones are answered by dropping them.
        let mut req = first;
        while let Ok(r) = rx.try_recv() {
            req = r;
        }
        if uia.is_none() {
            match Uia::new() {
                Ok(u) => uia = Some(u),
                Err(e) => log::warn!("UI Automation unavailable: {e}"),
            }
        }
        let rects = match &uia {
            Some(u) => u.chain(req.start, req.p).unwrap_or_else(|e| {
                log::trace!("UIA chain failed: {e}");
                Vec::new()
            }),
            None => Vec::new(),
        };
        store(req.top, req.p, &rects);
        BUSY.store(false, Ordering::Release);
        let _ = req.reply.try_send(rects);
    }
    drop(uia);
    if inited {
        // SAFETY: balances CoInitializeEx; all COM objects are dropped.
        unsafe { CoUninitialize() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_is_clipped_sorted_and_deduped() {
        let w = Rect::new(0, 0, 100, 100);
        let p = Point::new(50, 50);
        let rects = vec![
            Rect::new(40, 40, 20, 20),
            Rect::new(-10, 0, 200, 80), // clipped to (0,0,100,80)
            Rect::new(40, 40, 20, 20),  // duplicate
            Rect::new(0, 0, 10, 10),    // does not contain p
            Rect::new(0, 0, 100, 100),  // same as window
        ];
        let c = normalize_chain(w, rects, p);
        assert_eq!(c, vec![w, Rect::new(0, 0, 100, 80), Rect::new(40, 40, 20, 20)]);
    }
}
