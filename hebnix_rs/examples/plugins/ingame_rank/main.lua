-- ingame_rank: shows tracker.gg ranks of everyone in your match in a floating


local plugin = {}

local ALL_MODES = {
    "Casual", "Ranked Duel 1v1", "Ranked Doubles 2v2",
    "Ranked Standard 3v3", "Hoops", "Rumble", "Dropshot",
    "Snowday", "Tournament Matches", "Ranked 4v4 Quads", "Heatseeker",
}

local MAX_ROWS = 8

-- { {id = "...", name = "..."}, ... } in join order
local players = {}
local seen = {}
local current_mode = "Casual"
local cycle_was_pressed = false
local capture_target = nil

-- Helpers

-- percent is a share of the monitor rocket league is on, so the window covers
-- the same slice of the game whatever the resolution. a plain number is a size
-- in points, same physical size on every screen but a different slice of a
-- 1080p one than of a 4k one.
local function open_window()
    hebnix.window.open{ title = "In Game Ranks", width = "34%", height = "24%", opacity = 0.92 }
end

local function enabled_modes()
    local out = {}
    for _, mode in ipairs(ALL_MODES) do
        if hebnix.get_bool("enable_" .. mode, true) then
            table.insert(out, mode)
        end
    end
    if #out == 0 then out = { "Casual" } end
    return out
end

local function cycle_mode()
    local modes = enabled_modes()
    local idx = 0
    for i, m in ipairs(modes) do
        if m == current_mode then idx = i break end
    end
    current_mode = modes[(idx % #modes) + 1]
end

local function format_ranks(stats)
    local best_tier, best_str, best_mode = 0, "Unranked", ""
    local mode_str = "Unranked"

    for _, rank in pairs(stats.ranks or {}) do
        if rank.tier_id > best_tier then
            best_tier = rank.tier_id
            best_str = hebnix.shorten_rank(rank.tier_name)
            best_mode = rank.playlist_name
        end
        if string.lower(rank.playlist_name) == string.lower(current_mode) then
            local suffix
            if current_mode == "Casual" then
                suffix = " (" .. rank.mmr .. " MMR)"
            else
                suffix = " " .. rank.division_name .. " (" .. rank.mmr .. ")"
            end
            mode_str = hebnix.shorten_rank(rank.tier_name) .. suffix
        end
    end

    local best_display = best_str
    if best_tier > 0 and best_mode ~= "" then
        best_display = best_str .. " (" .. best_mode .. ")"
    end
    return mode_str, best_display
end

local function clear_players()
    players = {}
    seen = {}
    hebnix.clear_stats_cache()
end

-- Callbacks

function plugin.on_load()
    hebnix.log("InGameRank loaded")
    local modes = enabled_modes()
    current_mode = modes[1]
    hebnix.refresh_action_binds()
end

function plugin.on_unload()
    hebnix.window.close()
end

function plugin.on_game_event(event_type, event)
    if event_type == "UpdateState" then
        for _, p in ipairs(event.data.Players or {}) do
            local pid = p.PrimaryId or ""
            local name = p.Name or "Unknown"
            if pid ~= "" and not hebnix.is_bot(pid) and not seen[pid] then
                seen[pid] = true
                table.insert(players, { id = pid, name = name })
                hebnix.fetch_stats_async(pid, name)
            end
        end
    elseif event_type == "GameLeft" or event_type == "MatchEnded" then
        clear_players()
    end
end

function plugin.on_tick()
    local should_show = hebnix.is_action_pressed("Scoreboard")
    if should_show and not hebnix.window.is_open() then
        open_window()
    elseif not should_show and hebnix.window.is_open() then
        hebnix.window.close()
    end

    -- Cycle bind: advance the displayed gamemode on press (edge-triggered).
    local cycle = hebnix.get_string("cycle_bind_setting", "")
    if cycle ~= "" then
        local pressed = hebnix.is_bind_pressed(cycle)
        if pressed and not cycle_was_pressed then
            cycle_mode()
        end
        cycle_was_pressed = pressed
    end

    -- Finish a pending bind capture.
    if capture_target then
        local status, bind = hebnix.capture_bind_result()
        if status == "done" then
            hebnix.set("cycle_bind_setting", bind)
            hebnix.log("cycle bind updated to: " .. bind)
            capture_target = nil
        elseif status == "timeout" then
            hebnix.log("Bind capture timed out.")
            capture_target = nil
        end
    end
end

local function bind_row(ui, label, setting_key, target)
    local bind = hebnix.get_string(setting_key, "")
    ui.horizontal(function()
        ui.label(label .. ": " .. (bind ~= "" and bind or "(none)"))
        if capture_target == target then
            ui.colored_label("#d35400", "Press any key/button...")
        else
            if ui.button("Set") then
                if hebnix.capture_bind_async(10) then
                    capture_target = target
                end
            end
            if ui.button("Clear") then
                hebnix.set(setting_key, "")
                hebnix.log(target .. " bind cleared.")
            end
        end
    end)
end

function plugin.on_settings(ui)
    ui.heading("Gamemode Cycling")
    bind_row(ui, "Cycle bind", "cycle_bind_setting", "cycle")

    ui.space(8)
    ui.heading("Enabled Cycle Gamemodes")
    for _, mode in ipairs(ALL_MODES) do
        ui.checkbox("enable_" .. mode, "Enable " .. mode, true)
    end

    ui.space(8)
    ui.heading("Visual Toggles")
    ui.checkbox("show_best_rank", "Show Best Rank", true)
    ui.checkbox("show_total_matches", "Show Total Matches", false)
    ui.space(6)
    if ui.button("Clear Cache") then
        clear_players()
    end
    ui.space(4)
    ui.label("Version 2.0.0")
end

function plugin.on_window(ui)
    ui.colored_label("#5dade2", "Mode: " .. current_mode)
    ui.separator()

    if #players == 0 then
        ui.label("Waiting for players...")
        return
    end

    local show_best = hebnix.get_bool("show_best_rank", true)
    local show_matches = hebnix.get_bool("show_total_matches", false)

    for i, p in ipairs(players) do
        if i > MAX_ROWS then break end
        local tag = hebnix.platform_tag(p.id)
        local res = hebnix.stats_result(p.id)

        if res == nil or res == "pending" then
            ui.label(tag .. " " .. p.name .. "  (Fetching...)")
        elseif res.error ~= nil then
            ui.label(tag .. " " .. p.name .. "  (Private/Not Found)")
        else
            local mode_str, best_str = format_ranks(res)
            local line = tag .. " " .. p.name .. "  |  " .. current_mode .. ": " .. mode_str
            if show_best then
                line = line .. "  |  Best: " .. best_str
            end
            if show_matches and res.lifetime then
                line = line .. "  |  " .. res.lifetime.wins .. "W"
            end
            ui.label(line)
        end
    end
end

return plugin
