//! Tools: combine images, images to PDF, batch print.
//!
//! Combine is pure (no UI). PDF writes a file. Batch print shows the print dialog once.

pub mod batch;
pub mod combine;
pub mod pdf;
pub mod split;

#[cfg(test)]
mod tests;

// Entry points used by the Tools dialogs.
pub use batch::batch_print;
pub use combine::combine_images;
pub use pdf::images_to_pdf;
