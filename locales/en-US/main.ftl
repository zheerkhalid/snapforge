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

## Draw
draw-tool-select = Select
draw-tip-select = Select, move and resize annotations
draw-tool-text = Text
draw-tip-text = Add text (wraps at the box width)
draw-tool-callout = Callout
draw-tip-callout = Text box with a pointer tail
draw-tool-line = Line
draw-tip-line = Straight line (Shift snaps to 45°)
draw-tool-arrow = Arrow
draw-tip-arrow = Arrow with configurable heads
draw-tool-curve = Curve
draw-tip-curve = Curved line with control points
draw-tool-polyline = Polyline
draw-tip-polyline = Click to add points, double-click to finish
draw-tool-elbow = L-Line
draw-tip-elbow = Line with a right-angle elbow
draw-tool-rect = Rectangle
draw-tip-rect = Rectangle (corner radius in properties)
draw-tool-ellipse = Ellipse
draw-tip-ellipse = Ellipse or circle
draw-tool-polygon = Polygon
draw-tip-polygon = Filled polygon: click points, double-click to finish
draw-tool-bracket = Bracket
draw-tip-bracket = Curly or square bracket
draw-tool-highlighter = Highlighter
draw-tip-highlighter = Highlight an area like a marker
draw-tool-line-highlighter = Line Highlighter
draw-tip-line-highlighter = Freehand marker stroke (Ctrl draws straight)
draw-tool-magnifier = Magnifier
draw-tip-magnifier = Show an area magnified in a callout
draw-tool-step = Step
draw-tip-step = Numbered step stamp (auto-increments)
draw-tool-emoji = Emoji
draw-tip-emoji = Insert an emoji
draw-tool-sticker = 3D Arrow
draw-tip-sticker = Insert a 3D arrow
draw-tool-image = Image
draw-tip-image = Insert an image from a file or the clipboard
draw-tool-cursor = Pointer
draw-tip-cursor = Insert a mouse-pointer icon
draw-tool-blur = Blur
draw-tip-blur = Blur an area
draw-tool-pixelate = Pixelate
draw-tip-pixelate = Pixelate an area
draw-tool-eraser = Eraser
draw-tip-eraser = Erase to transparent or a background color
draw-tool-fill = Fill
draw-tip-fill = Flood-fill an area of similar color
draw-tool-pencil = Pencil
draw-tip-pencil = Freehand hard-pixel pencil
draw-prop-color = Color
draw-prop-fill = Fill
draw-prop-width = Width
draw-prop-opacity = Opacity
draw-prop-dash = Dash
draw-prop-shadow = Shadow
draw-prop-outline = Outline
draw-prop-font = Font
draw-prop-font-size = Size
draw-prop-bold = Bold
draw-prop-italic = Italic
draw-prop-underline = Underline
draw-prop-align = Alignment
draw-prop-text-color = Text Color
draw-prop-head-start = Start Head
draw-prop-head-end = End Head
draw-prop-head-size = Head Size
draw-prop-decor = Line Style
draw-prop-color2 = Second Color
draw-prop-text-box = Box
draw-prop-text-flow = Text Path
draw-prop-wrap = Wrap Text
draw-prop-padding = Padding
draw-prop-radius = Corner Radius
draw-prop-tail-width = Tail Width
draw-prop-facing = Facing
draw-prop-indent = Indent
draw-prop-blur-outside = Blur Outside
draw-prop-dim-outside = Dim Outside
draw-prop-connector = Connector
draw-prop-zoom = Zoom
draw-prop-step-shape = Shape
draw-prop-step-letters = Use Letters
draw-prop-strength = Strength
draw-prop-radius-brush = Brush Radius
draw-prop-tolerance = Tolerance
draw-prop-transparent = Transparent
draw-prop-rotation = Rotation
draw-prop-flip-h = Flip Horizontal
draw-prop-flip-v = Flip Vertical
draw-prop-timing = Show From / To (ms)
draw-align-left = Align Left
draw-align-right = Align Right
draw-align-top = Align Top
draw-align-bottom = Align Bottom
draw-align-center-h = Align Centers Horizontally
draw-align-center-v = Align Centers Vertically
draw-same-width = Same Width
draw-same-height = Same Height
draw-same-size = Same Size
draw-bring-to-front = Bring to Front
draw-send-to-back = Send to Back
draw-bring-forward = Bring Forward
draw-send-backward = Send Backward
draw-undo = Undo
draw-redo = Redo
draw-save-preset = Save Style as Preset…
draw-presets = Presets
draw-recent-colors = Recent Colors
draw-save-project = Save Project (.sfcap)…
draw-open-project = Open Project…
draw-project-newer = This project was saved by a newer version of SnapForge
