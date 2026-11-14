//! Keyboard state machine for a pin: which command a key triggers in each mode, and the
//! mode that follows. Pure, so the Space/Enter behaviours are unit-tested.

use crate::settings::pin::{EnterAction, SpaceAction};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Drawing,
    /// Text entry inside Draw Mode.
    Typing,
    /// Collapsed into a tab at the top edge; `drawing` = Draw Mode was on before.
    Tab {
        drawing: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Space,
    Enter,
    Escape,
    /// The D key (Draw Mode toggle).
    D,
    CtrlZ,
    CtrlC,
    CtrlS,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    None,
    HideToTab,
    RestoreFromTab,
    ActualSize,
    EnterDraw,
    ExitDraw,
    CopyAndClose,
    SaveAndClose,
    CommitText,
    CancelText,
    Close,
    Undo,
    Copy,
    SaveAs,
}

/// The command `key` triggers in `mode`.
pub fn on_key(mode: Mode, key: Key, space: SpaceAction, enter: EnterAction) -> Cmd {
    match mode {
        Mode::Typing => match key {
            Key::Enter => Cmd::CommitText,
            Key::Escape => Cmd::CancelText,
            // Space and D are typed as characters (WM_CHAR); shortcuts stay quiet.
            _ => Cmd::None,
        },
        Mode::Tab { .. } => match key {
            Key::Space | Key::Enter => Cmd::RestoreFromTab,
            Key::Escape => Cmd::Close,
            _ => Cmd::None,
        },
        Mode::Normal | Mode::Drawing => {
            let drawing = mode == Mode::Drawing;
            let toggle_draw = if drawing { Cmd::ExitDraw } else { Cmd::EnterDraw };
            match key {
                Key::Space => match space {
                    SpaceAction::HideToTab => Cmd::HideToTab,
                    SpaceAction::ActualSize => Cmd::ActualSize,
                    SpaceAction::ToggleDraw => toggle_draw,
                },
                Key::Enter => match enter {
                    EnterAction::CopyAndClose => Cmd::CopyAndClose,
                    EnterAction::SaveAndClose => Cmd::SaveAndClose,
                    EnterAction::Nothing => Cmd::None,
                },
                Key::Escape => Cmd::Close,
                Key::D => toggle_draw,
                Key::CtrlZ => Cmd::Undo,
                Key::CtrlC => Cmd::Copy,
                Key::CtrlS => Cmd::SaveAs,
            }
        }
    }
}

/// The mode after `cmd` ran in `mode` (commands that close the pin leave it unchanged).
pub fn next_mode(mode: Mode, cmd: Cmd) -> Mode {
    match (mode, cmd) {
        (Mode::Normal, Cmd::EnterDraw) => Mode::Drawing,
        (Mode::Drawing, Cmd::ExitDraw) => Mode::Normal,
        (Mode::Typing, Cmd::CommitText | Cmd::CancelText) => Mode::Drawing,
        // Exiting Draw Mode while typing (menu) commits first, then leaves.
        (Mode::Typing, Cmd::ExitDraw) => Mode::Normal,
        (Mode::Normal, Cmd::HideToTab) => Mode::Tab { drawing: false },
        (Mode::Drawing, Cmd::HideToTab) => Mode::Tab { drawing: true },
        (Mode::Tab { drawing }, Cmd::RestoreFromTab) => {
            if drawing {
                Mode::Drawing
            } else {
                Mode::Normal
            }
        }
        (m, _) => m,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(start: Mode, keys: &[Key], space: SpaceAction, enter: EnterAction) -> (Mode, Vec<Cmd>) {
        let mut m = start;
        let mut cmds = Vec::new();
        for &k in keys {
            let c = on_key(m, k, space, enter);
            m = next_mode(m, c);
            cmds.push(c);
        }
        (m, cmds)
    }

    #[test]
    fn space_hide_to_tab_round_trip() {
        let (m, c) = run(Mode::Normal, &[Key::Space, Key::Space], SpaceAction::HideToTab, EnterAction::Nothing);
        assert_eq!(c, vec![Cmd::HideToTab, Cmd::RestoreFromTab]);
        assert_eq!(m, Mode::Normal);
        // Draw Mode survives the tab.
        let (m, _) = run(Mode::Drawing, &[Key::Space], SpaceAction::HideToTab, EnterAction::Nothing);
        assert_eq!(m, Mode::Tab { drawing: true });
        let (m, _) = run(m, &[Key::Enter], SpaceAction::HideToTab, EnterAction::CopyAndClose);
        assert_eq!(m, Mode::Drawing);
    }

    #[test]
    fn space_actual_size_and_toggle_draw() {
        assert_eq!(on_key(Mode::Normal, Key::Space, SpaceAction::ActualSize, EnterAction::Nothing), Cmd::ActualSize);
        let (m, c) = run(Mode::Normal, &[Key::Space, Key::Space], SpaceAction::ToggleDraw, EnterAction::Nothing);
        assert_eq!(c, vec![Cmd::EnterDraw, Cmd::ExitDraw]);
        assert_eq!(m, Mode::Normal);
    }

    #[test]
    fn enter_actions() {
        let sp = SpaceAction::HideToTab;
        assert_eq!(on_key(Mode::Normal, Key::Enter, sp, EnterAction::CopyAndClose), Cmd::CopyAndClose);
        assert_eq!(on_key(Mode::Drawing, Key::Enter, sp, EnterAction::SaveAndClose), Cmd::SaveAndClose);
        assert_eq!(on_key(Mode::Normal, Key::Enter, sp, EnterAction::Nothing), Cmd::None);
    }

    #[test]
    fn typing_captures_keys() {
        let sp = SpaceAction::ToggleDraw;
        let en = EnterAction::CopyAndClose;
        // Space and D type characters; Enter commits instead of copy-and-close.
        assert_eq!(on_key(Mode::Typing, Key::Space, sp, en), Cmd::None);
        assert_eq!(on_key(Mode::Typing, Key::D, sp, en), Cmd::None);
        let (m, c) = run(Mode::Typing, &[Key::Enter], sp, en);
        assert_eq!((m, c[0]), (Mode::Drawing, Cmd::CommitText));
        let (m, c) = run(Mode::Typing, &[Key::Escape], sp, en);
        assert_eq!((m, c[0]), (Mode::Drawing, Cmd::CancelText));
    }

    #[test]
    fn escape_and_d() {
        let sp = SpaceAction::HideToTab;
        let en = EnterAction::Nothing;
        assert_eq!(on_key(Mode::Normal, Key::Escape, sp, en), Cmd::Close);
        assert_eq!(on_key(Mode::Drawing, Key::Escape, sp, en), Cmd::Close);
        assert_eq!(on_key(Mode::Tab { drawing: false }, Key::Escape, sp, en), Cmd::Close);
        let (m, _) = run(Mode::Normal, &[Key::D], sp, en);
        assert_eq!(m, Mode::Drawing);
        let (m, _) = run(m, &[Key::D], sp, en);
        assert_eq!(m, Mode::Normal);
        assert_eq!(on_key(Mode::Tab { drawing: false }, Key::D, sp, en), Cmd::None);
    }
}
