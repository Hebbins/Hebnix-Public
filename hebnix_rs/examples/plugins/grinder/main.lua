local plugin = {}

local ENDPOINT = "https://req.hebnix.com/grinder"
local MODES = { "2v2", "3v3", "rumble", "hoops", "snowday", "dropshot", "heatseeker" }
local FILTER_MODES = { "All", "2v2", "3v3", "rumble", "hoops", "snowday", "dropshot", "heatseeker" }
local MODE_PLAYLIST = { ["2v2"] = 11, ["3v3"] = 13, rumble = 28, hoops = 27, dropshot = 29, snowday = 30 }
local REGIONS = { "NA", "EU", "OCE", "SAM", "ME", "ASIA", "AF", "IN" }
local FILTER_REGIONS = { "All", "NA", "EU", "OCE", "SAM", "ME", "ASIA", "AF", "IN" }
local PLAT_LABEL = { steam = "Steam", epic = "Epic", unknown = "PC" }
local DEMO = {
    { display_name = "Zwapo", platform = "steam", mode = "2v2", region = "eu", rank_tier = 16, rank_div = 2, rank_mmr = 1180, note = "mic, chill" },
    { display_name = "Nyx", platform = "epic", mode = "3v3", region = "na", rank_tier = 19, rank_div = 1, rank_mmr = 1420, note = "grinding to ssl" },
    { display_name = "kai.rl", platform = "steam", mode = "2v2", region = "eu", rank_tier = 13, rank_div = 3, rank_mmr = 1015, note = "" },
    { display_name = "Voltage", platform = "epic", mode = "hoops", region = "oce", rank_tier = 11, rank_div = 0, rank_mmr = 880, note = "just for fun" },
    { display_name = "mochi", platform = "steam", mode = "rumble", region = "na", rank_tier = 8, rank_div = 1, rank_mmr = 640, note = "casual" },
    { display_name = "Prime", platform = "epic", mode = "3v3", region = "eu", rank_tier = 22, rank_div = 0, rank_mmr = 1710, note = "lf duo" },
}

local S = {
    session_id = nil,
    username = nil,
    account_id = nil,
    platform = "unknown",
    profile_platform = nil,
    profile = nil,
    profile_key = nil,
    players = {},
    error = nil,
    updated_at = 0,
    last_post = 0,
    bind_was = false,
    capturing = false,
    sig = nil,
    rank_opts = nil,
}

local function mono() return hebnix.monotonic_seconds() or 0 end
local function trim(s) return (tostring(s or ""):gsub("^%s+", ""):gsub("%s+$", "")) end

local function gen_id()
    local chars = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
    math.randomseed(math.floor((hebnix.unix_millis() or 0)) + math.floor(mono() * 1000))
    local out = {}
    for i = 1, 24 do
        local n = math.random(1, #chars)
        out[i] = chars:sub(n, n)
    end
    return table.concat(out)
end

local function parse_identity(primary_id)
    local plat, acct = tostring(primary_id or ""):match("^([^|]+)|([^|]+)")
    if not plat or not acct or acct == "" then return end
    local p = plat:lower()
    local prof, disp
    if p == "epic" or p == "epicgames" then prof, disp = "epic", "epic"
    elseif p == "steam" then prof, disp = "steam", "steam"
    elseif p == "xbox" or p == "xboxone" or p == "xbl" then prof, disp = "xboxone", "xbox"
    elseif p == "ps4" or p == "ps5" or p == "psn" or p == "playstation" then prof, disp = "psn", "psn"
    elseif p == "switch" or p == "nintendo" then prof, disp = "switch", "switch"
    else prof, disp = p, "unknown" end
    S.account_id = acct
    S.profile_platform = prof
    S.platform = disp
end

local function rank_options()
    if S.rank_opts then return S.rank_opts end
    local opts = {}
    for t = 0, 22 do opts[t + 1] = (hebnix.tier_name(t) or ("Tier " .. t)) end
    S.rank_opts = opts
    return opts
end

local function tier_from_label(label)
    for i, name in ipairs(rank_options()) do
        if name == label then return i - 1 end
    end
    return nil
end

local function estimate_rank(mmr)
    if not mmr or mmr <= 0 then return 0, 0 end
    local step = 175.0
    local left = mmr - step
    local tier = 1
    while left >= 0 and tier < 22 do
        step = 60.0
        tier = tier + 1
        if tier >= 12 then step = 80.0 end
        if tier >= 15 then step = 120.0 end
        if tier >= 18 then step = 140.0 end
        if tier >= 20 then step = 160.0 end
        left = left - step
    end
    if tier == 22 then return 22, 0 end
    left = left + step
    step = step + 15
    local div = math.floor(left * (4 / step))
    return tier, math.max(0, math.min(3, div))
end

local function rank_for_mode(stats, mode)
    local pid = MODE_PLAYLIST[mode]
    if not pid then return { casual = true } end
    local ranks = type(stats) == "table" and stats.ranks or nil
    if type(ranks) ~= "table" then return nil end
    for key, r in pairs(ranks) do
        if tonumber(r.playlist_id or key) == pid then
            local tier = math.floor(tonumber(r.tier_id) or 0)
            local div = math.floor(tonumber(r.division_id) or 0)
            local mmr = math.floor(tonumber(r.mmr) or 0)
            if tier == 0 and mmr > 0 then tier, div = estimate_rank(mmr) end
            return { tier = tier, div = math.max(0, math.min(3, div)), mmr = mmr }
        end
    end
    return nil
end

local function ensure_profile()
    if S.profile or S.profile_key then return end
    if not S.account_id or not S.profile_platform then return end
    S.profile_key = hebnix.fetch_profile_async(S.profile_platform, S.account_id)
end

local function resolved_name()
    if S.canonical_name and S.canonical_name ~= "" then return S.canonical_name end
    if S.profile and type(S.profile.display_name) == "string" and S.profile.display_name ~= "" then
        return S.profile.display_name
    end
    if S.username and S.username ~= "" then return S.username end
    return nil
end

local function build_filter()
    local opts = rank_options()
    local f = {}
    local fm = hebnix.get_string("grinder_filter_mode", "All")
    if fm ~= "All" and fm ~= "" then f.mode = fm end
    local fr = hebnix.get_string("grinder_filter_region", "All")
    if fr ~= "All" and fr ~= "" then f.region = string.lower(fr) end
    local mn = tier_from_label(hebnix.get_string("grinder_min_rank", opts[1]))
    local mx = tier_from_label(hebnix.get_string("grinder_max_rank", opts[23]))
    if mn and mn > 0 then f.min_tier = mn end
    if mx and mx < 22 then f.max_tier = mx end
    return f
end

local function do_post()
    if not S.session_id then return end
    S.last_post = mono()
    ensure_profile()
    local name = resolved_name()
    local available = hebnix.get_bool("grinder_available", false) and name ~= nil
    local body = { id = S.session_id, available = available, filter = build_filter() }
    if available then
        local mode = hebnix.get_string("grinder_avail_mode", "2v2")
        local rk = S.profile and rank_for_mode(S.profile, mode) or nil
        body.display_name = name
        body.platform = S.platform
        body.mode = mode
        body.rank_tier = (rk and rk.tier) or 0
        body.rank_div = (rk and rk.div) or 0
        body.rank_mmr = rk and rk.mmr or nil
        body.region = string.lower(hebnix.get_string("grinder_region", "eu"))
        local note = trim(hebnix.get_string("grinder_note", ""))
        if note ~= "" then body.note = note end
    end
    hebnix.http_request_async("grinder", "POST", ENDPOINT, hebnix.json_encode(body),
        { ["Content-Type"] = "application/json" })
end

local function toggle_window()
    if hebnix.window.is_open() then
        hebnix.window.close()
    else
        hebnix.window.open({ title = "Grinder", width = 500, height = 720, opacity = 0.97 })
        ensure_profile()
        S.last_post = 0
    end
end

local function norm_platform(p)
    p = tostring(p or ""):lower()
    if p == "epicgames" or p == "epic" then return "epic" end
    if p == "steam" then return "steam" end
    if p == "xbl" or p == "xbox" or p == "xboxone" then return "xbox" end
    if p == "ps4" or p == "ps5" or p == "psn" or p == "playstation" then return "psn" end
    if p == "switch" or p == "nintendo" then return "switch" end
    return nil
end

local function try_identity()
    S.log_try = mono()
    local ok, info = pcall(hebnix.parse_launch_log, false)
    if not ok or type(info) ~= "table" or type(info.session) ~= "table" then return end
    local sess = info.session
    if sess.username then S.username = sess.username end
    if sess.primary_id then parse_identity(sess.primary_id) end
    local plat = norm_platform(sess.platform)
    if plat then S.platform = plat end
    if sess.epic_id and sess.epic_id ~= "" then S.epic_id = sess.epic_id end
end

function plugin.on_load()
    S.session_id = hebnix.get_string("grinder_session_id", "")
    if S.session_id == "" then
        S.session_id = gen_id()
        hebnix.set("grinder_session_id", S.session_id)
    end
    local ro = rank_options()
    if hebnix.get_string("grinder_min_rank", "") == "" then hebnix.set("grinder_min_rank", ro[1]) end
    if hebnix.get_string("grinder_max_rank", "") == "" then hebnix.set("grinder_max_rank", ro[23]) end
    try_identity()
end

function plugin.on_unload()
    if S.session_id then
        hebnix.http_request_async("grinder", "POST", ENDPOINT,
            hebnix.json_encode({ id = S.session_id, available = false }),
            { ["Content-Type"] = "application/json" })
    end
end

function plugin.on_http_result(id, status, body)
    if id ~= "grinder" then return end
    if status < 200 or status >= 300 then
        S.error = "server " .. tostring(status)
        return
    end
    local ok, j = pcall(hebnix.json_decode, body)
    if ok and type(j) == "table" and type(j.players) == "table" then
        S.players = j.players
        S.error = nil
        S.updated_at = mono()
    else
        S.error = "bad response"
    end
end

function plugin.on_tick()
    if not S.username and mono() - (S.log_try or -999) >= 4 then try_identity() end
    if S.epic_id and not S.canonical_name and not S.name_key then
        S.name_key = hebnix.fetch_profile_async("epic", S.epic_id)
    end
    if S.name_key and not S.canonical_name then
        local st = hebnix.stats_result(S.name_key)
        if type(st) == "table" then
            if not st.error and type(st.display_name) == "string" and st.display_name ~= "" then
                S.canonical_name = st.display_name
            end
            S.name_key = nil
        end
    end

    local bind = hebnix.get_string("grinder_bind", "f3")
    if not S.capturing then
        local pressed = bind ~= "" and hebnix.is_bind_pressed(bind)
        if pressed and not S.bind_was then toggle_window() end
        S.bind_was = pressed
    else
        local status, b = hebnix.capture_bind_result()
        if status == "done" then
            hebnix.set("grinder_bind", b or "f3")
            S.capturing = false
        elseif status == "timeout" then
            S.capturing = false
        end
    end

    if S.profile_key and not S.profile then
        local st = hebnix.stats_result(S.profile_key)
        if type(st) == "table" then
            if not st.error then S.profile = st end
            S.profile_key = nil
        end
    end

    local available = hebnix.get_bool("grinder_available", false)
    local open = hebnix.window.is_open()
    local interval = open and 15 or (available and 90 or nil)
    if interval and (S.last_post == 0 or mono() - S.last_post >= interval) then
        do_post()
    end
end

local function my_rank()
    if not S.profile then return nil, nil, "detecting rank (needs Rocket League running)...", nil end
    local mode = hebnix.get_string("grinder_avail_mode", "2v2")
    local rk = rank_for_mode(S.profile, mode)
    if not rk then return nil, nil, "no ranked data for this mode", nil end
    if rk.casual then return nil, nil, "casual (no ranked playlist)", nil end
    if rk.tier == 0 then return 0, 0, "unranked", rk.mmr end
    return rk.tier, rk.div, nil, rk.mmr
end

local function tier_badge(ui, tier)
    local t = math.max(0, math.min(22, math.floor(tonumber(tier) or 0)))
    ui.image("tiers/" .. t .. ".png", { width = 42, height = 28 })
end

local function div_stack(ui, tier, div)
    if tier < 1 or tier > 21 then return end
    local colour = math.floor((tier - 1) / 3) + 1
    local d = math.max(0, math.min(3, math.floor(tonumber(div) or 0)))
    for slot = 0, 3 do
        local img = (slot <= d) and colour or 0
        ui.image("divisions/" .. img .. ".png", { width = 24, height = 6, x = 5, y = (3 - slot) * 6 + 2 })
    end
    ui.image("divisions/0.png", { width = 32, height = 28, tint = "#00000000" })
end

function plugin.on_window(ui)
    ui.heading("Find teammates to grind with.")

    ui.separator()
    ui.checkbox("grinder_available", "List me as available")
    local pubname = resolved_name()
    if pubname then
        ui.colored_label("#7cc242", "Others add you as:  " .. pubname)
    else
        ui.colored_label("#e0a030", "Detecting your Rocket League name (needs the game running)...")
    end
    ui.space(2)
    ui.combo_box("grinder_avail_mode", "Mode", MODES)
    ui.combo_box("grinder_region", "Region", REGIONS)
    ui.horizontal(function()
        local mt, md, mstatus, mmmr = my_rank()
        if mstatus then
            ui.colored_label("#8a97a6", mstatus)
        else
            tier_badge(ui, mt)
            div_stack(ui, mt, md)
        end
        if mmmr and mmmr > 0 then ui.colored_label("#8a97a6", mmmr .. " mmr") end
    end)
    ui.text_input("grinder_note", "note (optional: mic, chill, casual...)")

    ui.separator()
    ui.heading("Available players")
    local opts = rank_options()
    ui.combo_box("grinder_filter_mode", "Show mode", FILTER_MODES)
    ui.combo_box("grinder_filter_region", "Region", FILTER_REGIONS)
    ui.combo_box("grinder_min_rank", "Min rank", opts)
    ui.combo_box("grinder_max_rank", "Max rank", opts)
    ui.checkbox("grinder_demo", "Show demo players (preview only)")

    local sig = table.concat({
        tostring(hebnix.get_bool("grinder_available", false)),
        hebnix.get_string("grinder_avail_mode", "2v2"),
        hebnix.get_string("grinder_region", "eu"),
        hebnix.get_string("grinder_note", ""),
        hebnix.get_string("grinder_filter_mode", "All"),
        hebnix.get_string("grinder_filter_region", "All"),
        hebnix.get_string("grinder_min_rank", opts[1]),
        hebnix.get_string("grinder_max_rank", opts[23]),
    }, "|")
    if S.sig ~= nil and S.sig ~= sig then
        S.last_post = 0
        ensure_profile()
    end
    S.sig = sig

    if S.error then
        ui.space(4)
        ui.colored_label("#e33d2e", "Cannot reach the finder: " .. S.error)
    end
    local myname_l = nil
    do
        local mn = resolved_name()
        myname_l = mn and mn:lower() or nil
    end
    local list = {}
    if hebnix.get_bool("grinder_demo", false) then
        for _, p in ipairs(DEMO) do list[#list + 1] = p end
    end
    for _, p in ipairs(S.players) do
        if not (myname_l and tostring(p.display_name or ""):lower() == myname_l) then
            list[#list + 1] = p
        end
    end
    local total = #list
    if total == 0 then
        ui.space(6)
        ui.label(S.error and "" or "No one available yet. List yourself and share the plugin with friends.")
    else
        ui.space(2)
        ui.colored_label("#8a97a6", total .. " found")
        for _, p in ipairs(list) do
            ui.separator()
            local tier = math.floor(tonumber(p.rank_tier) or 0)
            local name = tostring(p.display_name or "?")
            ui.horizontal(function()
                tier_badge(ui, tier)
                div_stack(ui, tier, p.rank_div)
                ui.colored_label("#eaeef2", name)
                if ui.button("Copy name") then ui.copy_to_clipboard(name) end
            end)
            local bits = { tostring(p.mode or "?"), string.upper(tostring(p.region or "?")),
                (PLAT_LABEL[p.platform] or tostring(p.platform or "?")) }
            local mmr = tonumber(p.rank_mmr)
            if mmr and mmr > 0 then bits[#bits + 1] = math.floor(mmr) .. " mmr" end
            ui.colored_label("#8a97a6", "    " .. table.concat(bits, "    |    "))
            if p.note and p.note ~= "" then
                ui.colored_label("#c9b072", "    note:  " .. tostring(p.note))
            end
        end
        ui.separator()
    end
end

function plugin.on_settings(ui)
    local bind = hebnix.get_string("grinder_bind", "f3")
    ui.horizontal(function()
        ui.label("Bind plugin window: " .. (bind ~= "" and bind or "(none)"))
        if S.capturing then
            ui.colored_label("#d35400", "Press any key/button...")
        else
            if ui.button("Set") then
                S.capturing = hebnix.capture_bind_async(10) and true or false
            end
            if ui.button("Reset to F3") then hebnix.set("grinder_bind", "f3") end
        end
    end)
    ui.label("Press the bind in-game to open or close the Grinder window.")
end

return plugin
