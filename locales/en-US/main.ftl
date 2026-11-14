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
## Pin
pin-window-title = Pinned Image
pin-menu-copy = Copy
pin-menu-save-as = Save As…
pin-menu-editor = Send to Editor
pin-menu-editor-all = Send All Pins to Editor
pin-menu-zoom-100 = Zoom 100%
pin-menu-opacity = Opacity
pin-menu-draw = Draw Mode
pin-menu-hide = Hide to Top Edge
pin-menu-close = Close
pin-menu-close-all = Close All Pins
pin-badge-opacity = Opacity { $pct }%
pin-badge-copied = Copied
pin-badge-saved = Saved
pin-clipboard-empty = The clipboard does not contain an image to pin
## OCR
ocr-title = Capture Text
ocr-language = Language
ocr-status-working = Recognizing text…
ocr-status-done = { $lines } lines · { $words } words
ocr-status-none = No text found
ocr-status-failed = Text recognition failed
ocr-status-copied = Copied to clipboard
ocr-status-no-selection = Select some text first
ocr-no-language = No OCR language is installed. Add a language with “Optical character recognition” in Settings › Time & language › Language & region, then try again.
ocr-keep-breaks = Keep line breaks
ocr-copy-all = Copy All
ocr-copy-selected = Copy Selected
ocr-close = Close
