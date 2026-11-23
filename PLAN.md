# SnapForge: Plan

Developer: [Zheer Khalid](https://zheer.dev)

## Crate layout

A single binary crate (`SnapForge.exe`). Modules:

```
build.rs            icon (vector-rendered), manifest, version resource
src/
  main.rs           entry point, CLI, single-instance forwarding
  app.rs            UI-thread state machine and message loop
  cli.rs            command-line parsing
  bench.rs          `--bench` performance budget report
  logging.rs        rotating file logger
  i18n.rs           Fluent (.ftl) string lookup
  common/           geometry, BGRA image buffer, Win32 helpers, time, rng
  settings/         TOML settings, portable mode, file-name templates
  hotkeys/          RegisterHotKey + low-level hook thread for PrtSc
  tray/             notification-area icon and menu
  toolbar/          floating native capture toolbar and its icons
  capture/          WGC, DXGI duplication, GDI, window enumeration, scrolling and stitching, worker
  overlay/          region / freehand / window-object / fixed-size overlays, countdown
  output/           clipboard, save/encode, print, email, Office COM, FTP/SFTP, external program, stamp
  editor/           egui editor (lazy, hidden and reused)
  draw/ effects/ recorder/ video_editor/ ocr/ pin/ tools/   later phases
locales/en-US/main.ftl
installer/          Inno Setup script
docs/USAGE.md
```

## Dependencies

| Crate | Version | Why | Approx. binary cost |
|---|---|---|---|
| windows | 0.58 | All Win32/WinRT/COM access (WGC, DXGI, GDI, shell, COM) | only what is used |
| eframe / egui (glow) | 0.31 | Editor, settings and dialog UI | ~4–5 MB, needed for the UI |
| winit | 0.30 | `with_any_thread` so the editor can run on its own thread | (shared with eframe) |
| tiny-skia | 0.11 | Anti-aliased CPU rendering for icons and annotations | ~0.5 MB |
| image | 0.25 (png, jpeg, bmp, gif, tiff, tga) | Encoders and decoders | ~1.5 MB, needed for the formats |
| webp | 0.3 | libwebp, statically linked, for lossy WebP | ~0.5 MB |
| serde, toml, serde_json | 1 / 0.8 / 1 | Settings and `.sfcap` metadata | small |
| anyhow, thiserror, log | 1 / 2 / 0.4 | Error handling and logging | small |

| pdf-writer | 0.12 | Images to PDF (raw PDF objects only) | ~0.1 MB |
| miniz_oxide | 0.8 | PDF Flate streams (already in the tree via PNG) | 0 |
| zip | 4 (deflate only) | `.sfcap` projects | ~0.1 MB |
| gif | 0.14 | Animated GIF export (already in the tree via `image`) | 0 |
| windows-core | 0.58 | `#[implement]` for COM callbacks (already in the tree) | 0 |
| rqrr | 0.10 (no `img` feature) | QR code reading in Capture Text and the editor | ~0.1 MB |

Not needed in the end: `mp3lame-encoder` (Windows' built-in MP3 encoder MFT is used), `gifski` (GPL licensing and about 1 MB), and `cosmic-text` (DirectWrite renders text instead, adding nothing to the binary). The release EXE is **12.0 MB**.

## Milestones

Each phase was built by parallel agents, each in its own git worktree, against shared contracts (`app.rs` command bus, `settings/`, module stubs), then merged and integrated. Every merge passed `cargo build`, `cargo test`, `cargo clippy --all-targets -D warnings` and `cargo fmt`.

### Phase 1: core capture ✅
- [x] Skeleton, release profile, static CRT, manifest (PerMonitorV2, long paths), version resource, generated icon
- [x] Settings (TOML, portable mode, DPAPI passwords), rotating log, Fluent strings
- [x] File-name templates (`$Y $M $D $H $N $S`, `###`, `@`, `%VARS%`), daily counter reset
- [x] Capture backends: WGC (borderless, HDR→SDR), DXGI duplication, GDI; z-order snapshot; UI Automation element chain
- [x] Overlays: region (loupe, Tab corner, nudges, Ctrl+Z), freehand, window/object (multi-select, levels), fixed size, scroll pick
- [x] Scrolling capture (adaptive wheel, sticky header/footer, horizontal, both directions)
- [x] Delay countdown, repeat last, auto capture
- [x] Hotkeys (PrtSc override hook on its own thread), tray, floating toolbar (auto-hide, big buttons, lock, custom buttons)
- [x] Outputs: editor, draw, clipboard, file, printer, email, Word/Excel/PowerPoint/OneNote, FTP/SFTP, program, stamp
- [x] CLI automation, single-instance forwarding, `--bench`

### Phase 2: editor ✅
- [x] Tabs and workspaces, memory-aware unlimited undo, menus, ribbon, command palette (Ctrl+K)
- [x] Selections (rect/freehand, move/copy, stats, edge trim/extend), crop, resize, canvas, rotate, flip
- [x] All image operations and effects with live preview; clone, strips, transparency
- [x] File commands (Save All, sequence save, delete to Recycle Bin, properties, recent multi-open), Edit With, WIA scanner with deskew
- [x] Tools: combine images, images to PDF, batch print

### Phase 3: draw ✅
- [x] 25 tools: text (curved/path/callout), lines (arrowheads, decor, dashes), shapes, highlighters, magnifier, steps, emoji, 3D arrows, pointers, blur/pixelate/eraser/fill/pencil
- [x] Multi-select, handles, align, z-order, presets, font preview, screen color pick, F-keys and Tab cycling
- [x] `.sfcap` projects with auto-save; golden-image tests

### Phase 4: recorder and video editor ✅
- [x] WGC capture, GPU NV12, hardware H.264/AAC MP4, WMV, MP3; mic + loopback mixing; webcam PiP
- [x] Control bar (excluded from capture), click highlights and sounds, F5 zoom, follow mouse, pause
- [x] Scheduled recording, crash recovery (fragmented MP4 + journal), Convert to MP4, audio extraction
- [x] Video editor: timeline, trim/cut, frame stepping, timed annotations, zoom/pan, title cards, GIF export, `.sfvid`

### Phase 5: pin, OCR and screen tools ✅
- [x] Pin to screen (zoom, opacity, draw mode, canvas expand, Space/Enter behaviours, send to editor)
- [x] OCR with Windows.Media.Ocr (language choice, tiling above 10 000 px)
- [x] Color picker, magnifier, crosshair, focus, calibrated ruler with protractor

### 0.2.0: share-ready and history ✅
- [x] Beautify effect (gradient / solid / transparent backdrop, padding, corners, shadow, fixed shapes)
- [x] Hide Private Details (OCR + e-mail / phone / card matchers → editable cover annotations)
- [x] QR code reading (editor command, Capture Text)
- [x] Capture History (store, search by text in the picture, window, tray entry)
- [x] Exit prompt for unsaved tabs, reserved output names, serialized settings saves, linked undo steps

### Phase 6: settings, polish and packaging ✅
- [x] Settings window (all tabs, hotkey conflict detection, import/export, per-tab reset, remembered placement)
- [x] Fluent localization, keyboard navigation and accessibility labels (AccessKit)
- [x] Inno Setup per-user installer, portable ZIP, `--uninstall-cleanup`, CI
