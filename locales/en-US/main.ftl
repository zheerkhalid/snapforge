# SnapForge: English (United States)
# Agents: add every new UI string here; look up with crate::i18n::t("key").
app-name = SnapForge
app-tagline = Fast screen capture, annotation and recording
app-credit = Developed by Zheer Khalid · zheer.dev

## Capture modes
mode-active = Capture Active Window
mode-window = Capture Window / Object
mode-region = Capture Rectangular Region
mode-freehand = Capture Freehand Region
mode-fullscreen = Capture Full Screen
mode-scrolling = Capture Scrolling Window
mode-fixed = Capture Fixed-Size Region
repeat-last = Repeat Last Capture
auto-capture = Auto Capture
auto-capture-every = Auto Capture Every { $secs } s

## Destinations
dest-editor = Editor
dest-draw = Draw
dest-clipboard = Clipboard
dest-file = File
dest-printer = Printer
dest-email = Email
dest-word = Microsoft Word
dest-excel = Microsoft Excel
dest-powerpoint = Microsoft PowerPoint
dest-onenote = Microsoft OneNote
dest-upload = FTP / SFTP Upload
dest-program = External Program
opt-preview = Preview in Editor First
opt-autocopy = Also Copy to Clipboard
opt-timestamp = Add Date/Time Stamp
output-to = Output To

## Delay
delay = Delay Before Capture
delay-none = No Delay
delay-secs = { $secs } seconds

## Tray
tray-tooltip = SnapForge: Ctrl+PrtSc to capture a region
tray-show-toolbar = Show Capture Toolbar
tray-open-editor = Open Editor
tray-open-folder = Open Output Folder
tray-settings = Settings…
tray-start-with-windows = Start with Windows
tray-about = About SnapForge
tray-exit = Exit

## Toolbar
toolbar-big-buttons = Bigger Buttons
toolbar-lock = Lock Position
toolbar-auto-hide = Auto-Hide at Top Edge
toolbar-customize = Visible Buttons
toolbar-minimize = Minimize to Tray
toolbar-menu = Menu

## Notifications
notify-saved = Saved to { $path }
notify-copied = Copied to clipboard
notify-uploaded = Uploaded: { $url }
notify-error = Something went wrong: { $error }
notify-hotkey-failed = Hotkey { $key } is in use by another program
notify-scroll-stop = Scrolling capture: press Esc or click to stop

## Overlay hints
hint-region = Drag to select · Click a window to select it · Tab switches corner · Arrows nudge 1 px · Right-click or Esc cancels
hint-freehand = Hold the left button and draw around the area · Esc cancels
hint-window = Click to capture · Ctrl+click selects several · Mouse wheel changes level · Esc cancels
hint-fixed = Click to capture · Shift+arrows resize · Space or wheel cycles presets · Esc cancels
hint-scroll = Click a scrollable window, or drag an area to scroll · Esc cancels

## Editor
editor-title = SnapForge Editor
editor-save = Save
editor-save-as = Save As…
editor-copy = Copy
editor-close-tab = Close Tab
editor-continue = Continue to { $dest }
editor-zoom-fit = Fit
editor-unsaved = Unsaved

## Hotkeys
hotkey-conflict = { $key } is assigned to both “{ $a }” and “{ $b }”
hotkey-invalid = Unrecognized hotkey “{ $key }” for { $action }
hotkey-hook-failed = Could not install the keyboard hook for { $key }

## Toolbar tooltips
toolbar-output-tip = Output To: { $dest }
toolbar-delay-tip = Delay Before Capture: { $delay }
## Output
notify-onenote-paste = The image is on the clipboard. Paste it into OneNote with Ctrl+V
notify-printed = Sent to the printer
notify-uploaded-no-url = Uploaded to { $host }
## Overlay (adjust mode) and countdown
hint-adjust = Drag inside to move · Drag handles to resize · Arrows move 1 px · Shift+arrows resize · Enter or double-click captures · Right-click reselects
countdown-cancel = Esc to cancel

## Screen tools
tools-name-color-picker = Screen Color Picker
tools-name-magnifier = Screen Magnifier
tools-name-crosshair = Screen Crosshair
tools-name-focus = Screen Focus
tools-name-ruler = Screen Ruler
tools-close = Close
tools-ok = OK
tools-cancel = Cancel
tools-copied = Copied ✓
tools-frozen = FROZEN
tools-picker-hint = Click picks · Space freezes · +/− zoom
tools-format-rgb = RGB
tools-format-hex = Hex
tools-format-decimal = Decimal
tools-format-html = HTML
tools-format-delphi = Delphi
tools-color-title = Color
tools-color-hex = Hex
tools-color-copy = Copy
tools-color-format = Format
tools-color-prefix = Prefix
tools-color-suffix = Suffix
tools-color-recent = Recent colors
tools-color-copy-hex = Copy HEX
tools-color-copy-rgb = Copy RGB
tools-color-pick-again = Pick Again
tools-color-original = Picked
tools-color-current = Adjusted
tools-magnifier-title = Magnifier
tools-mag-zoom = Zoom
tools-mag-smooth = Smooth
tools-mag-negative = Negative
tools-mag-lens = Lens (follows the cursor)
tools-mag-pane = Fixed pane
tools-ruler-hint = Drag the ticks to measure · Right-click for options
tools-ruler-units = Units
tools-unit-px = Pixels
tools-unit-cm = Centimeters
tools-unit-in = Inches
tools-ruler-vertical = Vertical
tools-ruler-protractor = Protractor
tools-ruler-opacity = Opacity
tools-ruler-autocopy = Copy measurements automatically
tools-ruler-copy = Copy measurement
tools-ruler-calibrate = Calibrate…
tools-cal-title = Calibrate Ruler
tools-cal-prompt = Diagonal size of this screen ({ $monitor }):
tools-cal-inches = inches
tools-cal-source-user = calibrated
tools-cal-source-edid = reported by the display
tools-cal-source-default = assumed 96 PPI
tools-cal-detected = Detected size: { $size }″. Reset uses it.
tools-cal-reset = Reset
