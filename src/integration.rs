//! Dispatch of Phase 2-6 commands to their modules (UI thread).

use crate::app::{self, AppCommand, ToolWindow};
use crate::i18n::t;

pub fn execute(cmd: AppCommand) {
    match cmd {
        AppCommand::PinImage(img) => crate::pin::pin_image(img, None),
        AppCommand::PinFromClipboard => match crate::output::clipboard::paste_image() {
            Some(img) => crate::pin::pin_image(img, None),
            None => app::notify(t("app-name"), t("pin-clipboard-empty"), false),
        },
        AppCommand::PinCapture => {
            if let Some((img, rect)) = crate::controller::select_region_image() {
                crate::pin::pin_image(img, Some(rect));
            }
        }
        AppCommand::CaptureText => {
            if let Some((img, _)) = crate::controller::select_region_image() {
                crate::ocr::capture_text_flow(img);
            }
        }
        AppCommand::ScreenTool(t) => crate::tools::open(t),
        AppCommand::OpenSettings | AppCommand::OpenTool(ToolWindow::Settings) => crate::editor::open_settings(),
        AppCommand::About => crate::editor::open_about(),
        AppCommand::OpenTool(ToolWindow::CombineImages) => crate::editor::open_combine_images(),
        AppCommand::OpenTool(ToolWindow::ImagesToPdf) => crate::editor::open_images_to_pdf(),
        AppCommand::OpenTool(ToolWindow::BatchPrint) => crate::editor::open_batch_print(),
        _ => log::info!("command not available yet"),
    }
}
