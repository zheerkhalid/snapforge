//! Scroll loop tests with a fake driver that serves a synthetic page.

use super::stitch::tests::{noise_page, text_page};
use super::*;

/// A fake scrollable view over `page`, with optional sticky header/footer, a smooth-scroll
/// animation (intermediate frames), and an optional stop after `cancel_after` grabs.
struct FakeDriver {
    page: Image,
    header: Option<Image>,
    footer: Option<Image>,
    view_w: u32,
    view_h: u32,
    x: i32,
    y: i32,
    /// Pixels per wheel notch (x, y).
    ppn: (i32, i32),
    wheel_works: bool,
    page_works: bool,
    /// Intermediate positions served after each scroll (smooth scrolling).
    anim: Vec<(i32, i32)>,
    clock: f64,
    grabs: u32,
    cancel_after: Option<u32>,
    wheel_calls: u32,
    page_calls: u32,
}

impl FakeDriver {
    fn new(page: Image, view_w: u32, view_h: u32) -> Self {
        Self {
            page,
            header: None,
            footer: None,
            view_w,
            view_h,
            x: 0,
            y: 0,
            ppn: (33, 40),
            wheel_works: true,
            page_works: false,
            anim: Vec::new(),
            clock: 0.0,
            grabs: 0,
            cancel_after: None,
            wheel_calls: 0,
            page_calls: 0,
        }
    }

    fn move_to(&mut self, nx: i32, ny: i32) {
        let nx = nx.clamp(0, (self.page.width - self.view_w) as i32);
        let ny = ny.clamp(0, (self.page.height - self.view_h) as i32);
        // Smooth scrolling: two intermediate frames before the final position.
        self.anim = vec![
            (self.x + (nx - self.x) * 2 / 3, self.y + (ny - self.y) * 2 / 3),
            (self.x + (nx - self.x) / 3, self.y + (ny - self.y) / 3),
        ];
        self.x = nx;
        self.y = ny;
    }

    fn view_at(&self, x: i32, y: i32) -> Image {
        let content = self.page.crop(Rect::new(x, y, self.view_w as i32, self.view_h as i32));
        let hh = self.header.as_ref().map_or(0, |h| h.height);
        let fh = self.footer.as_ref().map_or(0, |f| f.height);
        let mut out = Image::new(self.view_w, hh + self.view_h + fh);
        if let Some(h) = &self.header {
            out.blit(h, 0, 0);
        }
        out.blit(&content, 0, hh as i32);
        if let Some(f) = &self.footer {
            out.blit(f, 0, (hh + self.view_h) as i32);
        }
        out
    }
}

impl ScrollDriver for FakeDriver {
    fn grab(&mut self) -> Result<Image, CaptureError> {
        self.grabs += 1;
        self.clock += 5.0;
        let (x, y) = self.anim.pop().unwrap_or((self.x, self.y));
        Ok(self.view_at(x, y))
    }
    fn wheel(&mut self, axis: Axis, notches: i32) {
        self.wheel_calls += 1;
        if !self.wheel_works {
            return;
        }
        match axis {
            Axis::Vertical => self.move_to(self.x, self.y + notches * self.ppn.1),
            Axis::Horizontal => self.move_to(self.x + notches * self.ppn.0, self.y),
        }
    }
    fn can_page(&self) -> bool {
        self.page_works
    }
    fn page(&mut self, axis: Axis, forward: bool) {
        self.page_calls += 1;
        if !self.page_works {
            return;
        }
        let s = if forward { 1 } else { -1 };
        match axis {
            Axis::Vertical => self.move_to(self.x, self.y + s * (self.view_h as i32 - 20)),
            Axis::Horizontal => self.move_to(self.x + s * (self.view_w as i32 - 20), self.y),
        }
    }
    fn cancelled(&mut self) -> bool {
        self.cancel_after.is_some_and(|n| self.grabs >= n)
    }
    fn sleep_ms(&mut self, ms: u32) {
        self.clock += ms as f64;
    }
    fn now_ms(&mut self) -> f64 {
        self.clock
    }
}

fn cfg(direction: ScrollDirection) -> LoopConfig {
    let s = ScrollSettings { direction, max_screens: 60, step_delay_ms: 60 };
    LoopConfig::new(&s, 0)
}

#[test]
fn vertical_full_page() {
    let page = text_page(220, 2400, 101, &[(700, 90)]);
    let mut d = FakeDriver::new(page.clone(), 220, 300);
    let out = run_loop(&mut d, &cfg(ScrollDirection::Vertical)).expect("capture");
    assert_eq!(out, page);
    // Probe notch, then ~75 % steps: far fewer wheel calls than 2400/40 notches.
    assert!(d.wheel_calls < 20, "{} wheel calls", d.wheel_calls);
}

#[test]
fn vertical_sticky_bands_and_start_mid_page() {
    let page = text_page(200, 1800, 103, &[]);
    let header = noise_page(200, 36, 104);
    let footer = noise_page(200, 22, 105);
    let mut d = FakeDriver::new(page.clone(), 200, 260);
    d.header = Some(header.clone());
    d.footer = Some(footer.clone());
    d.y = 500; // capture starts at the current scroll position
    let out = run_loop(&mut d, &cfg(ScrollDirection::Vertical)).expect("capture");
    let content = page.crop(Rect::new(0, 500, 200, 1300));
    let mut expected = Image::new(200, 36 + 1300 + 22);
    expected.blit(&header, 0, 0);
    expected.blit(&content, 0, 36);
    expected.blit(&footer, 0, 36 + 1300);
    assert_eq!(out, expected);
}

#[test]
fn horizontal_page() {
    let page = noise_page(1700, 150, 107);
    let mut d = FakeDriver::new(page.clone(), 320, 150);
    let out = run_loop(&mut d, &cfg(ScrollDirection::Horizontal)).expect("capture");
    assert_eq!(out, page);
}

#[test]
fn wheel_ignored_falls_back_to_page_scrolling() {
    let page = text_page(180, 1500, 109, &[]);
    let mut d = FakeDriver::new(page.clone(), 180, 300);
    d.wheel_works = false;
    d.page_works = true;
    let out = run_loop(&mut d, &cfg(ScrollDirection::Vertical)).expect("capture");
    assert_eq!(out, page);
    assert!(d.page_calls > 0);
}

#[test]
fn not_scrollable_returns_one_screen() {
    let page = text_page(180, 300, 111, &[]);
    let mut d = FakeDriver::new(page.clone(), 180, 300);
    let out = run_loop(&mut d, &cfg(ScrollDirection::Vertical)).expect("capture");
    assert_eq!(out, page);
}

#[test]
fn max_screens_is_respected() {
    let page = noise_page(100, 5000, 113);
    let mut d = FakeDriver::new(page.clone(), 100, 200);
    let mut c = cfg(ScrollDirection::Vertical);
    c.max_screens = 4;
    let out = run_loop(&mut d, &c).expect("capture");
    assert!(out.height > 200 && out.height < 1000, "height {}", out.height);
    assert_eq!(out, page.crop(Rect::new(0, 0, 100, out.height as i32)));
}

#[test]
fn stop_keeps_what_was_captured() {
    let page = noise_page(120, 4000, 115);
    let mut d = FakeDriver::new(page.clone(), 120, 250);
    d.cancel_after = Some(14);
    let out = run_loop(&mut d, &cfg(ScrollDirection::Vertical)).expect("partial capture");
    assert!(out.height > 250 && out.height < 4000, "height {}", out.height);
    assert_eq!(out, page.crop(Rect::new(0, 0, 120, out.height as i32)));
}

#[test]
fn immediate_stop_is_cancelled() {
    let page = noise_page(120, 1000, 117);
    let mut d = FakeDriver::new(page, 120, 250);
    d.cancel_after = Some(0);
    assert!(matches!(run_loop(&mut d, &cfg(ScrollDirection::Vertical)), Err(CaptureError::Cancelled)));
    let mut d = FakeDriver::new(noise_page(120, 1000, 118), 120, 250);
    d.cancel_after = Some(2); // stopped before any scrolled content arrived
    assert!(matches!(run_loop(&mut d, &cfg(ScrollDirection::Vertical)), Err(CaptureError::Cancelled)));
}

#[test]
fn both_directions() {
    let page = noise_page(700, 1100, 119);
    let mut d = FakeDriver::new(page.clone(), 250, 300);
    let out = run_loop(&mut d, &cfg(ScrollDirection::Both)).expect("capture");
    assert_eq!(out, page);
}

#[test]
fn settle_waits_for_stable_frames() {
    // With smooth scrolling every step serves two moving frames first; the result must
    // still be exact (asserted in the tests above). Here: the settle grabs are counted.
    let page = text_page(150, 900, 121, &[]);
    let mut d = FakeDriver::new(page, 150, 300);
    let c = cfg(ScrollDirection::Vertical);
    let _ = run_loop(&mut d, &c).expect("capture");
    assert!(d.clock >= 60.0);
    assert!(d.grabs >= 8);
}
