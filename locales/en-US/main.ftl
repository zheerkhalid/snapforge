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

## Effects
# Menu / dialog titles (one per operation)
fx-crop = Crop
fx-resize = Resize…
fx-resize-quick = Resize to { $percent }%
fx-canvas-size = Canvas Size…
fx-expand-canvas = Expand Canvas…
fx-rotate-90 = Rotate Right 90°
fx-rotate-180 = Rotate 180°
fx-rotate-270 = Rotate Left 90°
fx-rotate-any = Rotate by Angle…
fx-flip-horizontal = Flip Horizontal
fx-flip-vertical = Flip Vertical
fx-sharpen = Sharpen…
fx-blur = Blur…
fx-pixelate = Pixelate…
fx-adjust-colors = Adjust Colors…
fx-auto-levels = Auto Levels
fx-auto-contrast = Auto Contrast
fx-sepia = Sepia
fx-negative = Negative
fx-grayscale = Grayscale
fx-count-colors = Count Colors
fx-count-colors-result = This image has { $count } unique colors
fx-reduce-colors = Reduce Colors…
fx-make-transparent = Make Color Transparent…
fx-clone-tool = Clone Stamp
fx-remove-strip = Remove Strip…
fx-insert-strip = Insert Strip…
fx-selection-stats = Selection: { $pixels } pixels, { $colors } colors
fx-move-selection = Move Selection
fx-copy-selection = Copy Selection
fx-spotlight = Spotlight…
fx-drop-shadow = Drop Shadow…
fx-frame = Frame…
fx-torn-edge = Torn Edge…
fx-fade-edge = Fade Edge…
fx-reflection = Reflection…
fx-watermark = Watermark…
fx-watermark-text = Text Watermark…
fx-caption = Caption…
fx-oil-paint = Oil Paint…
fx-sketch = Sketch…
fx-preview = Preview
fx-reset = Reset
fx-apply = Apply
fx-presets = Presets
fx-save-preset = Save Preset…
# Shared parameters
fx-param-color = Color
fx-param-background = Background
fx-param-transparent = Transparent
fx-param-transparent-background = Transparent background
fx-param-opacity = Opacity (%)
fx-param-width = Width
fx-param-height = Height
fx-param-size = Size
fx-param-position = Position
fx-param-anchor = Anchor
fx-param-radius = Radius
fx-param-amount = Amount (%)
fx-param-threshold = Threshold
fx-param-seed = Random seed
fx-param-randomize = Randomize
fx-param-sides = Sides
fx-side-top = Top
fx-side-right = Right
fx-side-bottom = Bottom
fx-side-left = Left
fx-anchor-top-left = Top left
fx-anchor-top = Top
fx-anchor-top-right = Top right
fx-anchor-left = Left
fx-anchor-center = Center
fx-anchor-right = Right
fx-anchor-bottom-left = Bottom left
fx-anchor-bottom = Bottom
fx-anchor-bottom-right = Bottom right
fx-orientation-horizontal = Horizontal
fx-orientation-vertical = Vertical
# Resize
fx-resize-unit = Unit
fx-resize-pixels = Pixels
fx-resize-percent = Percent
fx-resize-keep-aspect = Keep aspect ratio
fx-resize-filter = Resampling
fx-filter-nearest = Nearest neighbor (hard pixels)
fx-filter-bilinear = Bilinear
fx-filter-mitchell = Mitchell (smooth)
fx-filter-lanczos = Lanczos 3 (sharpest)
# Canvas / expand / rotate
fx-canvas-fill = Fill new area with
fx-expand-left = Left
fx-expand-top = Top
fx-expand-right = Right
fx-expand-bottom = Bottom
fx-rotate-angle = Angle (degrees, clockwise)
# Adjust colors
fx-adjust-brightness = Brightness
fx-adjust-contrast = Contrast
fx-adjust-gamma = Gamma
fx-adjust-saturation = Saturation
fx-adjust-hue = Hue
fx-adjust-red = Red
fx-adjust-green = Green
fx-adjust-blue = Blue
# Filters
fx-blur-radius = Blur radius (px)
fx-sharpen-amount = Amount (%)
fx-sharpen-radius = Radius (px)
fx-sharpen-threshold = Threshold
fx-pixelate-block = Block size (px)
# Color reduction / transparency
fx-reduce-count = Number of colors
fx-reduce-colors-n = { $count } colors
fx-reduce-dither = Dithering (Floyd–Steinberg)
fx-transparent-color = Color to remove
fx-transparent-pick = Pick from image
fx-transparent-tolerance = Tolerance (%)
fx-transparent-mode = Mode
fx-transparent-contiguous = Connected area only (flood fill)
fx-transparent-global = Everywhere in the image
fx-transparent-smooth = Smooth edges
fx-transparent-show-mask = Show affected area
# Clone / strips / selection
fx-clone-radius = Brush size
fx-clone-hardness = Hardness
fx-clone-hint = Alt+click to set the source, then paint
fx-strip-orientation = Strip direction
fx-strip-position = Position
fx-strip-size = Size
fx-strip-color = Fill color
fx-move-copy = Keep original (copy)
# Spotlight
fx-spotlight-shape = Shape
fx-spotlight-rectangle = Rectangle
fx-spotlight-ellipse = Ellipse
fx-spotlight-dim = Dim outside (%)
fx-spotlight-feather = Feather (px)
fx-spotlight-transparent = Fade outside to transparent
# Drop shadow
fx-shadow-offset-x = Horizontal offset
fx-shadow-offset-y = Vertical offset
fx-shadow-blur = Blur
fx-shadow-opacity = Opacity (%)
fx-shadow-color = Shadow color
fx-shadow-corner-radius = Round image corners
# Frame
fx-frame-style = Style
fx-frame-solid = Solid
fx-frame-double = Double line
fx-frame-width = Border width
fx-frame-gap-color = Gap color
fx-frame-corner-radius = Corner radius
# Torn edge
fx-torn-depth = Tear depth
fx-torn-tooth = Tooth width
fx-torn-rim = Paper fiber rim
fx-torn-shadow = Add shadow
# Fade / reflection
fx-fade-size = Fade width (px)
fx-fade-to = Fade to
fx-reflection-height = Reflection height (%)
fx-reflection-gap = Gap (px)
fx-reflection-opacity = Starting opacity (%)
# Watermark
fx-watermark-image = Watermark image…
fx-watermark-text-label = Text
fx-watermark-margin = Margin (px)
fx-watermark-scale = Scale (%)
fx-watermark-tile = Tile across the image
fx-watermark-spacing = Tile spacing (px)
fx-watermark-outline = Dark halo for readability
# Caption and text
fx-caption-text = Caption text
fx-caption-position = Position
fx-caption-top = Top
fx-caption-bottom = Bottom
fx-caption-font = Font
fx-caption-font-size = Font size
fx-caption-bold = Bold
fx-caption-italic = Italic
fx-caption-underline = Underline
fx-caption-text-color = Text color
fx-caption-bar-color = Bar color
fx-caption-align = Alignment
fx-caption-align-left = Left
fx-caption-align-center = Center
fx-caption-align-right = Right
fx-caption-padding = Padding (px)
fx-caption-border = Border
fx-caption-border-none = None
fx-caption-border-line = Separator line
fx-caption-border-frame = Frame around image and caption
fx-caption-border-color = Border color
fx-caption-border-width = Border width
# Artistic
fx-oil-radius = Brush size
fx-sketch-strength = Pencil pressure (%)
fx-sketch-color = Keep colors

## Tools: combine, PDF, batch print
tool-combine = Combine Images…
tool-combine-direction = Arrange
tool-combine-side-by-side = Side by side
tool-combine-stacked = Stacked
tool-combine-spacing = Spacing (px)
tool-combine-margin = Margin (px)
tool-combine-align = Alignment
tool-combine-align-start = Top / left
tool-combine-align-center = Center
tool-combine-align-end = Bottom / right
tool-combine-shadow = Drop shadow under each image
tool-combine-labels = File names under images
tool-pdf = Images to PDF…
tool-pdf-done = Created { $path } ({ $pages } pages)
pdf-page-size = Page size
pdf-page-a4 = A4
pdf-page-letter = Letter
pdf-page-legal = Legal
pdf-page-a3 = A3
pdf-page-custom = Custom
pdf-page-fit = Fit to image
pdf-custom-width = Width (mm)
pdf-custom-height = Height (mm)
pdf-orientation = Orientation
pdf-orientation-auto = Automatic
pdf-orientation-portrait = Portrait
pdf-orientation-landscape = Landscape
pdf-margin = Margin (mm)
pdf-quality = Image quality
pdf-quality-jpeg = JPEG (smaller file)
pdf-quality-lossless = Lossless (sharp text)
pdf-jpeg-quality = JPEG quality
pdf-split-long = Split long images across pages
pdf-title = Document title
tool-batch-print = Batch Print…
tool-batch-print-done = Printed { $count } images
print-fit = Size
print-fit-page = Fit to page
print-fit-width = Fit to page width
print-actual-size = Actual size
print-margins = Margins (mm)
print-header = Header
print-footer = Footer
# $tokens: pass "{file} {path} {n} {total} {date} {time}"
print-template-help = Placeholders: { $tokens }
print-header-footer-size = Header/footer size (pt)
print-avoid-split = Don’t split text lines across pages
print-center = Center on page
