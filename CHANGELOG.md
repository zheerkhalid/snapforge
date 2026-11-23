# Changelog

All notable changes to SnapForge are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [0.2.0] - 2026-11-24

### Added

- **Beautify** (Effects menu, the ✨ button, or `Ctrl+B`): puts a screenshot on a padded
  background with rounded corners and a soft shadow, in one step. Choose one of eight
  gradients, a solid color or a transparent background, and optionally pad the result to
  16:9, 4:3, 1:1, 3:4 or 9:16. The preview updates as you change the values.
- **Hide Private Details** (Tools menu, the 🙈 button, or `Ctrl+Shift+H`): SnapForge reads
  the text in the picture and covers every e-mail address, phone number and payment card
  number it finds, with pixels, a blur or a black bar. Each cover is a normal annotation
  that you can move, resize or delete, and one Undo removes them all. With a selection,
  only the selection is checked. Everything happens on your PC.
- **Read QR Code** (Tools menu, the 🔳 button, or `Ctrl+Shift+Q`): shows what the QR codes
  in the picture (or in the selection) say, with Copy and Open Link buttons.
- **Capture Text also reads QR codes**: when the captured area contains a QR code, its
  content appears above the recognized text. This works even when no OCR language is
  installed.
- **Capture History** (Tools menu, tray menu, or `Ctrl+H` in the editor): the most recent captures are kept and can
  be found again, including captures that only went to the clipboard. Search by date,
  size, capture type, file name or by words that appear in the picture. Open a capture
  in the editor, copy it, show it in its folder or remove it. The number of captures kept
  (50 by default) and an off switch are in Settings ▸ Capture History.
- **Right-to-left names**: Kurdish, Arabic, Persian and Hebrew file names are drawn
  correctly (joined letters, right order) on the editor's tabs and in Capture History.
- A **Share-ready** group on the editor toolbar with Beautify, Hide Private Details and
  Read QR Code.
- `CHANGELOG.md` (this file).

### Changed

- **Exit asks first** when pictures in the editor have changes that were never saved.
  Closing the editor window only hides it, so these pictures used to be lost without a
  word when you chose Exit.
- Cropping a picture that has annotations is now **one Undo step** (it used to take two,
  and after the first one the annotations sat in the wrong place).
- A capture started from the command line that opens the editor now leaves a complete
  SnapForge running (tray icon, toolbar and hotkeys), not a half-started one.
- Relative paths given on the command line (`--open a.png`, `--file shot.png`) are
  resolved in the folder you ran the command from, also when SnapForge is already running.
- `--file` with a name that is not an image file (no or unknown extension) is rejected
  with a message, instead of saving nothing and reporting success.
- `scripts/package.ps1` and the installer script use `CARGO_TARGET_DIR` when it is set.

### Fixed

- **Captures taken within the same second no longer replace each other.** With the
  default file name (one name per second), repeated or scripted captures could get the
  same name, and the later one silently replaced the earlier one.
- **Effects on a pasted or moved piece that hangs over the edge of the picture** did
  nothing ("no change"). The selection is now cut to the picture when the piece is merged.
- **Edit ▸ Paste, the right-click Paste and the command palette** could not paste copied
  annotations (only `Ctrl+V` could).
- **Settings could be saved out of order** when several changes came close together, which
  could leave an older state on disk. Settings are also written when Windows shuts down or
  you sign out.
- **Capture Text window**: `Esc` closes the window again after you clicked Copy All, Copy
  Selected or Keep line breaks, and when recognition failed. A maximized window no longer
  overwrites the remembered window size.
- The `###` counter in file names no longer skips numbers when a capture is sent by
  e-mail, to an Office program or uploaded, or when a pin's Save As dialog is cancelled.
- User guide: the Hotkeys and File names sections described editing `settings.toml`;
  they now describe the Settings window. The README said full screen captures the
  "active" monitor; it is the monitor under the pointer.

## [0.1.3] - 2026-11-21

### Added

- Right-click the picture in the editor to copy it, paste, crop, save or pin it.
- Scrolling capture starts from the top of the page (can be turned off).

### Fixed

- Scrolling capture in browsers: a scroll that arrived late could join two screens with a
  red line. SnapForge now waits for the page to move and come to rest before each stitch.

## [0.1.2] - 2026-11-15

### Added

- The pointer shows the size of the drawing tool (ring, stamp outline or stroke dot).
- A size slider in the tool bar, and `[` / `]` or `Alt` + wheel to change the size.

### Changed

- Pencil, highlighter and eraser strokes are no longer selected after drawing, so the next
  stroke can start on top of the last one.

## [0.1.1] - 2026-11-15

### Added

- Drop pictures, `.sfcap` projects or videos on the capture toolbar to open them.
- *Toolbar Always Visible* option.
- *Also Save to File (Auto-Save)*: every capture is saved to the output folder, whatever
  the destination.

## [0.1.0] - 2026-11-15

First release: screen capture (region, window, freehand, scrolling, fixed size), image
editor with annotations and effects, screen recorder and video editor, pin to screen,
Capture Text (OCR), screen tools, installer and portable ZIP.

[0.2.0]: https://github.com/zheerkhalid/snapforge/releases/tag/v0.2.0
[0.1.3]: https://github.com/zheerkhalid/snapforge/releases/tag/v0.1.3
[0.1.2]: https://github.com/zheerkhalid/snapforge/releases/tag/v0.1.2
[0.1.1]: https://github.com/zheerkhalid/snapforge/releases/tag/v0.1.1
[0.1.0]: https://github.com/zheerkhalid/snapforge/releases/tag/v0.1.0
