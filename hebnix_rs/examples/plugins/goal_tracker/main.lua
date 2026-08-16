-- counts goals, shows them in a little always-on-top window

local plugin = {}

local goals = 0
local last_scorer = nil

local function open_window()
    hebnix.window.open{ title = "Goal Tracker", width = 240, height = 130, opacity = 0.9 }
end

function plugin.on_load()
    hebnix.log("Goal Tracker loaded (Hebnix " .. hebnix.app_version() .. ")")
    if hebnix.get_bool("show_window", false) then
        open_window()
    end
end

function plugin.on_unload()
    hebnix.window.close()
end

function plugin.on_game_event(event_type, event)
    if event_type == "GoalScored" then
        goals = goals + 1
        last_scorer = event.data.Scorer and event.data.Scorer.Name or "someone"
        hebnix.log("GOAL by " .. last_scorer .. "! (" .. goals .. " this session)")
    elseif event_type == "GameLeft" or event_type == "MatchEnded" then
        goals = 0
        last_scorer = nil
    end
end

function plugin.on_settings(ui)
    ui.label("Counts goals scored while you play.")
    ui.space(4)
    local show = ui.checkbox("show_window", "Show overlay window", false)
    if show and not hebnix.window.is_open() then
        open_window()
    elseif not show and hebnix.window.is_open() then
        hebnix.window.close()
    end
    ui.space(6)
    if ui.button("Reset counter") then
        goals = 0
        hebnix.log("Counter reset.")
    end
end

function plugin.on_window(ui)
    ui.heading("Goals: " .. goals)
    if last_scorer then
        ui.label("Last: " .. last_scorer)
    end
    if hebnix.rl_connected() then
        ui.colored_label("#2ecc71", "Connected")
    else
        ui.colored_label("#aaaaaa", "Waiting for Rocket League...")
    end
end

return plugin
