-- html overlay page. plugin.overlay_page names a file in assets/, overlay.send
-- pushes data into it.
local plugin = {}

plugin.overlay_page = "hud.html"

local CORNERS = { "Top left", "Top right", "Bottom left", "Bottom right" }

local ticks = 0

local function push()
    local info = hebnix.find_rocket_league()
    hebnix.overlay.send({
        ticks = ticks,
        platform = info and info.platform or "not running",
        connected = hebnix.rl_connected(),
        corner = hebnix.get_string("page_demo_corner", "Top left"),
    })
end

function plugin.on_load()
    hebnix.log("page_demo loaded")
end

-- on_tick is every frame, so only push twice a second
function plugin.on_tick()
    ticks = ticks + 1
    if ticks % 30 == 0 then
        push()
    end
end

function plugin.on_settings(ui)
    ui.label("This plugin draws an HTML overlay page.")
    ui.label("Focus Rocket League to see it.")
    ui.space(6)
    if ui.combo_box("page_demo_corner", "Corner", CORNERS) then
        push()
    end
    if ui.button("Push an update now") then
        push()
    end
    ui.space(6)
    ui.label("The page also tries one local and one remote image.")
    ui.label("The remote one should be refused, check the console.")
end

return plugin
