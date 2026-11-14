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
11. **Scrolling capture starts at the current scroll position.** It does not jump to the top first. The user chooses where the capture begins, and pages with lazy loading or scroll side effects are not disturbed. `Both` captures vertical columns and scrolls back up by the amount it moved down between columns.
12. **Stitch mismatch policy: append after a separator.** When a new frame does not line up with the previous one (content changed unexpectedly), the whole frame is appended after a dashed red separator, so nothing is lost and the problem is visible. The scroll loop stops after three mismatches in a row. `MismatchPolicy::Stop` is available for callers that would rather reject the frame.
13. **While scrolling, the cursor rests on the scrollbar side of the target**, and the matcher ignores a margin of about 20 px at each edge across the scroll axis. Hover effects in the content then stay constant, and the moving scrollbar thumb can't break the row matching. Scroll frames are always grabbed without the cursor.
