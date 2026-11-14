# SnapForge user guide

SnapForge runs in the notification area (tray). Press a hotkey, or click a button on the floating capture toolbar, and the capture goes to the destination you picked.

*SnapForge is developed by [Zheer Khalid](https://zheer.dev).*

## Hotkeys

| Action | Default hotkey |
|---|---|
| Full screen (the monitor under the cursor; all monitors is an option) | `PrtSc` |
| Active window | `Alt+PrtSc` |
| Window / object under the cursor | `Shift+PrtSc` |
| Rectangular region | `Ctrl+PrtSc` |
| Freehand region | `Ctrl+Shift+PrtSc` |
| Scrolling window or area | `Ctrl+Alt+PrtSc` |
| Fixed-size region | `Ctrl+Shift+Alt+PrtSc` |
| Repeat last capture | *(unassigned)* |

You can reassign any hotkey in `settings.toml` under `[hotkeys]` (a Settings window arrives in Phase 6). If Windows has mapped PrtSc to open the Snipping Tool, SnapForge still takes over PrtSc when `prtsc_override = true` (the default).

## Inside a capture

| Mode | Keys and mouse |
|---|---|
| **Region** | Drag to select. A click without dragging selects the window under the cursor. **Tab** switches which corner you are dragging. **Arrow keys** move 1 px. **Ctrl+Z** or **right-click** starts over. **Esc** cancels. |
| **Freehand** | Hold the left button and draw around the area. |
| **Window / object** | Hover to highlight. Use the **mouse wheel** to step between a window and its inner controls. **Ctrl+click** selects several windows, objects or menus. A plain click or **Enter** finishes. |
| **Fixed size** | The rectangle follows the cursor. **Shift+arrows** resize it (with **Ctrl** too, in 10 px steps). **Space** or the **wheel** switches presets. Click to capture. |
| **Scrolling** | Click the scrollable window or control, or drag a custom area. SnapForge then scrolls and stitches automatically. Press **Esc** or click to stop early and keep what has been captured. |

The magnifier loupe shows the cursor position, the pixel color and the selection size. You can capture areas as small as 1×1 pixel.

## Output destinations

Choose a destination from the toolbar's **Output** button or the tray menu:

- Editor
- Draw
- Clipboard
- File (named automatically)
- Printer
- Email (opens your mail client with the image attached)
- Microsoft Word, Excel, PowerPoint or OneNote
- FTP/SFTP upload
- An external program

These options can be combined with any destination:

- **Preview in Editor First**: shows the capture in the editor, with a *Continue to …* button.
- **Also Copy to Clipboard**
- **Add Date/Time Stamp**

## File names

Set the template with `[filename] template`. Tokens:

| Token | Meaning | Example |
|---|---|---|
| `$Y` `$M` `$D` | year, month, day | `2026` `03` `07` |
| `$H` `$N` `$S` | hour, minute, second | `09` `05` `02` |
| `###` | counter, zero-padded to the number of `#` (with optional daily reset) | `007` |
| `@` | one random character | `k` |
| `%USERNAME%` `%COMPUTERNAME%` | user and computer names | |
| `%DESKTOP%` `%DOCUMENTS%` `%PICTURES%` | folders (put them at the start for an absolute path) | |

The default template is `SnapForge_$Y-$M-$D_$H$N$S`, saved in `Pictures\SnapForge`. Existing files are never overwritten; SnapForge adds ` (2)`, ` (3)` and so on.

## Command line

```text
SnapForge.exe --capture region --out clipboard
SnapForge.exe --capture fullscreen --file "C:\shots\screen.png"
SnapForge.exe --capture active --delay 3 --out file
SnapForge.exe --open picture.png
SnapForge.exe --bench
SnapForge.exe --help
```

If SnapForge is already running, the command is forwarded to it.

## Portable mode and settings

- Settings are stored in `%APPDATA%\SnapForge\settings.toml`.
- Put an empty `portable.flag` file next to `SnapForge.exe` to keep settings beside the EXE instead. The portable ZIP already includes it.
- Logs are kept in the `logs` subfolder.
- FTP passwords are encrypted with Windows DPAPI.
- `SnapForge.exe --uninstall-cleanup` removes the settings, the logs and the autostart entry.
