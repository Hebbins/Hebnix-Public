local plugin = {}

local SB = {
    left = 537,
    blue_bottom = 67,
    orange_top = 43,
    banner_distance = 57,
    board_w = 1033,
    board_h = 548,
    imbalance = 32,
    y_offcenter = 32,
}

local MUTATOR_EDGE = 1030
local REPLAY_SHIFT = 0
local X_OFFSET = -35
local X_OFFSET_FIRST = -35

local ICON_COL = -530.5
local ICON_PX = 100
local IMAGE_SCALE = 0.48
local GHOST_OPACITY = 0.4
local FADE_SECONDS = 0.3
local PRIVATE_PLAYLIST = 6
local TOURNAMENT_PLAYLIST = 34

local STYLES = { "Square", "Circle", "Full" }
local STYLE_DIRS = { Square = "square", Circle = "circle", Full = "full" }

local ICON_UNKNOWN = 0
local ICON_STEAM = 1
local ICON_PSN = 2
local ICON_XBOX = 3
local ICON_SWITCH = 4
local ICON_EPIC = 5

local PLATFORM_ICONS = {
    steam = ICON_STEAM,
    ps3 = ICON_PSN,
    ps4 = ICON_PSN,
    ps5 = ICON_PSN,
    psn = ICON_PSN,
    playstation = ICON_PSN,
    xbl = ICON_XBOX,
    xbox = ICON_XBOX,
    xboxone = ICON_XBOX,
    dingo = ICON_XBOX,
    switch = ICON_SWITCH,
    nnx = ICON_SWITCH,
    oldnnx = ICON_SWITCH,
    nintendo = ICON_SWITCH,
    epic = ICON_EPIC,
    epicgames = ICON_EPIC,
}

local players = {}
local roster = {}
local roster_seq = 0
local in_match = false
local in_replay = false
local match_ended = false
local match_guid = nil
local my_id = nil
local freeplay = false
local current_playlist = nil
local offline = false
local mutators = {}
local mutator_count = 0
local first_tab_pending = true
local in_first_open = false
local scoreboard_was_held = false
local was_drawing = false
local fade_from = nil
local shown_first_open = false
local log_key = nil
local last_layout = nil

local function icon_for(primary_id)
    local platform = tostring(primary_id or ""):match("^([^|]+)")
    if not platform then return ICON_UNKNOWN end
    return PLATFORM_ICONS[string.lower(platform)] or ICON_UNKNOWN
end

local function matchmade()
    if offline then return false end
    return current_playlist ~= nil and current_playlist ~= PRIVATE_PLAYLIST
end

local function clear_players()
    players = {}
    roster = {}
    roster_seq = 0
    match_guid = nil
    in_match = false
    in_replay = false
    current_playlist = nil
    offline = false
    mutators = {}
    mutator_count = 0
    freeplay = false
    log_key = nil
    first_tab_pending = true
    in_first_open = false
    scoreboard_was_held = false
    was_drawing = false
    fade_from = nil
end

local function update_players(event)
    local data = event and (event.data or event.Data)
    local source = type(data) == "table" and (data.Players or data.players) or nil
    if type(source) ~= "table" then return end

    local game = type(data) == "table" and (data.Game or data.game) or nil
    if type(game) == "table" then
        in_replay = game.bReplay == true or game.replay == true
    end

    local overlap, had = 0, next(roster) ~= nil
    for _, player in ipairs(source) do
        local id = tostring(player.PrimaryId or player.primary_id or "")
        local name = tostring(player.Name or player.name or "Unknown")
        local key = (id == "" or hebnix.is_bot(id)) and ("bot:" .. name) or id
        if roster[key] then overlap = overlap + 1 end
    end
    if had and overlap == 0 then
        roster = {}
        roster_seq = 0
    end

    local seen = {}
    for _, player in ipairs(source) do
        local primary_id = tostring(player.PrimaryId or player.primary_id or "")
        local is_bot = primary_id == "" or hebnix.is_bot(primary_id)
        local name = tostring(player.Name or player.name or "Unknown")
        local key = is_bot and ("bot:" .. name) or primary_id
        seen[key] = true

        local entry = roster[key]
        if not entry then
            roster_seq = roster_seq + 1
            entry = { order = roster_seq, awaiting_slot = true }
            roster[key] = entry
        end
        entry.id = primary_id
        entry.name = name
        entry.score = tonumber(player.Score or player.score) or 0
        entry.shortcut = tonumber(player.Shortcut or player.shortcut)
        entry.bot = is_bot
        entry.icon = is_bot and ICON_UNKNOWN or icon_for(primary_id)

        local reported = tonumber(player.TeamNum or player.team_num) or -1
        if reported == 0 or reported == 1 then
            entry.team = reported
            entry.ghost = entry.left or false
        else
            entry.ghost = true
            entry.team = entry.team or -1
        end
    end

    for key, entry in pairs(roster) do
        if not seen[key] then
            if entry.bot then roster[key] = nil else entry.ghost = true end
        end
    end

    if not matchmade() then
        for key, entry in pairs(roster) do
            if entry.ghost then roster[key] = nil end
        end
    end

    local claiming = {}
    for key, entry in pairs(roster) do
        if entry.awaiting_slot and (entry.team == 0 or entry.team == 1) then
            table.insert(claiming, { key = key, entry = entry })
        end
    end
    table.sort(claiming, function(a, b) return a.entry.order < b.entry.order end)
    for _, arrival in ipairs(claiming) do
        arrival.entry.awaiting_slot = nil
        local oldest, oldest_key = nil, nil
        for k, e in pairs(roster) do
            if e.ghost and e.team == arrival.entry.team and k ~= arrival.key
                and (not oldest or e.order < oldest.order) then
                oldest, oldest_key = e, k
            end
        end
        if oldest_key then roster[oldest_key] = nil end
    end

    players = {}
    for _, entry in pairs(roster) do table.insert(players, entry) end
    in_match = #players > 0
end

local function mark_left(event)
    local data = event and (event.data or event.Data)
    if type(data) ~= "table" then return end
    local id = tostring(data.PrimaryId or data.primary_id or "")
    local name = tostring(data.PlayerName or data.player_name or "")
    local key = (id == "" or hebnix.is_bot(id)) and ("bot:" .. name) or id
    local entry = roster[key]
    if entry then
        entry.left = true
        entry.ghost = true
    end
end

local function spectating()
    if not my_id or my_id == "" or #players == 0 then return false end
    for _, player in ipairs(players) do
        if player.id == my_id and (player.team == 0 or player.team == 1) then
            return false
        end
    end
    return true
end

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

local function refresh_playlist()
    if not log_key then
        hebnix.clear_launch_log()
        log_key = hebnix.parse_launch_log_async(false)
        return
    end
    local info = hebnix.launch_log_result(log_key)
    if type(info) ~= "table" then return end
    if type(info.session) == "table" then my_id = info.session.primary_id end
    if type(info.game) ~= "table" then return end
    current_playlist = tonumber(info.game.playlist_id)
    offline = info.game.offline == true
    mutators = info.game.mutators or {}
    freeplay = false
    for _, tag in ipairs(mutators) do
        if tag == "Freeplay" then freeplay = true end
    end
    mutator_count = math.max(tonumber(info.game.mutator_count) or 0, #mutators)
end

local function shows_mutator_strip()
    if current_playlist == TOURNAMENT_PLAYLIST then return true end
    return mutator_count > 0
end

local function sb_layout(w, h, ui_scale, x_offset, mutator_edge, blues, oranges, replay_shift)
    local scale
    if w / h > 1.5 then
        scale = 0.507 * h / SB.board_h
    else
        scale = 0.615 * w / SB.board_w
    end
    local s = scale * ui_scale

    local cx = w / 2
    if mutator_edge > 0 then
        local strip_cx = w - mutator_edge * s
        if strip_cx < cx then cx = strip_cx end
    end
    cx = cx + x_offset * s
    cx = cx - replay_shift * s

    local cy = h / 2 + SB.y_offcenter * s

    local difference = blues - oranges
    local lopsided = (blues == 0) ~= (oranges == 0)
    local sign = difference >= 0 and 1 or -1
    cy = cy + SB.imbalance * (difference - (lopsided and sign or 0)) * s

    return {
        scale = s,
        centre = cx,
        size = ICON_PX * IMAGE_SCALE * s,
        blue_y = cy + (-SB.blue_bottom + 6 * (4 - blues) - SB.banner_distance * blues + 9) * s,
        orange_y = cy + SB.orange_top * s,
        separation = SB.banner_distance * s,
    }
end

local function last_layout_readout()
    if not last_layout then return "No frame drawn yet, hold the scoreboard once." end
    return string.format(
        "%dv%d, %d ghost, %d listed, 1 unit = %.2f px, icon %.0f px, blue row1 %.0f%s%s",
        last_layout.blues or 0, last_layout.oranges or 0,
        last_layout.ghosts or 0, #players,
        last_layout.scale, last_layout.size, last_layout.blue_y,
        last_layout.first_open and " (first open)" or "",
        last_layout.replay and " (replay)" or "")
        .. (last_layout.watching and " (spectating)" or "")
end

function plugin.on_load()
    clear_players()
end

function plugin.on_unload()
    clear_players()
end

function plugin.on_game_event(event_type, event)
    local guid = event and (event.match_guid or event.MatchGuid)
    if guid and guid ~= "" and guid ~= match_guid then
        match_guid = guid
        roster = {}
        roster_seq = 0
        players = {}
    end

    if event_type == "UpdateState" then
        update_players(event)
    elseif event_type == "PlayerLeft" then
        mark_left(event)
    elseif event_type == "MatchCreated" or event_type == "MatchInitialized" then
        in_match = true
        match_ended = false
        current_playlist = nil
        offline = false
        mutators = {}
        mutator_count = 0
        log_key = nil
        roster = {}
        roster_seq = 0
        first_tab_pending = true
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
        clear_players()
    end
end

function plugin.on_tick()
    if in_match and not current_playlist then refresh_playlist() end

    if in_match then
        local held = hebnix.is_action_pressed("scoreboard")
        if held and not scoreboard_was_held then
            in_first_open = first_tab_pending
            first_tab_pending = false
        elseif not held then
            in_first_open = false
        end
        scoreboard_was_held = held
    end
end

function plugin.on_overlay(draw, w, h)
    if not in_match or match_ended or #players == 0 or freeplay then return end

    local held = hebnix.is_action_pressed("scoreboard")
    local opacity = 1.0
    if held then
        was_drawing = true
        fade_from = nil
        shown_first_open = in_first_open
    else
        if was_drawing then
            was_drawing = false
            fade_from = hebnix.monotonic_seconds()
        end
        if not fade_from then return end
        local elapsed = hebnix.monotonic_seconds() - fade_from
        if elapsed >= FADE_SECONDS then
            fade_from = nil
            return
        end
        opacity = 1.0 - elapsed / FADE_SECONDS
    end

    local style = hebnix.get_string("platform_display_style", "Square")
    local dir = STYLE_DIRS[style] or STYLE_DIRS.Square
    local hide_steam = hebnix.get_bool("platform_display_hide_steam", false)
    local hide_self = hebnix.get_bool("platform_display_hide_self", false)
    local show_ghosts = hebnix.get_bool("platform_display_show_ghosts", true)
    local ui_scale = (hebnix.ui_scale and hebnix.ui_scale() or 1.0)
        * hebnix.get_number("platform_display_display_scale", 100) / 100

    local list = sorted_players()
    local blues, oranges, ghosts = 0, 0, 0
    for _, player in ipairs(list) do
        if player.team == 0 then
            blues = blues + 1
        elseif player.team == 1 then
            oranges = oranges + 1
        end
        if player.ghost then ghosts = ghosts + 1 end
    end

    local mutator_edge = shows_mutator_strip()
        and hebnix.get_number("platform_display_mutator_edge", MUTATOR_EDGE) or 0
    local x_offset = shown_first_open
        and hebnix.get_number("platform_display_x_offset_first", X_OFFSET_FIRST)
        or hebnix.get_number("platform_display_x_offset", X_OFFSET)
    local replay_shift = in_replay
        and hebnix.get_number("platform_display_replay_shift", REPLAY_SHIFT) or 0
    local layout = sb_layout(w, h, ui_scale, x_offset, mutator_edge, blues, oranges, replay_shift)
    local y_nudge = hebnix.get_number("platform_display_y_nudge", 0) * layout.scale
    local icon_x = layout.centre
        + hebnix.get_number("platform_display_icon_x", ICON_COL) * layout.scale
    layout.first_open = shown_first_open
    layout.replay = in_replay
    layout.blues = blues
    layout.oranges = oranges
    layout.ghosts = ghosts
    layout.watching = spectating()
    last_layout = layout

    local blue_row, orange_row = -1, -1
    for _, player in ipairs(list) do
        if player.team == 0 then
            blue_row = blue_row + 1
        elseif player.team == 1 then
            orange_row = orange_row + 1
        else
            goto continue
        end
        if not player.bot
            and (show_ghosts or not player.ghost)
            and not (hide_steam and player.icon == ICON_STEAM)
            and not (hide_self and my_id ~= nil and player.id == my_id) then
            local y = y_nudge + (player.team == 0
                and layout.blue_y + layout.separation * blue_row
                or layout.orange_y + layout.separation * orange_row)
            local alpha = player.ghost and opacity * GHOST_OPACITY or opacity
            draw.image(string.format("assets/%s/%d.png", dir, player.icon),
                icon_x, y, layout.size, layout.size, { opacity = alpha })
        end
        ::continue::
    end
end

function plugin.on_settings(ui)
    ui.heading("Icons")
    ui.combo_box("platform_display_style", "Style", STYLES)
    ui.checkbox("platform_display_show_ghosts", "Show players who left, dimmed", true)
    ui.checkbox("platform_display_hide_steam", "Hide the Steam icon", false)
    ui.checkbox("platform_display_hide_self", "Hide my own icon", false)
    ui.space(8)

    ui.collapsing("Advanced alignment", function(ui)
        ui.label("Mutators: " .. mutator_count
            .. (#mutators > 0 and " (" .. table.concat(mutators, ", ") .. ")" or "")
            .. (shows_mutator_strip() and ", board shifted" or ""))
        ui.label("Display scale has no home in the save, match it to the game.")
        ui.label(string.format("Interface scale: %.2f, read from the save",
            hebnix.ui_scale and hebnix.ui_scale() or 1.0))
        ui.slider("platform_display_display_scale", "Display scale", 90, 100, 100)
        ui.label("Below are 1080p pixels, they scale with the resolution.")
        ui.label("Set X offset in a plain match first, the strip edge only bites when it overlaps.")
        ui.slider("platform_display_x_offset", "X offset", -250, 250, X_OFFSET)
        ui.slider("platform_display_x_offset_first", "X offset, first tab", -250, 250, X_OFFSET_FIRST)
        ui.slider("platform_display_mutator_edge", "Mutator strip edge", 800, 1300, MUTATOR_EDGE)
        ui.slider("platform_display_replay_shift", "Replay board shift", -250, 250, REPLAY_SHIFT)
        ui.slider("platform_display_icon_x", "Icon column", -700, -300, ICON_COL)
        ui.slider("platform_display_y_nudge", "Y nudge", -100, 100, 0)
        ui.label(last_layout_readout())
    end)
    ui.space(8)

    ui.label("Version 1.0.0")
end

return plugin
