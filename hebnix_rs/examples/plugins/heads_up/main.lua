local plugin = {}

local CACHE_FILE = "encounters.json"
local PRIVATE_PLAYLIST = 6
local CASUAL_MMR = 0
local RANKED_MMR_PLAYLISTS = {
    [10] = true, [11] = true, [13] = true, [27] = true, [28] = true,
    [29] = true, [30] = true, [34] = true, [61] = true, [63] = true,
}
local TEAM_SIZE_PLAYLIST = { [1] = 10, [2] = 11, [3] = 13, [4] = 61 }
local ROSTER_SETTLE_SECONDS = 1.5
local MMR_TIMEOUT_SECONDS = 12.0
local LOG_RETRY_SECONDS = 1.0

local encounters = {}
local players = {}
local recorded_ids = {}
local request_keys = {}
local stats_by_id = {}
local stats_done = {}

local in_match = false
local match_guid = nil
local match_started_at = 0
local roster_changed_at = 0
local toast_sent = false
local mmr_started_at = nil

local my_id = nil
local my_team = nil
local current_playlist = nil
local offline = false
local log_key = nil
local next_log_attempt = 0

local function now()
    return hebnix.monotonic_seconds()
end

local function clamped_number(key, default, minimum, maximum)
    local value = math.floor(tonumber(hebnix.get_number(key, default)) or default)
    return math.max(minimum, math.min(maximum, value))
end

local function display_name(name)
    local cleaned = tostring(name or "Unknown"):gsub("[\r\n]", " ")
    if cleaned == "" then return "Unknown" end
    return cleaned
end

local function load_encounters()
    encounters = {}
    local text = hebnix.read_asset_text(CACHE_FILE)
    if not text or text == "" then return end

    local ok, decoded = pcall(hebnix.json_decode, text)
    if not ok or type(decoded) ~= "table" then
        hebnix.log("Heads Up: encounter cache is invalid; starting with an empty cache")
        return
    end

    local cached_players = type(decoded.players) == "table" and decoded.players or decoded
    for id, entry in pairs(cached_players) do
        if type(id) == "string" and id ~= "" then
            if type(entry) == "number" then
                encounters[id] = { encounters = math.max(0, math.floor(entry)) }
            elseif type(entry) == "table" then
                encounters[id] = {
                    encounters = math.max(0, math.floor(tonumber(entry.encounters) or 0)),
                    last_name = display_name(entry.last_name),
                }
            end
        end
    end
end

local function save_encounters()
    local ok, text = pcall(hebnix.json_encode, { version = 1, players = encounters })
    if not ok or not hebnix.write_asset(CACHE_FILE, text) then
        hebnix.log("Heads Up: could not save the encounter cache")
    end
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

local function request_stats(primary_id)
    if stats_done[primary_id] or request_keys[primary_id] then return end
    local platform, account_id = profile_identity(primary_id)
    if not platform then
        stats_done[primary_id] = true
        return
    end
    local key = hebnix.fetch_profile_async(platform, account_id)
    if key then
        request_keys[primary_id] = key
        if not mmr_started_at then mmr_started_at = now() end
    else
        stats_done[primary_id] = true
    end
end

local function reset_match_state(guid)
    players = {}
    recorded_ids = {}
    request_keys = {}
    stats_by_id = {}
    stats_done = {}
    in_match = true
    match_guid = guid
    match_started_at = now()
    roster_changed_at = match_started_at
    toast_sent = false
    mmr_started_at = nil
    my_team = nil
    current_playlist = nil
    offline = false
    log_key = nil
    next_log_attempt = 0
    hebnix.clear_launch_log()
end

local function clear_match_state()
    players = {}
    recorded_ids = {}
    request_keys = {}
    stats_by_id = {}
    stats_done = {}
    in_match = false
    match_guid = nil
    toast_sent = false
    mmr_started_at = nil
    my_team = nil
    current_playlist = nil
    offline = false
    log_key = nil
    next_log_attempt = 0
end

local function update_players(event)
    local data = event and (event.data or event.Data)
    local source = type(data) == "table" and (data.Players or data.players) or nil
    if type(source) ~= "table" then return end

    local updated = {}
    local signature_parts = {}
    for _, player in ipairs(source) do
        local id = tostring(player.PrimaryId or player.primary_id or "")
        if id ~= "" and not hebnix.is_bot(id) then
            local team = tonumber(player.TeamNum or player.team_num) or -1
            local entry = {
                id = id,
                name = display_name(player.Name or player.name),
                team = team,
            }
            table.insert(updated, entry)
            table.insert(signature_parts, id .. ":" .. tostring(team))
        end
    end
    table.sort(signature_parts)
    local signature = table.concat(signature_parts, ";")

    local old_parts = {}
    for _, player in ipairs(players) do
        table.insert(old_parts, player.id .. ":" .. tostring(player.team))
    end
    table.sort(old_parts)
    if signature ~= table.concat(old_parts, ";") then roster_changed_at = now() end

    players = updated
    if not in_match and #players > 0 then reset_match_state(nil) end
end

local function refresh_match_info()
    if current_playlist and my_id and my_id ~= "" then return end
    local current_time = now()
    if not log_key then
        if current_time < next_log_attempt then return end
        log_key = hebnix.parse_launch_log_async(false)
        return
    end

    local info = hebnix.launch_log_result(log_key)
    if type(info) ~= "table" then return end
    if type(info.session) == "table" and info.session.primary_id then
        my_id = tostring(info.session.primary_id)
    end
    if type(info.game) ~= "table" then
        hebnix.clear_launch_log()
        log_key = nil
        next_log_attempt = current_time + LOG_RETRY_SECONDS
        return
    end
    current_playlist = tonumber(info.game.playlist_id)
    offline = info.game.offline == true
end

local function local_team()
    if not my_id or my_id == "" then return nil end
    for _, player in ipairs(players) do
        if player.id == my_id and (player.team == 0 or player.team == 1) then
            return player.team
        end
    end
    return nil
end

local function should_consider(player)
    if player.id == my_id then return false end
    if not hebnix.get_bool("heads_up_warn_only_opponents", true) then return true end
    return my_team ~= nil and (player.team == 0 or player.team == 1) and player.team ~= my_team
end

local function record_new_encounters()
    my_team = local_team()
    if not my_team then return end

    local changed = false
    for _, player in ipairs(players) do
        if player.id ~= my_id and not recorded_ids[player.id] then
            recorded_ids[player.id] = {
                id = player.id,
                name = player.name,
                team = player.team,
            }
            local cached = encounters[player.id] or { encounters = 0 }
            cached.encounters = math.max(0, math.floor(tonumber(cached.encounters) or 0)) + 1
            cached.last_name = player.name
            encounters[player.id] = cached
            changed = true
        end
    end
    if changed then save_encounters() end
end

local function poll_stats()
    for id, key in pairs(request_keys) do
        if not stats_done[id] then
            local result = hebnix.stats_result(key)
            if type(result) == "table" then
                stats_by_id[id] = result
                stats_done[id] = true
            end
        end
    end
end

local function all_stats_done()
    for id, _ in pairs(request_keys) do
        if not stats_done[id] then return false end
    end
    return true
end

local function rank_mmr(stats, playlist_id)
    for key, rank in pairs((stats and stats.ranks) or {}) do
        if tonumber(rank.playlist_id or key) == playlist_id then
            return tonumber(rank.mmr)
        end
    end
    return nil
end

local function current_mode_difference(their_stats, local_stats)
    if not current_playlist then return nil end
    local mmr_playlist
    if current_playlist == PRIVATE_PLAYLIST then
        local team_counts = { [0] = 0, [1] = 0 }
        for _, player in ipairs(players) do
            if player.team == 0 or player.team == 1 then
                team_counts[player.team] = team_counts[player.team] + 1
            end
        end
        mmr_playlist = TEAM_SIZE_PLAYLIST[math.max(team_counts[0], team_counts[1])]
    else
        mmr_playlist = RANKED_MMR_PLAYLISTS[current_playlist] and current_playlist or CASUAL_MMR
    end
    if not mmr_playlist then return nil end
    local their_mmr = rank_mmr(their_stats, mmr_playlist)
    local local_mmr = rank_mmr(local_stats, mmr_playlist)
    if their_mmr and local_mmr then return math.floor(their_mmr - local_mmr + 0.5) end
    return nil
end

local function best_mode_difference(their_stats, local_stats)
    local best = nil
    for key, rank in pairs((their_stats and their_stats.ranks) or {}) do
        local playlist_id = tonumber(rank.playlist_id or key)
        local their_mmr = tonumber(rank.mmr)
        local local_mmr = playlist_id and rank_mmr(local_stats, playlist_id) or nil
        if their_mmr and local_mmr then
            local difference = math.floor(their_mmr - local_mmr + 0.5)
            if not best or difference > best then best = difference end
        end
    end
    return best
end

local function request_needed_stats()
    if not hebnix.get_bool("heads_up_warn_higher_mmr", true) then return end
    request_stats(my_id)
    for _, player in pairs(recorded_ids) do
        if should_consider(player) then request_stats(player.id) end
    end
end

local function emit_warning()
    if toast_sent then return end
    toast_sent = true

    local warn_threshold = clamped_number("heads_up_warn_threshold", 10, 1, 50)
    local warn_mmr = hebnix.get_bool("heads_up_warn_higher_mmr", true)
    local mmr_threshold = clamped_number("heads_up_mmr_threshold", 20, 1, 500)
    local current_only = hebnix.get_bool("heads_up_mmr_current_only", true)
    local local_stats = stats_by_id[my_id]
    local candidates = {}
    for _, player in pairs(recorded_ids) do
        if should_consider(player) then table.insert(candidates, player) end
    end
    table.sort(candidates, function(a, b) return a.name < b.name end)

    local lines = {}
    for _, player in ipairs(candidates) do
        local count = encounters[player.id] and encounters[player.id].encounters or 0
        if count > warn_threshold then
            table.insert(lines, string.format("%s has been your opponent %d times", player.name, count))
        end

        if warn_mmr and local_stats then
            local difference
            if current_only then
                difference = current_mode_difference(stats_by_id[player.id], local_stats)
            else
                difference = best_mode_difference(stats_by_id[player.id], local_stats)
            end
            if difference and difference > mmr_threshold then
                table.insert(lines, string.format("%s has %d MMR more than you", player.name, difference))
            end
        end
    end

    if #lines > 0 then
        hebnix.toast(table.concat(lines, "\n"), { duration = 5, accent = "#f0a020" })
    end
end

function plugin.on_load()
    load_encounters()
    hebnix.log("Heads Up loaded")
end

function plugin.on_unload()
    if next(encounters) then save_encounters() end
    clear_match_state()
end

function plugin.on_game_event(event_type, event)
    local guid = event and (event.match_guid or event.MatchGuid)
    if guid and guid ~= "" and guid ~= match_guid then reset_match_state(guid) end

    if event_type == "MatchCreated" or event_type == "MatchInitialized" then
        if not in_match then reset_match_state(guid) end
    elseif event_type == "UpdateState" then
        if not in_match then reset_match_state(guid) end
        update_players(event)
    elseif event_type == "GameLeft" or event_type == "MatchDestroyed" then
        clear_match_state()
    end
end

function plugin.on_tick()
    if not in_match then return end
    refresh_match_info()
    if not current_playlist or not my_id or my_id == "" then return end

    record_new_encounters()
    if toast_sent then return end
    if not next(recorded_ids) then return end
    request_needed_stats()
    poll_stats()

    if now() - roster_changed_at < ROSTER_SETTLE_SECONDS then return end
    if not hebnix.get_bool("heads_up_warn_higher_mmr", true) then
        emit_warning()
        return
    end
    if all_stats_done() or (mmr_started_at and now() - mmr_started_at >= MMR_TIMEOUT_SECONDS) then
        emit_warning()
    end
end

function plugin.on_settings(ui)
    ui.heading("Encounter warnings")
    ui.checkbox("heads_up_warn_only_opponents", "Warn only opponents", true)
    ui.slider("heads_up_warn_threshold", "Warn threshold", 1, 50, 10)

    ui.separator()
    ui.heading("MMR warnings")
    ui.checkbox("heads_up_warn_higher_mmr", "Warn higher MMR players", true)
    ui.slider("heads_up_mmr_threshold", "MMR threshold", 1, 500, 20)
    ui.checkbox("heads_up_mmr_current_only", "MMR warn on current gamemode only", true)
end

return plugin
