//! Tools: combine images, images to PDF, batch print.
//!
//! Combine is pure (no UI). PDF writes a file. Batch print shows the print dialog once.

pub mod batch;
pub mod combine;
pub mod pdf;
pub mod split;

#[cfg(test)]
mod tests;

// Convenience re-exports for the UI; unused until the Tools menu is wired up.
#[allow(unused_imports)]
pub use batch::batch_print;
#[allow(unused_imports)]
pub use combine::{combine_files, combine_images, CombineItem, CombineParams};
#[allow(unused_imports)]
pub use pdf::{images_to_pdf, PdfOptions, PdfSource};
