# Decisions

Each entry records a choice that the spec left open, or a place where the build departs from the spec, and why.

1. **`windows` 0.58 pinned.** The API is stable and well understood. Newer versions changed handle and `Option` signatures throughout. We will upgrade in one dedicated pass.
2. **eframe uses the `glow` (OpenGL) backend, not wgpu.** It starts faster and adds about 4 MB less to the binary.
3. **The editor runs on its own thread** (winit `with_any_thread`). The Win32 UI thread (tray, hotkeys, overlays) stays independent of the egui event loop, so cold start never pays for OpenGL initialization. The editor is created on first use, then hidden on close and shown again via `ShowWindow`, so later opens are instant.
4. **The PrtSc override runs a low-level keyboard hook on a dedicated thread.** It is never on the UI thread, so a busy UI can never stall system-wide keyboard input. Other hotkeys use `RegisterHotKey`.
5. **SFTP uses the OpenSSH client built into Windows** (`sftp.exe`, Windows 10 1809+) in batch mode with key-based authentication. A pure-Rust SSH stack would add several MB plus an async runtime. Plain FTP is a small built-in client (passive mode).
6. **OneNote:** its COM API needs page-XML plumbing, so OneNote output copies the image to the clipboard and activates OneNote. Word, Excel and PowerPoint use real COM automation (`IDispatch`).
7. **Passwords** (FTP) are stored encrypted with Windows DPAPI (per user), never in plain text in the TOML file.
8. **The app icon is drawn as vector art at build time** (`build.rs` + tiny-skia), so the repo has no binary icon assets and the branding is fully original.
9. **Capture overlays use a frozen screenshot.** The screen is captured first and the overlay paints that image, so menus and tooltips stay capturable and the selection shows exactly what will be saved.
10. **SnapForge's own windows use `WDA_EXCLUDEFROMCAPTURE`** (toolbar, countdown, scrolling indicator, recording bar), so they never show up in captures.
11. **The editor's event loop is created once and lives until exit.** winit cannot create a second event loop in one process, so closing the editor hides the window (`CancelClose` + `Visible(false)`). Other threads show it again with `ShowWindowAsync` + `SetForegroundWindow` (a hidden window gets no paint messages, so `update()` cannot do it), then `request_repaint()`. `ShowWindowAsync` never blocks the caller on the editor thread.
12. **Editor texture filtering is fixed per texture:** nearest-neighbour magnification, linear minification with mipmaps. This gives crisp pixels above 100% and smooth downscaling below it, without re-uploading when the zoom crosses 100%. Images are split into tiles of at most 8192 px (or the driver limit, if lower).
13. **Unnamed captures open as "dirty"** (never saved), so closing their tab asks Save / Don't Save / Cancel. Files opened from disk start clean.
14. **Editor Copy runs `clipboard::copy_image` on a worker thread**, because encoding the PNG clipboard format can take more than a frame for large images. This requires `copy_image` to work from any thread (`OpenClipboard(NULL)`, no delayed rendering).
