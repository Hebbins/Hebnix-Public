# Window Widget API

Hebnix Lua plugins can enumerate a visible top-level Windows window, ask the
user for permission to capture it, and draw its latest frame in the native
Rocket League overlay.

```lua
local windows = hebnix.capture.windows()
local capture = nil

-- Call this in response to the user's window selection. Hebnix displays a
-- native confirmation containing the plugin, window title, and process.
local function select_window(window)
    if capture then
        hebnix.capture.window_capture_stop(capture)
    end
    capture = hebnix.capture.window_capture_start(window.id, {
        fps = 30,
        cursor = false,
    })
end

function plugin.on_overlay(draw, width, height)
    if not capture then return end
    local frame = hebnix.capture.window_capture_frame(capture)
    if frame then
        draw.capture_image(frame, 40, 40, 640, 360, { opacity = 1.0 })
    end
end

function plugin.on_unload()
    if capture then
        hebnix.capture.window_capture_stop(capture)
    end
end
```

## API

- `hebnix.capture.windows()` returns visible, titled top-level windows as
  `{ id, title, process }` records. Only IDs from the plugin's latest call can
  be passed to `window_capture_start`.
- `hebnix.capture.window_capture_start(id, options)` asks the user to approve
  that exact window, then returns an opaque handle or `nil`. `fps` is clamped
  to 1–30 and `cursor` defaults to `false`.
- `hebnix.capture.window_capture_frame(handle)` returns the opaque latest-frame
  handle, or `nil` before the first frame and after the target disappears.
- `draw.capture_image(frame, x, y, width, height, options)` draws the current
  frame. `opacity` defaults to `1.0` and is clamped by the native renderer.
- `hebnix.capture.window_capture_stop(handle)` stops and joins the worker.

Frames are capped at 1920×1080 while preserving aspect ratio. Each plugin can
have at most four active captures. Capture workers and their native resources
are also stopped automatically when the plugin unloads.
