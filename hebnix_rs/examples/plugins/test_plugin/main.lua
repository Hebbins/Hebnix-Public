-- test_plugin: pokes every part of the hebnix lua api. copy what you need.
--
-- lifecycle callbacks you can define:
--   on_load()                    plugin enabled / reloaded
--   on_unload()                  plugin disabled / app closing
--   on_game_event(type, event)   every StatsAPI event
--   on_tick()                    every UI frame (~20x/s), keep it cheap
--   on_settings(ui)              Settings > Plugin Settings page
--   on_window(ui)                contents of the floating window
--   on_overlay(draw, w, h)       click-through canvas over the game window
--   on_http_response(id, status, body)  result of your http_get/post_async

local plugin = {}

-- state

local tick_count = 0
local event_counts = {}          -- event_type -> count
local event_order = {}           -- insertion order for stable display
local last_goal = nil
local last_touch_speed = nil
local capture_active = false
local captured_bind = nil
local fetch_pid = nil            -- key of the demo stats fetch
local log_info = nil             -- result of parse_launch_log
local rl_info = nil              -- result of find_rocket_league
local save_summary = nil         -- result of load_save_summary
local http_status = nil          -- last http_get_async status
local http_bytes = nil

local function count_event(event_type)
    if event_counts[event_type] == nil then
        event_counts[event_type] = 0
        table.insert(event_order, event_type)
    end
    event_counts[event_type] = event_counts[event_type] + 1
end

-- lifecycle

function plugin.on_load()
    hebnix.log("Test Plugin loaded")
    hebnix.log("  app_version = " .. hebnix.app_version())
    hebnix.log("  slug        = " .. hebnix.slug())

    -- Persisted settings round trip (plugins/config/<slug>/settings.toml)
    hebnix.set("load_count", hebnix.get_number("load_count", 0) + 1)
    hebnix.log("  load_count  = " .. hebnix.get_number("load_count", 0))

    -- Utils sanity checks
    hebnix.log("  shorten_rank('Grand Champion II') = " .. hebnix.shorten_rank("Grand Champion II"))
    hebnix.log("  tier_name(22) = " .. hebnix.tier_name(22))
    hebnix.log("  platform_tag('Steam|123|0') = " .. hebnix.platform_tag("Steam|123|0"))
    hebnix.log("  is_bot('unknown') = " .. tostring(hebnix.is_bot("unknown")))

    if hebnix.get_bool("open_window_on_load", true) then
        hebnix.window.open{ title = "Test Plugin", width = 340, height = 260, opacity = 0.92 }
    end
end

function plugin.on_unload()
    hebnix.log("Test Plugin unloading (tick_count=" .. tick_count .. ")")
    hebnix.window.close()
end

-- game events

function plugin.on_game_event(event_type, event)
    count_event(event_type)

    if event_type == "GoalScored" then
        local scorer = event.data.Scorer and event.data.Scorer.Name or "?"
        local speed = event.data.GoalSpeed or 0
        last_goal = scorer .. " (" .. math.floor(speed) .. " kph)"
        hebnix.log("GoalScored by " .. scorer)
    elseif event_type == "BallHit" then
        last_touch_speed = event.data.Ball and event.data.Ball.PostHitSpeed
    elseif event_type == "StatfeedEvent" then
        hebnix.log("Statfeed: " .. (event.data.EventName or "?")
            .. " by " .. ((event.data.MainTarget or {}).Name or "?"))
    elseif event_type == "MatchEnded" then
        hebnix.log("Match ended, winner team " .. tostring(event.data.WinnerTeamNum))
    elseif event_type == "GameLeft" then
        hebnix.log("Left the game (reason: " .. tostring(event.data.reason) .. ")")
    end

    -- Kick an async tracker fetch for the first real player we see.
    if event_type == "UpdateState" and fetch_pid == nil then
        for _, p in ipairs(event.data.Players or {}) do
            local pid = p.PrimaryId or ""
            if pid ~= "" and not hebnix.is_bot(pid) then
                fetch_pid = pid
                hebnix.fetch_stats_async(pid, p.Name or "Unknown")
                hebnix.log("Queued tracker fetch for " .. (p.Name or "?"))
                break
            end
        end
    end
end

-- only fires for ids this plugin asked for, still check if you have several
-- in flight

function plugin.on_http_response(id, status, body)
    if id ~= "test_ping" then
        return
    end
    http_status = status
    http_bytes = #body
    hebnix.log("http_get_async -> status " .. status .. ", " .. #body .. " bytes")
end

-- per-frame tick

function plugin.on_tick()
    tick_count = tick_count + 1

    -- Poll a pending bind capture (started from the settings page).
    if capture_active then
        local status, bind = hebnix.capture_bind_result()
        if status == "done" then
            captured_bind = bind
            hebnix.set("test_bind", bind)
            hebnix.log("Captured bind: " .. bind)
            capture_active = false
        elseif status == "timeout" then
            hebnix.log("Bind capture timed out")
            capture_active = false
        end
    end
end

-- settings page

function plugin.on_settings(ui)
    ui.heading("Widgets")
    ui.label("A plain label.")
    ui.colored_label("#2ecc71", "A green label.")
    ui.horizontal(function()
        ui.label("Inline row:")
        if ui.button("Log hello") then
            hebnix.log("hello from the settings page")
        end
        ui.checkbox("inline_check", "inline checkbox", false)
    end)
    ui.separator()

    ui.heading("Persisted values")
    ui.checkbox("open_window_on_load", "Open window on load", true)
    local text = ui.text_input("test_text", "type something, press enter/click away")
    ui.label("Stored text: '" .. text .. "'")
    ui.space(6)

    ui.heading("Floating window")
    ui.horizontal(function()
        if ui.button("Open") then
            hebnix.window.open{ title = "Test Plugin", width = 340, height = 260, opacity = 0.92 }
        end
        if ui.button("Close") then
            hebnix.window.close()
        end
        if ui.button("Rename") then
            hebnix.window.set_title("Renamed @ tick " .. tick_count)
        end
        ui.label("open: " .. tostring(hebnix.window.is_open()))
    end)
    ui.space(6)

    ui.heading("Input binds")
    local bind = ui.text_input("test_bind", "e.g. tab, controller_a, cross, l1")
    if bind ~= "" then
        if hebnix.is_bind_pressed(bind) then
            ui.colored_label("#2ecc71", "'" .. bind .. "' is HELD")
        else
            ui.label("'" .. bind .. "' is not held")
        end
    end
    if capture_active then
        ui.colored_label("#d35400", "Press any key / controller button...")
    elseif ui.button("Capture a bind (keyboard / Xbox / PlayStation)") then
        capture_active = hebnix.capture_bind_async(10)
    end
    if captured_bind then
        ui.label("Last captured: " .. captured_bind)
    end
    ui.space(6)

    ui.heading("SDK calls")
    ui.horizontal(function()
        if ui.button("find_rocket_league()") then
            rl_info = hebnix.find_rocket_league()
            if rl_info then
                hebnix.log("RL pid=" .. rl_info.pid .. " platform=" .. rl_info.platform)
            else
                hebnix.log("Rocket League is not running")
            end
        end
        if ui.button("parse_launch_log()") then
            log_info = hebnix.parse_launch_log(false) -- verify=false: fast
            local user = (log_info.session or {}).username or "?"
            hebnix.log("Launch.log user: " .. tostring(user))
        end
    end)
    if rl_info then
        ui.label("RL: pid " .. rl_info.pid .. ", " .. rl_info.platform .. ", " .. rl_info.root_dir)
    end
    if log_info and log_info.session then
        ui.label("Log user: " .. tostring(log_info.session.username)
            .. "  platform: " .. tostring(log_info.session.platform))
    end
    ui.space(6)

    ui.heading("Tracker (async)")
    ui.label("Platform: steam / epic / xbl / psn / switch")
    local platform_input = ui.text_input("tracker_platform", "steam")
    ui.label("Identifier: Steam id64 or display name (any platform)")
    local ident_input = ui.text_input("tracker_ident", "76561198... or DisplayName")
    if ui.button("Fetch profile") and ident_input ~= "" then
        local platform = platform_input ~= "" and platform_input or "steam"
        fetch_pid = hebnix.fetch_profile_async(platform, ident_input)
    end
    if fetch_pid then
        local res = hebnix.stats_result(fetch_pid)
        if res == nil then
            ui.label("No fetch queued for " .. fetch_pid)
        elseif res == "pending" then
            ui.colored_label("#f39c12", "Fetching " .. fetch_pid .. " ...")
        elseif res.error ~= nil then
            ui.colored_label("#e74c3c", "Error: " .. tostring(res.error))
        else
            ui.label("Handle: " .. tostring(res.platform_user_handle))
            local best_tier, best = 0, "Unranked"
            for _, rank in pairs(res.ranks or {}) do
                if rank.tier_id > best_tier then
                    best_tier = rank.tier_id
                    best = hebnix.shorten_rank(rank.tier_name) .. " (" .. rank.playlist_name .. ")"
                end
            end
            ui.label("Best rank: " .. best)
            if res.lifetime then
                ui.label("Lifetime wins: " .. res.lifetime.wins)
            end
        end
    end
    if ui.button("Clear stats cache") then
        hebnix.clear_stats_cache()
        fetch_pid = nil
    end
    ui.space(6)

    ui.heading("HTTP (async)")
    if ui.button("GET api.hebnix.com/plugins") then
        http_status, http_bytes = nil, nil
        hebnix.http_get_async("test_ping", "https://api.hebnix.com/plugins")
    end
    if http_status then
        ui.label("status " .. http_status .. ", " .. http_bytes .. " bytes")
    end
    ui.space(6)

    ui.heading("Game Overlay")
    ui.checkbox("overlay_demo", "Show overlay demo while playing (crosshair + HUD)", false)
    ui.label("Renders on a click-through canvas over the game while it's focused.")
    ui.space(6)

    ui.heading("SaveData (.save file)")
    if ui.button("Find latest .save") then
        local path = hebnix.find_save_file()
        hebnix.log("find_save_file: " .. tostring(path))
    end
    if ui.button("Load save summary") then
        save_summary = hebnix.load_save_summary()
        if save_summary.error ~= nil then
            hebnix.log("load_save_summary error: " .. save_summary.error)
        else
            hebnix.log("Loaded save: " .. save_summary.path
                .. " (" .. save_summary.objects .. " objects)")
        end
    end
    if save_summary and save_summary.error == nil then
        ui.label("File: " .. save_summary.path)
        ui.label("Objects: " .. save_summary.objects
            .. "   Inventory items: " .. tostring(save_summary.inventory_count))
        if save_summary.profile_name then
            ui.label("Profile: " .. save_summary.profile_name
                .. "   Title: " .. tostring(save_summary.player_title))
        end
        if save_summary.xp then
            ui.label("Level: " .. save_summary.xp.level
                .. "   XP into level: " .. save_summary.xp.xp
                .. "   Total XP: " .. save_summary.xp.total_xp)
        end
        -- stale until RL exits, hebnix.window_mode() is live
        if save_summary.video then
            local v = save_summary.video
            ui.label("Window mode: " .. v.window_mode
                .. "   Resolution: " .. v.resolution
                .. "   Max FPS: " .. v.max_fps)
        end
        if save_summary.camera then
            local c = save_summary.camera
            ui.label(string.format("Camera: FOV %.0f  H %.0f  A %.1f  D %.0f  Stiff %.2f  ballcam %s",
                c.fov, c.height, c.angle, c.distance, c.stiffness,
                tostring(c.ball_cam_default)))
        end
        if save_summary.loadout then
            local l = save_summary.loadout
            ui.label("Loadout: body " .. l.body .. "  decal " .. l.decal
                .. "  wheels " .. l.wheels .. "  boost " .. l.boost)
        end
        if save_summary.skills then
            ui.label("Skill tiers (from save):")
            for playlist_id, skill in pairs(save_summary.skills) do
                ui.label("  playlist " .. playlist_id .. ": " .. skill.tier_name
                    .. " (" .. skill.matches_played .. " matches)")
            end
        end
        -- 3 values per stat id, slots unconfirmed
        if save_summary.stats then
            local w = save_summary.stats.Win or {}
            local g = save_summary.stats.Goal or {}
            ui.label("Win values: " .. table.concat(w, " / ")
                .. "   Goal values: " .. table.concat(g, " / "))
        end
        if save_summary.recent_players_count then
            ui.label("Recent players: " .. save_summary.recent_players_count
                .. "   Observed: " .. tostring(save_summary.observed_players_count))
        end
    end
    ui.space(6)
    ui.label("Version 2.1.0")
end

-- game overlay (click-through, over the focused game)

function plugin.on_overlay(draw, w, h)
    if not hebnix.get_bool("overlay_demo", false) then
        return
    end
    local cx, cy = w / 2, h / 2

    -- Crosshair
    draw.line(cx - 14, cy, cx - 4, cy, { color = "#2ecc71", width = 2 })
    draw.line(cx + 4, cy, cx + 14, cy, { color = "#2ecc71", width = 2 })
    draw.line(cx, cy - 14, cx, cy - 4, { color = "#2ecc71", width = 2 })
    draw.line(cx, cy + 4, cx, cy + 14, { color = "#2ecc71", width = 2 })
    draw.circle(cx, cy, 22, { color = "#2ecc7180", width = 1 })

    -- HUD box (top-left)
    draw.rect(20, 20, 240, 74, { color = "#000000a0", filled = true })
    draw.rect(20, 20, 240, 74, { color = "#2ecc71", width = 1 })
    draw.text(30, 28, "Hebnix overlay  " .. w .. "x" .. h, { color = "#ffffff", size = 14 })
    draw.text(30, 48, "goals this session: " .. (event_counts["GoalScored"] or 0),
        { color = "#dddddd", size = 13 })
    draw.text(30, 66, "ticks: " .. tick_count, { color = "#888888", size = 12 })

    -- Corner marker triangle
    draw.polygon({ { w - 20, 20 }, { w - 60, 20 }, { w - 20, 60 } }, { color = "#e74c3c90" })
end

-- floating window

function plugin.on_window(ui)
    ui.label("gui open: " .. tostring(hebnix.is_gui_open())
        .. "   rl: " .. tostring(hebnix.rl_connected()))
    ui.label("ticks: " .. tick_count)
    if last_goal then
        ui.colored_label("#2ecc71", "Last goal: " .. last_goal)
    end
    if last_touch_speed then
        ui.label("Last touch speed: " .. math.floor(last_touch_speed))
    end
    ui.separator()
    ui.label("Event counters:")
    for _, event_type in ipairs(event_order) do
        ui.label("  " .. event_type .. ": " .. event_counts[event_type])
    end
    if #event_order == 0 then
        ui.label("  (no events yet, join a match)")
    end
end

return plugin
