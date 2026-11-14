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

Later phases add `pdf-writer`, `zip`, `gif`/`gifski` and `mp3lame-encoder`. Each will be justified here when it is added.

## Milestones

### Phase 1: core capture
- [x] Project skeleton, release profile, static CRT, manifest (PerMonitorV2, long paths), version resource, icon
- [ ] Settings (TOML, portable mode, DPAPI-protected passwords), rotating log
- [ ] File-name templating with tests
- [ ] Capture backends: WGC (monitor and window, borderless, HDR → SDR), DXGI duplication, GDI; window enumeration
- [ ] Capture worker thread
- [ ] Overlays: rectangle (loupe, readout, Tab corner, Ctrl+Z, nudges), freehand, window/object (hover, Ctrl multi-select, wheel to change level), fixed-size (presets, Shift+arrows)
- [ ] Full screen (active or all monitors), active window
- [ ] Scrolling capture (auto-scroll, overlap detection, sticky header/footer, custom area, horizontal) with stitch tests
- [ ] Delay countdown, repeat last capture, auto capture at an interval
- [ ] Hotkeys (reassignable, conflict detection, PrtSc override via a low-level hook)
- [ ] Tray icon and menu; floating toolbar (drag, auto-hide, big buttons, lock, custom icons, context menu)
- [ ] Outputs: editor, clipboard (DIB + PNG), file, printer, email (MAPI), Word/Excel/PowerPoint (COM), OneNote, FTP/SFTP, external program, auto-copy, timestamp, preview-first
- [ ] Minimal editor (tabs, zoom, save, copy) as the Phase 2 foundation
- [ ] CLI automation and single-instance forwarding
- [ ] `--bench`

### Phase 2: editor · Phase 3: draw · Phase 4: recorder · Phase 5: pin/OCR/tools · Phase 6: settings/packaging
These follow the feature spec. A checklist will be added at the start of each phase.
