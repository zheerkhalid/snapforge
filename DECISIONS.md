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
11. **Popup menus are tracked with `TPM_RETURNCMD`, owned by the tray's hidden top-level window** (`tray::menu`). The tray and the toolbar share one menu builder and dispatcher. After a menu closes, focus goes back to the window that was in the foreground, so "Capture Active Window" from a menu still targets the user's window.
12. **The PrtSc hook masks swallowed Win/Alt combos** by injecting an unassigned key (VK 0xE8), like AutoHotkey does. Without it, releasing Win would open the Start menu, and releasing Alt would activate the focused window's menu bar. A plain PrtSc also uses the hook, so Windows does not open the Snipping Tool.
13. **Toolbar rendering:** tiny-skia draws into a 32-bit DIB back buffer. Icons are rendered once per size and theme into premultiplied BGRA DIBs and composited with `AlphaBlend`. Only the delay badge digits use GDI text. Auto-hide polls the cursor every 100 ms, and only while the toolbar is docked, shown and the cursor is outside its client area. The slide animation timer runs only while the toolbar moves.
