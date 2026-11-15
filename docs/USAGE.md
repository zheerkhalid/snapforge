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

## Draw tools

The editor's left palette holds every drawing tool. Annotations stay editable objects on top of the picture: select one to move it, drag its handles (Shift keeps the aspect ratio or snaps angles, Alt resizes from the centre), double-click text to edit it in place. The bar under the toolbar shows the properties of the active tool or of the selected objects (colors with swatches and an eye dropper, right-click a color box to pick from the screen; width, dash, arrowheads, line style, font with live previews up to 200 pt, shadow and outline, box and tail, saved style presets, align and order).

| Key | Tool | Key | Tool |
|---|---|---|---|
| `F2` | Select / move | `Shift+F2` | Callout |
| `F3` | Text | `Shift+F3` | Curve |
| `F4` | Arrow | `Shift+F4` | Polyline |
| `F5` | Line | `Shift+F5` | L-line |
| `F6` | Rectangle | `Shift+F6` | Polygon |
| `F7` | Ellipse | `Shift+F7` | Bracket |
| `F8` | Highlighter | `Shift+F8` | Line highlighter (hold Ctrl for a straight stroke) |
| `F9` | Step stamp | `Shift+F9` | Magnifier |
| `F10` | Blur | `Shift+F10` | Pixelate |
| `F11` | Eraser (mouse wheel changes the size) | `Shift+F11` | Fill |
| `F12` | Pencil | `Shift+F12` | Insert image |
| `Ctrl+F2` | Emoji | `Ctrl+F3` | 3D arrow |
| `Ctrl+F5` | Mouse pointer | | |

- `Tab` / `Shift+Tab` cycle through the tools in palette order. `F1` lists every shortcut.
- Right-click a palette tool and choose **Set as Default Tool**: the **Draw** destination opens captures with it.
- Arrow keys move the selected annotations (with Shift, 10 px). `Ctrl+J` duplicates, `Del` deletes, `Ctrl+C` / `Ctrl+V` copy and paste them, `Ctrl+A` selects them all. Polylines and polygons end with a double-click, Enter or Esc.
- Pixel edits (effects, crop) and annotation edits share one undo history (`Ctrl+Z` / `Ctrl+Y`).
- **File ▸ Save Project (.sfcap)** (`Ctrl+Alt+S`) keeps the annotations editable; open a project with File ▸ Open or by dropping it on the editor. When you save an annotated image, an editable `.sfcap` copy is saved next to it (setting `draw.autosave_project`, on by default; if that folder is read-only it goes to the `autosave` folder in the settings folder).
- Save, Copy, Print, Pin and *Continue to …* use the flattened picture; the original pixels are kept separately while you edit.

Image ▸ Resize to 25 / 50 / 75 / 200 % is on `Ctrl+1` … `Ctrl+4`. Effects apply to the selection when there is one. Make Color Transparent, Spotlight, Remove / Insert Strip and Watermark work directly on the canvas; the Clone Stamp tool takes its source from an Alt+click.
