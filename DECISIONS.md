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
11. **Capture backends: Auto uses DXGI when warm, GDI when cold, and WGC for HDR and for windows.** Measured on the dev machine (release build; 1920×1200 and 2560×1440 SDR monitors). The machine was busy with parallel builds, so treat these numbers as upper bounds. Full primary monitor, in ms: GDI cold 30–50, warm 30. DXGI cold 38–56 (device and duplication creation), warm 3–4. WGC cold 128–195, warm 30–32 (warm was about 130 before the WinRT item factory and monitor items were cached). Virtual screen (4480×1443): GDI 39–58; DXGI cold 61, warm 7–10; WGC cold 171–186, warm 47–81. WGC window grab: 19–32 ms. GPU resources are released after 10 s idle, so most hotkey presses are cold. Auto still captures therefore use GDI when no duplication is cached. They also warm DXGI in the background for the grabs that follow (scrolling, repeats, auto capture), and use DXGI first while it is warm. If the rect touches any HDR monitor, WGC goes first. Window captures always try WGC `CreateForWindow` first (it works when the window is covered), then fall back to grabbing the bounds. All GPU objects live on one MTA worker thread, which drops them after 10 s idle or on `release_idle_resources`. Driver DLLs stay loaded after release. In the test process, private bytes went from 2 MB at start to 54 MB warm, then 24 MB after release; the D3D user-mode driver accounts for the remainder.
12. **HDR → SDR tone mapping** (WGC, `R16G16B16A16Float`, scRGB):
    1. Divide by the monitor's SDR white level (`DISPLAYCONFIG_SDR_WHITE_LEVEL`, fallback 200 nits), so SDR content maps 1:1.
    2. Clip negative components (colors outside the sRGB gamut).
    3. Leave everything below a knee of 0.8 untouched. Above the knee, compress the max channel with an extended-Reinhard shoulder: slope 1 at the knee, exactly 1.0 at the frame's peak, peak capped at 64× SDR white. RGB are scaled together to keep the hue.
    4. Encode as sRGB.

    A frame with no HDR highlights converts exactly. DXGI duplication does not support rotated outputs; those fall back to GDI.
