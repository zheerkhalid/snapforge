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
editor-file = File
editor-open = Open…
editor-open-button = Open
editor-save-as-button = Save As
editor-open-folder = Open Folder
editor-recent = Recent Files
editor-recent-empty = No recent files
editor-recent-clear = Clear Recent Files
editor-about = About SnapForge
editor-version = Version { $version }
editor-close = Close
editor-zoom-in = Zoom In
editor-zoom-out = Zoom Out
editor-zoom-fit-width = Width
editor-close-prompt-title = Save changes?
editor-close-prompt = Do you want to save the changes to “{ $name }” before closing it?
editor-discard = Don't Save
editor-cancel = Cancel
editor-empty-title = No image open
editor-empty-hint = Open an image, or drop image files here.
editor-drop-hint = Drop images to open them
editor-filter-images = All images
editor-filter-all = All files
editor-open-failed = Could not open { $name }: { $error }
editor-unsupported = unsupported file type
editor-save-failed = Could not save: { $error }
editor-copy-failed = Could not copy: { $error }
editor-saving = Saving…
editor-loading = Opening…
