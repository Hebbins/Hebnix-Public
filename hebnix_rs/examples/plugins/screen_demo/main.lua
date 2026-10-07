-- screen_demo: reference for hebnix.screen, and a handy calibration tool.
--
-- hebnix.screen reads RL's window the way OBS window capture does. nothing
-- is injected and no game memory is read. coordinates are window pixels from
-- the top-left, the same space draw.* uses. needs Borderless or Windowed.
--
--   screen.available()                           -> bool
--   screen.size()                                -> w, h
--   screen.cursor()                              -> mouse x, y in window pixels
--   screen.pixel(x, y)                           -> r, g, b
--   screen.average(x, y, w, h)                   -> r, g, b
--   screen.match_color(x, y, w, h, r, g, b, tol) -> 0..1 share of pixels within tol
--
-- this plugin shows the colour under the mouse on the overlay, so you can
-- read off the coordinates and colour to feed match_color.

local plugin = {}

local cursor = nil      -- { x, y, r, g, b }

function plugin.on_tick()
    if not hebnix.get_bool("show_cursor", true) or not hebnix.rl_connected() then
        cursor = nil
        return
    end
    local x, y = hebnix.screen.cursor()
    local r, g, b = nil, nil, nil
    if x then r, g, b = hebnix.screen.pixel(x, y) end
    cursor = r and { x = x, y = y, r = r, g = g, b = b } or nil
end

function plugin.on_overlay(draw, w, h)
    if cursor then
        local hex = string.format("#%02x%02x%02x", cursor.r, cursor.g, cursor.b)
        local label = string.format("%d, %d   %d %d %d   %s", cursor.x, cursor.y, cursor.r, cursor.g, cursor.b, hex)
        draw.rect(cursor.x + 16, cursor.y + 16, 22, 22, { color = hex, filled = true })
        draw.rect(cursor.x + 16, cursor.y + 16, 22, 22, { color = "#ffffff", width = 1 })
        draw.rect(cursor.x + 42, cursor.y + 16, 250, 22, { color = "#000000cc", filled = true })
        draw.text(cursor.x + 48, cursor.y + 19, label, { color = "#ffffff", size = 13 })
    end
end

function plugin.on_settings(ui)
    ui.heading("Screen Demo")
    if hebnix.screen.available() then
        local w, h = hebnix.screen.size()
        ui.label(string.format("Capturing Rocket League, %dx%d.", w, h))
    else
        ui.label("No frame yet. Rocket League has to be open in Borderless or Windowed mode.")
    end
    ui.space(6)
    ui.checkbox("show_cursor", "Show the colour under the mouse on the overlay", true)
end

return plugin
