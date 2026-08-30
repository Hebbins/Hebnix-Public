-- hud_toggle hides and shows the game hud
local plugin = {}

local KEY = "hud_toggle_visible"

local last = nil
local sent = 0

function plugin.on_load()
    hebnix.log("HUD Toggle loaded")
end

function plugin.on_settings(ui)
    ui.label("Toggles the in game HUD through the statsapi socket.")
    if hebnix.rl_connected() then
        ui.colored_label("#2ecc71", "socket connected")
    else
        ui.colored_label("#e74c3c", "not connected, the command goes nowhere")
    end
    ui.space(6)

    local visible = ui.checkbox(KEY, "HUD visible", true)
    if last == nil then
        last = visible
    elseif visible ~= last then
        last = visible
        hebnix.command.set_hud_visibility(visible)
        sent = sent + 1
        hebnix.log("SetHUDVisibility " .. tostring(visible))
    end

    ui.space(6)
    if ui.button("Send it again") then
        hebnix.command.set_hud_visibility(visible)
        sent = sent + 1
        hebnix.log("SetHUDVisibility " .. tostring(visible) .. " (resend)")
    end
    ui.label("commands sent: " .. sent)
end

return plugin
