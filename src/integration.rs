//! Dispatch of Phase 2-6 commands to their modules (UI thread).

use crate::app::AppCommand;

pub fn execute(cmd: AppCommand) {
    let _ = cmd;
    log::info!("command not available yet");
}
