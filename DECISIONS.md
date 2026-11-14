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
11. **Clipboard images are written three ways:** CF_DIBV5 (32-bit BI_BITFIELDS, straight alpha, bottom-up for compatibility), CF_DIB (24-bit composited over white, for apps that misread 32-bit DIBs) and the registered "PNG" format (transparency for Office and browsers). Pasting prefers PNG, then DIBV5, then DIB; a 32-bit DIB whose alpha is all zero is treated as opaque.
12. **SFTP is key-only.** `sftp.exe` runs with `BatchMode=yes`, which disables password prompts, so a stored password is ignored for SFTP (a warning is logged). Passing passwords to OpenSSH would need an askpass helper and is left for later.
13. **FTP passive mode connects the data channel to the control connection's address**, ignoring the IP in the PASV reply (servers behind NAT often report a private address). EPSV is tried first.
14. **Printing scales the image to fit the printable area** (up or down), centred, aspect ratio kept, composited over white, with HALFTONE stretching.
