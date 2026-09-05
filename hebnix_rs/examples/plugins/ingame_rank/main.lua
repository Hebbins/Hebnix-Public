-- InGameRank: rank icons pinned to the rows of the in game scoreboard.
-- layout ported from the bakkesmod plugin, ScoreboardPosition.cpp.

local plugin = {}

-- offsets in 1080p pixels, everything gets multiplied by scale
local SB = {
    left = 537,
    blue_bottom = 67,
    orange_top = 43,
    banner_distance = 57,
    board_w = 1033,
    board_h = 548,
    imbalance = 32,
    skip_tick = 67,
    y_offcenter = 32,
}

-- measured at 16:9. bakkesmod pins the center 1005 from the right edge, which
-- works out to 45 here and to no shift at all past 21:9.
local MUTATOR_SHIFT = 110

local IMAGE_SCALE = 0.48
local TIER_W, TIER_H = 150, 100
local DIV_W, DIV_H = 100, 25
local DIV_STEP = 25
local PLAYLIST_PX = 100
local TIER_UNSYNCED = 23

local PLAYLISTS = {
    { id = 10, name = "Solo Duel", image = "0.png" },
    { id = 11, name = "Doubles", image = "1.png" },
    { id = 13, name = "Standard", image = "2.png" },
    { id = 61, name = "Quads", image = "8.png" },
    { id = 27, name = "Hoops", image = "3.png", extra = true },
    { id = 28, name = "Rumble", image = "4.png", extra = true },
    { id = 29, name = "Dropshot", image = "5.png", extra = true },
    { id = 30, name = "Snow Day", image = "6.png", extra = true },
    { id = 63, name = "Heatseeker", image = "9.png", extra = true },
    { id = 34, name = "Tournaments", image = "7.png", tournament = true },
}

local BUTTONS = {
    "btn_south", "btn_east", "btn_west", "btn_north",
    "dpad_up", "dpad_down", "dpad_left", "dpad_right",
    "bumper_l", "bumper_r", "trigger_l", "trigger_r",
    "stick_l", "stick_r",
}

local players = {}
local request_keys = {}
local in_match = false
local in_replay = false
local match_ended = false
local current_playlist = nil
local mutators = {}
local log_key = nil
local mode = "Current"
local cycle_was_pressed = false
local capture_target = nil
local waiting_for_release = false

local function playlist_by_id(id)
    for _, entry in ipairs(PLAYLISTS) do
        if entry.id == id then return entry end
    end
    return nil
end

local function mode_options()
    local options = { "Current", "Best" }
    for _, entry in ipairs(PLAYLISTS) do table.insert(options, entry.name) end
    return options
end

local function mode_allowed(entry, extras, tournaments)
    if entry.extra and not extras then return false end
    if entry.tournament and not tournaments then return false end
    return true
end

local function format_bind_name(bind)
    if string.sub(bind or "", 1, 4) ~= "pad_" then return bind or "" end
    local names = {
        pad_btn_south = "Gamepad South (A/Cross)",
        pad_btn_east = "Gamepad East (B/Circle)",
        pad_btn_west = "Gamepad West (X/Square)",
        pad_btn_north = "Gamepad North (Y/Triangle)",
        pad_bumper_l = "Gamepad L1/LB",
        pad_bumper_r = "Gamepad R1/RB",
        pad_trigger_l = "Gamepad L2/LT",
        pad_trigger_r = "Gamepad R2/RT",
        pad_dpad_up = "Gamepad D-Pad Up",
        pad_dpad_down = "Gamepad D-Pad Down",
        pad_dpad_left = "Gamepad D-Pad Left",
        pad_dpad_right = "Gamepad D-Pad Right",
        pad_stick_l = "Gamepad L3/LS",
        pad_stick_r = "Gamepad R3/RS",
    }
    return names[bind] or bind
end

local function manual_controller_press()
    for _, pad in ipairs(hebnix.controllers() or {}) do
        for _, button in ipairs(BUTTONS) do
            if pad[button] then return "pad_" .. button end
        end
    end
    return nil
end

local function bind_pressed(bind)
    if not bind or bind == "" then return false end
    if string.sub(bind, 1, 4) ~= "pad_" then
        return hebnix.is_bind_pressed(bind)
    end
    local button = string.sub(bind, 5)
    for _, pad in ipairs(hebnix.controllers() or {}) do
        if pad[button] then return true end
    end
    return false
end

local function profile_identity(primary_id)
    local platform, account_id = tostring(primary_id or ""):match("^([^|]+)|([^|]+)")
    if not platform or not account_id or account_id == "" then return nil, nil end
    platform = string.lower(platform)
    if platform == "epicgames" then platform = "epic" end
    if platform == "xbl" or platform == "xbox" then platform = "xboxone" end
    if platform == "ps4" or platform == "ps5" or platform == "playstation" then platform = "psn" end
    if platform == "nintendo" then platform = "switch" end
    if platform ~= "epic" and platform ~= "steam" and platform ~= "xboxone"
        and platform ~= "psn" and platform ~= "switch" then
        return nil, nil
    end
    return platform, account_id
end

local function request_profile(primary_id)
    if request_keys[primary_id] then return request_keys[primary_id] end
    local platform, account_id = profile_identity(primary_id)
    if not platform then return nil end
    local key = hebnix.fetch_profile_async(platform, account_id)
    if key then request_keys[primary_id] = key end
    return key
end

local function clear_players(clear_cache)
    players = {}
    request_keys = {}
    in_match = false
    in_replay = false
    current_playlist = nil
    mutators = {}
    log_key = nil
    if clear_cache then hebnix.clear_stats_cache() end
end

local function update_players(event)
    local data = event and (event.data or event.Data)
    local source = type(data) == "table" and (data.Players or data.players) or nil
    if type(source) ~= "table" then return end

    local game = type(data) == "table" and (data.Game or data.game) or nil
    if type(game) == "table" then
        in_replay = game.bReplay == true or game.replay == true
    end

    local updated = {}
    for index, player in ipairs(source) do
        local primary_id = tostring(player.PrimaryId or player.primary_id or "")
        local is_bot = primary_id == "" or hebnix.is_bot(primary_id)
        local request_key = nil
        if not is_bot then request_key = request_profile(primary_id) end
        table.insert(updated, {
            id = primary_id,
            name = tostring(player.Name or player.name or "Unknown"),
            team = tonumber(player.TeamNum or player.team_num) or -1,
            score = tonumber(player.Score or player.score) or 0,
            shortcut = tonumber(player.Shortcut or player.shortcut),
            order = index,
            bot = is_bot,
            request_key = request_key,
        })
    end
    players = updated
    in_match = #players > 0
end

-- rl orders by team, then score desc, then player id desc
local function sorted_players()
    local sorted = {}
    for _, player in ipairs(players) do table.insert(sorted, player) end
    table.sort(sorted, function(a, b)
        if a.team ~= b.team then
            local a_team = a.team == 0 and 0 or (a.team == 1 and 1 or 2)
            local b_team = b.team == 0 and 0 or (b.team == 1 and 1 or 2)
            return a_team < b_team
        end
        if a.score ~= b.score then return a.score > b.score end
        if a.shortcut and b.shortcut and a.shortcut ~= b.shortcut then
            return a.shortcut > b.shortcut
        end
        if a.id ~= b.id then return a.id > b.id end
        return a.order < b.order
    end)
    return sorted
end

-- casual and private carry no ranked playlist id, the team sizes do
local TEAM_SIZE_PLAYLIST = { [1] = 10, [2] = 11, [3] = 13, [4] = 61 }

local function auto_playlist()
    local counts = {}
    local biggest = 0
    for _, player in ipairs(players) do
        if player.team == 0 or player.team == 1 then
            counts[player.team] = (counts[player.team] or 0) + 1
            if counts[player.team] > biggest then biggest = counts[player.team] end
        end
    end
    return TEAM_SIZE_PLAYLIST[biggest]
end

local function refresh_playlist()
    if not log_key then
        hebnix.clear_launch_log() -- the parse is cached, the last match's id would stick
        log_key = hebnix.parse_launch_log_async(false)
        return
    end
    local info = hebnix.launch_log_result(log_key)
    if type(info) ~= "table" or type(info.game) ~= "table" then return end
    current_playlist = tonumber(info.game.playlist_id)
    mutators = info.game.mutators or {}
end

-- these load a mutator package like the rest but never reach the strip
local OFF_STRIP = { Freeplay = true, MatchCreatorAdminEnabled = true }

-- the strip sits right of the board and pushes it left, tournaments always
-- carry one whether or not GameTags names anything
local function shows_mutator_strip()
    if current_playlist == 34 then return true end
    for _, tag in ipairs(mutators) do
        if not OFF_STRIP[tag] then return true end
    end
    return false
end

-- black magic, do not touch without a screenshot to compare against
local function sb_layout(w, h, ui_scale, mutator_shift, blues, oranges, replaying)
    local scale
    if w / h > 1.5 then
        scale = 0.507 * h / SB.board_h
    else
        scale = 0.615 * w / SB.board_w
    end
    local s = scale * ui_scale

    local cx = w / 2
    local cy = h / 2 + SB.y_offcenter * s
    cx = cx - mutator_shift * s
    if replaying then cx = cx - SB.skip_tick * s end

    local difference = blues - oranges
    local lopsided = (blues == 0) ~= (oranges == 0)
    local sign = difference >= 0 and 1 or -1
    cy = cy + SB.imbalance * (difference - (lopsided and sign or 0)) * s

    return {
        scale = s,
        x = cx + (-SB.left - TIER_W * IMAGE_SCALE) * s,
        div_x = cx + (-SB.left - DIV_W * IMAGE_SCALE) * s,
        blue_y = cy + (-SB.blue_bottom + 6 * (4 - blues) - SB.banner_distance * blues + 9) * s,
        orange_y = cy + SB.orange_top * s,
        separation = SB.banner_distance * s,
    }
end

local function rank_for(stats, playlist_id)
    for key, rank in pairs((stats and stats.ranks) or {}) do
        if tonumber(rank.playlist_id or key) == playlist_id then return rank end
    end
    return nil
end

local function division_of(rank)
    local raw = tonumber(rank and rank.division_id)
    if raw then return math.max(0, math.min(3, math.floor(raw))) end
    local name = string.upper(tostring((rank and rank.division_name) or ""))
    local levels = { I = 0, II = 1, III = 2, IV = 3 }
    local roman = name:match("(IV)$") or name:match("(III)$") or name:match("(II)$") or name:match("(I)$")
    return roman and levels[roman] or 0
end

-- rl's own mmr to rank curve, the api returns tier 0 for placements
local function rank_from_mmr(mmr, playlist_id)
    if playlist_id ~= 10 and playlist_id ~= 11 and playlist_id ~= 13 then return nil end
    local solo = playlist_id == 10
    local step = solo and 155.0 or 175.0
    local left = mmr - step
    local tier = 1
    while left >= 0 and tier < 22 do
        step = 60.0
        tier = tier + 1
        if not solo then
            if tier >= 12 then step = 80.0 end
            if tier >= 15 then step = 120.0 end
            if tier >= 18 then step = 140.0 end
            if tier >= 20 then step = 160.0 end
        end
        left = left - step
    end
    if tier == 22 then return 22, 0 end
    left = left + step
    step = step + 15
    return tier, math.floor(left * (4 / step))
end

local function entry_of(stats, playlist_id, calculate_unranked)
    local rank = rank_for(stats, playlist_id)
    if not rank then return nil end
    local tier = math.floor(tonumber(rank.tier_id) or 0)
    local division = division_of(rank)
    local mmr = math.floor(tonumber(rank.mmr) or 0)
    local unranked = tier == 0
    -- 600 with no games played is the placeholder the api hands back
    if unranked and calculate_unranked and (mmr ~= 600 or (tonumber(rank.matches_played) or 0) > 0) then
        local guess_tier, guess_div = rank_from_mmr(mmr, playlist_id)
        if guess_tier then
            tier = guess_tier
            division = guess_div
        end
    end
    return { tier = tier, division = division, unranked = unranked, playlist_id = playlist_id }
end

local function display_rank(stats, wanted, fallback, extras, tournaments, calculate_unranked)
    local best = nil
    if wanted then
        best = entry_of(stats, wanted, calculate_unranked)
        if best and best.unranked and not calculate_unranked then
            best.tier = 0
            best.division = -1
        end
    end
    if not best and (fallback or not wanted) then
        for _, entry in ipairs(PLAYLISTS) do
            if mode_allowed(entry, extras, tournaments) then
                local current = entry_of(stats, entry.id, calculate_unranked)
                if current then
                    if current.unranked and not calculate_unranked then
                        if not best then best = { tier = 0, division = -1, unranked = false } end
                    elseif not best or best.tier + (best.division + 1) * 0.1
                        < current.tier + (current.division + 1) * 0.1 then
                        best = current
                    end
                end
            end
        end
    end

    if not best then return { tier = TIER_UNSYNCED, division = -1, unranked = false } end
    if best.tier == 22 then best.division = -1 end
    if best.tier == 0 then
        best.division = -1
        best.unranked = false
    end
    return best
end

local function draw_row(draw, rank, layout, y, show_division, show_playlist, calculate_unranked)
    local s = layout.scale * IMAGE_SCALE
    local with_division = show_division and rank.division > -1 and rank.tier < 22
    local x = layout.x - (with_division and DIV_W * s or 0)

    local tier = math.max(0, math.min(TIER_UNSYNCED, rank.tier))
    draw.image("assets/tiers/" .. tier .. ".png", x, y, TIER_W * s, TIER_H * s)
    if rank.unranked and calculate_unranked then
        draw.image("assets/tiers/0.png", x, y, TIER_W * s * 0.5, TIER_H * s * 0.5)
    end

    if with_division then
        local colour = math.floor((tier - 1) / 3) + 1
        for slot = 0, 3 do
            local image = slot <= rank.division and colour or 0
            draw.image("assets/divisions/" .. image .. ".png",
                layout.div_x, y + (3 - slot) * DIV_STEP * s, DIV_W * s, DIV_H * s)
        end
    end

    if show_playlist and rank.playlist_id and rank.tier > 0 and rank.tier < TIER_UNSYNCED then
        local entry = playlist_by_id(rank.playlist_id)
        if entry then
            draw.image("assets/playlists/" .. entry.image,
                x - PLAYLIST_PX * s, y, PLAYLIST_PX * s, PLAYLIST_PX * s)
        end
    end
end

local function cycle_mode()
    local extras = hebnix.get_bool("ingame_rank_extras", false)
    local tournaments = hebnix.get_bool("ingame_rank_tournaments", false)
    local options = { "Current", "Best" }
    for _, entry in ipairs(PLAYLISTS) do
        if mode_allowed(entry, extras, tournaments) then table.insert(options, entry.name) end
    end
    local index = 0
    for i, option in ipairs(options) do
        if option == mode then index = i break end
    end
    mode = options[(index % #options) + 1]
    hebnix.set("ingame_rank_mode", mode)
end

local function bind_row(ui, label, setting, target)
    local bind = hebnix.get_string(setting, "")
    ui.horizontal(function()
        ui.label(label .. ": " .. (bind ~= "" and format_bind_name(bind) or "(none)"))
        if capture_target == target then
            ui.colored_label("#d35400", "Press any key/button...")
        else
            if ui.button("Set") then
                hebnix.capture_bind_async(10)
                capture_target = target
                waiting_for_release = true
            end
            if ui.button("Clear") then hebnix.set(setting, "") end
        end
    end)
end

function plugin.on_load()
    hebnix.log("InGameRank loaded")
    local stored = hebnix.get_string("ingame_rank_mode", "Current")
    for _, option in ipairs(mode_options()) do
        if option == stored then mode = stored break end
    end
end

function plugin.on_unload()
    clear_players(false)
end

function plugin.on_game_event(event_type, event)
    if event_type == "UpdateState" then
        update_players(event)
    elseif event_type == "MatchCreated" or event_type == "MatchInitialized" then
        in_match = true
        match_ended = false
        current_playlist = nil
        mutators = {}
        log_key = nil
    elseif event_type == "RoundStarted" or event_type == "CountdownBegin" then
        in_match = true
        match_ended = false
    elseif event_type == "GoalReplayStart" then
        in_replay = true
    elseif event_type == "GoalReplayEnd" then
        in_replay = false
    elseif event_type == "MatchEnded" then
        match_ended = true
    elseif event_type == "GameLeft" or event_type == "MatchDestroyed" then
        match_ended = false
        clear_players(false)
    end
end

function plugin.on_tick()
    if in_match and not current_playlist then refresh_playlist() end

    local cycle_bind = hebnix.get_string("ingame_rank_cycle_bind", "")
    if cycle_bind ~= "" and not capture_target then
        local pressed = bind_pressed(cycle_bind)
        if pressed and not cycle_was_pressed then cycle_mode() end
        cycle_was_pressed = pressed
    else
        cycle_was_pressed = false
    end

    if not capture_target then return end
    if waiting_for_release then
        if not manual_controller_press() then waiting_for_release = false end
        return
    end

    local status, bind = hebnix.capture_bind_result()
    if status == "done" then
        hebnix.set("ingame_rank_cycle_bind", bind or "")
        capture_target = nil
    elseif status == "timeout" then
        capture_target = nil
    else
        local manual = manual_controller_press()
        if manual then
            hebnix.set("ingame_rank_cycle_bind", manual)
            capture_target = nil
        end
    end
end

function plugin.on_overlay(draw, w, h)
    if not in_match or match_ended or #players == 0 then return end
    if not hebnix.is_action_pressed("scoreboard") then return end

    local extras = hebnix.get_bool("ingame_rank_extras", false)
    local tournaments = hebnix.get_bool("ingame_rank_tournaments", false)
    local show_division = hebnix.get_bool("ingame_rank_show_division", false)
    local show_playlist = hebnix.get_bool("ingame_rank_show_playlist", true)
    local calculate_unranked = hebnix.get_bool("ingame_rank_calculate_unranked", true)
    local ui_scale = hebnix.get_number("ingame_rank_interface_scale", 100) / 100
        * hebnix.get_number("ingame_rank_display_scale", 100) / 100

    local wanted, fallback = nil, false
    if mode == "Current" then
        wanted = playlist_by_id(current_playlist) and current_playlist or auto_playlist()
        fallback = true
    elseif mode ~= "Best" then
        for _, entry in ipairs(PLAYLISTS) do
            if entry.name == mode then wanted = entry.id break end
        end
    end

    local list = sorted_players()
    local blues, oranges = 0, 0
    for _, player in ipairs(list) do
        if player.team == 0 then blues = blues + 1 else oranges = oranges + 1 end
    end

    local mutator_shift = shows_mutator_strip()
        and hebnix.get_number("ingame_rank_mutator_shift", MUTATOR_SHIFT) or 0
    local layout = sb_layout(w, h, ui_scale, mutator_shift, blues, oranges, in_replay)
    local x_nudge = hebnix.get_number("ingame_rank_x_nudge", 0) * layout.scale
    local y_nudge = hebnix.get_number("ingame_rank_y_nudge", 0) * layout.scale
    layout.x = layout.x + x_nudge
    layout.div_x = layout.div_x + x_nudge

    local blue_row, orange_row = -1, -1
    for _, player in ipairs(list) do
        if player.team == 0 then blue_row = blue_row + 1 else orange_row = orange_row + 1 end
        if not player.bot and player.team <= 1 then
            local y = y_nudge + (player.team == 0
                and layout.blue_y + layout.separation * blue_row
                or layout.orange_y + layout.separation * orange_row)

            local stats = player.request_key and hebnix.stats_result(player.request_key) or nil
            local rank = { tier = TIER_UNSYNCED, division = -1, unranked = false }
            if type(stats) == "table" and not stats.error then
                rank = display_rank(stats, wanted, fallback, extras, tournaments, calculate_unranked)
            end
            -- the icon only earns its place when the row is not the asked playlist
            draw_row(draw, rank, layout, y, show_division,
                show_playlist and rank.playlist_id ~= wanted, calculate_unranked)
        end
    end
end

function plugin.on_settings(ui)
    ui.heading("Rank shown")
    local chosen = ui.combo_box("ingame_rank_mode", "Playlist", mode_options())
    if chosen and chosen ~= mode then mode = chosen end
    bind_row(ui, "Cycle bind", "ingame_rank_cycle_bind", "cycle")
    ui.checkbox("ingame_rank_extras", "Include extra modes", false)
    ui.checkbox("ingame_rank_tournaments", "Include tournaments", false)
    ui.checkbox("ingame_rank_calculate_unranked", "Guess placement ranks from MMR", true)
    ui.space(8)

    ui.heading("Appearance")
    ui.checkbox("ingame_rank_show_division", "Show division bars", false)
    ui.checkbox("ingame_rank_show_playlist", "Show playlist icon on Best", true)
    ui.space(8)

    ui.collapsing("Advanced alignment", function(ui)
        ui.label("Mutators: " .. (#mutators > 0 and table.concat(mutators, ", ") or "none")
            .. (shows_mutator_strip() and " (board shifted)" or ""))
        ui.label("Match these to Rocket League's Interface and Display Scale sliders.")
        ui.slider("ingame_rank_interface_scale", "Interface scale", 50, 100, 100)
        ui.slider("ingame_rank_display_scale", "Display scale", 90, 100, 100)
        ui.label("Below are 1080p pixels, they scale with the resolution.")
        ui.slider("ingame_rank_mutator_shift", "Mutator board shift", 0, 250, MUTATOR_SHIFT)
        ui.slider("ingame_rank_x_nudge", "X nudge", -100, 100, 0)
        ui.slider("ingame_rank_y_nudge", "Y nudge", -100, 100, 0)
    end)
    ui.space(8)

    if ui.button("Clear Profile Cache") then clear_players(true) end
    ui.space(4)
    ui.label("Version 4.0.0")
end

return plugin
