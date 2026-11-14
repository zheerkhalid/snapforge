//! Shared building blocks: geometry, image buffers, Win32 helpers, time, rng.
pub mod geom;
pub mod img;
pub mod rng;
pub mod time;
pub mod win;

pub use geom::{Point, Rect};
pub use img::Image;
