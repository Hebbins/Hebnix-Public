local plugin = {}

local CLIENT_ID = "65b708073fc0480ea92a077233ca87bd"
local CLIENT_VERSION = "1.2.78.418.gaeeb5ebd"
local TOKEN_URL = "https://accounts.spotify.com/api/token"
local CT_URL = "https://clienttoken.spotify.com/v1/clienttoken"
local DEALER = "wss://dealer.spotify.com/?access_token="
local SPCLIENT = "gew4-spclient.spotify.com"
local EXTMETA_URL = "https://gew4-spclient.spotify.com/extended-metadata/v0/extended-metadata"
local TRACK_V4 = 10
local TRACK_TYPE = "type.googleapis.com/spotify.metadata.Track"
local POLL = 5.0
local PING = 20.0

local DEFAULT_PALETTE = {
    dominant = "#7a26c8", vibrant = "#ff9de6", light = "#e6c8ff",
    average = "#7a26c8", dark = "#3d1466", text = "#ffffff", accent = "#ff9de6",
    base = "#7a26c8", grad1 = "#8f3bd6", grad2 = "#3d1466",
}

local S = {}
local function reset_state()
    S = {
        phase = "init",
        err = nil,
        pem = nil, refresh_token = nil, username = nil,
        access = nil, access_exp = 0,
        client_token = nil,
        device_id = nil,
        conn_id = nil,
        nonce = nil,
        refresh_pending = false,
        cluster_pending = false,
        cover_pending = false,
        last_poll = 0, last_ping = 0,
        last_proc_check = -999, spotify_running = true,
        last_uri = nil,
        cover_file = nil,
        palette = DEFAULT_PALETTE,
        meta_cache = {},
        now = nil,
    }
end

local function mono() return hebnix.monotonic_seconds() end

local function b64url(bytes)
    if not bytes then return nil end
    local s = hebnix.base64_encode(bytes)
    s = s:gsub("%+", "-")
    s = s:gsub("/", "_")
    s = s:gsub("=", "")
    return s
end

local function rand_bytes(n)
    local t = {}
    for i = 1, n do t[i] = string.char(math.random(0, 255)) end
    return table.concat(t)
end

local function hex(n)
    local t = {}
    for _ = 1, n do t[#t + 1] = string.format("%02x", math.random(0, 255)) end
    return table.concat(t)
end

local function is_text(s)
    if #s == 0 then return false end
    return s:match("^[\32-\126\9\10\13]*$") ~= nil
end

local function urlencode(s)
    return (s:gsub("[^%w%-%_%.%~]", function(c)
        return string.format("%%%02X", string.byte(c))
    end))
end

local function read_varint(s, i)
    local result, shift = 0, 0
    while true do
        local b = string.byte(s, i)
        if not b then return nil, i end
        i = i + 1
        result = result | ((b & 0x7f) << shift)
        if b < 128 then return result, i end
        shift = shift + 7
    end
end

local function pb_fields(s)
    local out, i, n = {}, 1, #s
    while i <= n do
        local tag; tag, i = read_varint(s, i)
        if not tag then break end
        local field = tag >> 3
        local wt = tag & 7
        local val
        if wt == 0 then
            val, i = read_varint(s, i)
        elseif wt == 2 then
            local len; len, i = read_varint(s, i)
            if not len then break end
            val = s:sub(i, i + len - 1); i = i + len
        elseif wt == 5 then
            val = s:sub(i, i + 3); i = i + 4
        elseif wt == 1 then
            val = s:sub(i, i + 7); i = i + 8
        else
            break
        end
        out[#out + 1] = { field = field, wt = wt, val = val }
    end
    return out
end

local function pb_full(s)
    local ok, res = pcall(function()
        local i, n = 1, #s
        while i <= n do
            local tag; tag, i = read_varint(s, i)
            if not tag then return false end
            local wt = tag & 7
            if wt == 0 then local _; _, i = read_varint(s, i)
            elseif wt == 2 then local len; len, i = read_varint(s, i); i = i + len
            elseif wt == 5 then i = i + 4
            elseif wt == 1 then i = i + 8
            else return false end
            if i > n + 1 then return false end
        end
        return i == n + 1
    end)
    return ok and res
end

local function walk_leaves(s, depth, acc)
    depth = depth or 0
    acc = acc or {}
    for _, f in ipairs(pb_fields(s)) do
        if f.wt == 2 then
            acc[#acc + 1] = f.val
            if depth < 8 and #f.val > 2 and pb_full(f.val) then
                walk_leaves(f.val, depth + 1, acc)
            end
        end
    end
    return acc
end

local function enc_varint(n)
    local t = {}
    repeat
        local b = n & 0x7f
        n = n >> 7
        if n ~= 0 then b = b | 0x80 end
        t[#t + 1] = string.char(b)
    until n == 0
    return table.concat(t)
end
local function pb_tag(f, w) return enc_varint((f << 3) | w) end
local function pf_bytes(f, s) return pb_tag(f, 2) .. enc_varint(#s) .. s end
local function pf_var(f, n) return pb_tag(f, 0) .. enc_varint(n) end

local function parse_dbrts(bytes)
    local pem, rt, user
    for _, leaf in ipairs(walk_leaves(bytes)) do
        if not pem and leaf:sub(1, 10) == "-----BEGIN" then
            pem = leaf
        elseif is_text(leaf) then
            if not user and #leaf == 25 and leaf:match("^[a-z0-9]+$") then
                user = leaf
            elseif not rt and #leaf >= 80 and (leaf:sub(1, 2) == "AQ" or leaf:sub(1, 2) == "AA") then
                rt = leaf
            end
        end
    end
    return pem, rt, user
end

local function dpop_proof(htm, htu, nonce)
    local pub = hebnix.p256_public(S.pem)
    if not pub or #pub < 65 then return nil end
    local jwk = { kty = "EC", crv = "P-256", x = b64url(pub:sub(2, 33)), y = b64url(pub:sub(34, 65)) }
    local header = hebnix.json_encode({ typ = "dpop+jwt", alg = "ES256", jwk = jwk })
    local payload = { htu = htu, htm = htm, jti = b64url(rand_bytes(16)), iat = os.time() }
    if nonce then payload.nonce = nonce end
    local si = b64url(header) .. "." .. b64url(hebnix.json_encode(payload))
    local sig = hebnix.p256_sign(S.pem, si)
    if not sig then return nil end
    return si .. "." .. b64url(sig)
end

local function build_ct_request()
    local platform = pf_bytes(5, "")
    local conn = pf_bytes(1, platform) .. pf_bytes(2, S.device_id)
    local cdata = pf_bytes(1, CLIENT_VERSION) .. pf_bytes(2, CLIENT_ID) .. pf_bytes(3, conn)
    return pf_var(1, 1) .. pf_bytes(2, cdata)
end

local function parse_ct_response(bytes)
    for _, f in ipairs(pb_fields(bytes)) do
        if f.field == 2 and f.wt == 2 then
            for _, g in ipairs(pb_fields(f.val)) do
                if g.field == 1 and g.wt == 2 then return g.val end
            end
        end
    end
    return nil
end

local function build_extmeta(uri)
    local query = pf_bytes(2, pf_var(1, TRACK_V4))
    local ent = pf_bytes(1, uri) .. query
    local header = pf_bytes(1, "US") .. pf_bytes(2, "premium")
    return pf_bytes(1, header) .. pf_bytes(2, ent)
end

local function find_track_bytes(bytes, depth)
    depth = depth or 0
    if depth > 12 then return nil end
    local fields = pb_fields(bytes)
    for _, f in ipairs(fields) do
        if f.field == 1 and f.wt == 2 and f.val == TRACK_TYPE then
            for _, g in ipairs(fields) do
                if g.field == 2 and g.wt == 2 then return g.val end
            end
        end
    end
    for _, f in ipairs(fields) do
        if f.wt == 2 and #f.val > 1 and pb_full(f.val) then
            local got = find_track_bytes(f.val, depth + 1)
            if got then return got end
        end
    end
    return nil
end

local function parse_track(tb)
    local name, album, artists = nil, nil, {}
    for _, f in ipairs(pb_fields(tb)) do
        if f.field == 2 and f.wt == 2 and not name then
            name = f.val
        elseif f.field == 4 and f.wt == 2 then
            for _, g in ipairs(pb_fields(f.val)) do
                if g.field == 2 and g.wt == 2 then artists[#artists + 1] = g.val; break end
            end
        elseif f.field == 3 and f.wt == 2 and not album then
            for _, g in ipairs(pb_fields(f.val)) do
                if g.field == 2 and g.wt == 2 then album = g.val; break end
            end
        end
    end
    return name, artists, album
end

local function send_refresh()
    if S.refresh_pending then return end
    local proof = dpop_proof("POST", TOKEN_URL, S.nonce)
    if not proof then S.phase = "error"; S.err = "cannot sign DPoP proof"; return end
    S.refresh_pending = true
    local body = "grant_type=refresh_token&refresh_token=" .. urlencode(S.refresh_token)
        .. "&client_id=" .. CLIENT_ID
    hebnix.http_request_async("refresh", "POST", TOKEN_URL, body, {
        ["Content-Type"] = "application/x-www-form-urlencoded",
        ["DPoP"] = proof,
        ["Accept"] = "application/json",
    })
end

local function send_client_token()
    hebnix.http_request_async("ct", "POST", CT_URL, build_ct_request(), {
        ["Content-Type"] = "application/x-protobuf",
        ["Accept"] = "application/x-protobuf",
    })
end

local function poll_cluster()
    if S.cluster_pending or not S.conn_id or not S.access then return end
    S.cluster_pending = true
    local url = "https://" .. SPCLIENT .. "/connect-state/v1/devices/" .. S.device_id
    local body = hebnix.json_encode({
        member_type = "CONNECT_STATE",
        device = { device_info = { capabilities = {
            can_be_player = false, hidden = true, needs_full_player_state = true } } },
    })
    hebnix.http_request_async("cluster", "PUT", url, body, {
        ["Authorization"] = "Bearer " .. S.access,
        ["X-Spotify-Connection-Id"] = S.conn_id,
        ["client-token"] = S.client_token,
        ["Content-Type"] = "application/json",
    })
end

local function img_url(uri)
    if uri and uri:sub(1, 14) == "spotify:image:" then
        return "https://i.scdn.co/image/" .. uri:sub(15)
    end
    return nil
end

local function apply_cluster(cl)
    local ps = cl.player_state or {}
    local tr = ps.track or {}
    local md = tr.metadata or {}
    local uri = tr.uri

    if uri ~= S.last_uri then
        S.last_uri = uri
        hebnix.clear_asset_dir("temp")
        S.cover_file = nil
        local cover = img_url(md.image_xlarge_url or md.image_large_url or md.image_url)
        if cover then
            S.cover_pending = true
            hebnix.http_download_async("cover", cover, nil)
        end
        if uri and uri:sub(1, 14) == "spotify:track:" and not S.meta_cache[uri] then
            hebnix.http_request_async("track", "POST", EXTMETA_URL, build_extmeta(uri), {
                ["Authorization"] = "Bearer " .. S.access,
                ["client-token"] = S.client_token,
                ["Accept"] = "application/protobuf",
                ["Content-Type"] = "application/protobuf",
            })
        end
    end

    if not uri then
        S.now = nil
        return
    end

    local server_ts = tonumber(ps.timestamp) or 0
    local pos_at_ts = tonumber(ps.position_as_of_timestamp) or 0
    local playing = not ps.is_paused
    local duration = tonumber(ps.duration) or tonumber(md.duration) or 0

    if S.now and S.now.track_uri == uri and S.now.server_ts == server_ts then
        S.now.is_playing = playing
        S.now.duration_ms = duration
        S.now.title = md.title
        S.now.album = md.album_title
    else
        local base = pos_at_ts
        if playing and server_ts > 0 then
            local drift = os.time() * 1000 - server_ts
            if drift >= 0 and drift < 3600000 then base = pos_at_ts + drift end
        end
        S.now = {
            title = md.title,
            album = md.album_title,
            artist = (S.now and S.now.track_uri == uri and S.now.artist) or nil,
            is_playing = playing,
            position_ms = base,
            duration_ms = duration,
            at = mono(),
            server_ts = server_ts,
            track_uri = uri,
        }
    end

    local cached = S.meta_cache[uri]
    if cached then
        if cached.artist then S.now.artist = cached.artist end
        if cached.album and (not S.now.album or #S.now.album == 0) then
            S.now.album = cached.album
        end
    end
end

function plugin.on_http_result(id, status, body, headers)
    if id == "refresh" then
        S.refresh_pending = false
        local h = {}
        pcall(function() h = hebnix.json_decode(headers) or {} end)
        if h["dpop-nonce"] and h["dpop-nonce"] ~= S.nonce then
            S.nonce = h["dpop-nonce"]
        end
        if status == 200 then
            local ok, j = pcall(hebnix.json_decode, body)
            if ok and j and j.access_token then
                S.access = j.access_token
                S.access_exp = mono() + (tonumber(j.expires_in) or 3600)
                if j.refresh_token then S.refresh_token = j.refresh_token end
                if S.phase == "need_token" then
                    S.phase = "need_ct"
                    send_client_token()
                end
            else
                S.phase = "error"; S.err = "bad token response"
            end
        elseif (status == 400 or status == 401) and S.nonce then
            send_refresh()
        else
            S.phase = "error"; S.err = "token refresh failed (" .. tostring(status) .. ")"
        end
    elseif id == "ct" then
        if status == 200 and #body > 0 then
            local tok = parse_ct_response(body)
            if tok then
                S.client_token = tok
                S.phase = "connect"
            else
                S.phase = "error"; S.err = "no client-token in response"
            end
        else
            S.phase = "error"; S.err = "client-token failed (" .. tostring(status) .. ")"
        end
    elseif id == "cluster" then
        S.cluster_pending = false
        if status == 200 then
            local ok, cl = pcall(hebnix.json_decode, body)
            if ok and cl then apply_cluster(cl) end
        elseif status == 401 then
            S.access = nil
            S.conn_id = nil
            hebnix.ws_close("dealer")
            S.phase = "need_token"
            send_refresh()
        end
    elseif id == "track" then
        if status == 200 and #body > 0 then
            local tb = find_track_bytes(body)
            if tb then
                local _, artists, album = parse_track(tb)
                local artist = #artists > 0 and table.concat(artists, ", ") or nil
                if S.now then
                    if artist then S.now.artist = artist end
                    if album and (not S.now.album or #S.now.album == 0) then S.now.album = album end
                    if S.now.track_uri then
                        S.meta_cache[S.now.track_uri] = { artist = artist, album = album }
                    end
                end
            end
        end
    end
end

function plugin.on_http_download_response(id, status, body)
    if id == "cover" then
        S.cover_pending = false
        if status == 200 and body and #body > 0 then
            local fname = "cover_" .. hex(6) .. ".jpg"
            if hebnix.write_asset("temp/" .. fname, body) then
                S.cover_file = fname
                local pal = hebnix.image_palette("temp/" .. fname)
                if pal then S.palette = pal end
            end
        end
    end
end

function plugin.on_ws_open(id)
    if id == "dealer" then S.last_ping = mono() end
end

function plugin.on_ws_message(id, data)
    if id ~= "dealer" then return end
    local ok, msg = pcall(hebnix.json_decode, data)
    if not ok or type(msg) ~= "table" then return end
    local h = msg.headers
    if type(h) == "table" then
        local cid = h["Spotify-Connection-Id"] or h["spotify-connection-id"]
        if cid and not S.conn_id then
            S.conn_id = cid
            S.phase = "poll"
            S.last_poll = 0
            return
        end
    end
    if S.phase == "poll" and type(msg.uri) == "string"
        and msg.uri:find("connect-state", 1, true) then
        S.last_poll = 0
    end
end

function plugin.on_ws_close(id, reason)
    if id == "dealer" then
        S.conn_id = nil
        if S.phase == "poll" or S.phase == "connect" or S.phase == "connect_wait" then
            S.phase = "connect"
        end
    end
end

local function detect_client_version()
    local appdata = os.getenv("APPDATA")
    if not appdata then return nil end
    local prefs = hebnix.read_file(appdata .. "\\Spotify\\prefs")
    if not prefs then return nil end
    return prefs:match('app%.last%-launched%-version="([^"]+)"')
end

local function load_device_id()
    local dir = hebnix.plugin_dir and hebnix.plugin_dir()
    if not dir then return hex(20) end
    local path = dir .. "\\device_id"
    local f = io.open(path, "rb")
    if f then
        local id = (f:read("*a") or ""):gsub("%s+$", "")
        f:close()
        if #id >= 20 then return id end
    end
    local id = hex(20)
    local w = io.open(path, "wb")
    if w then w:write(id); w:close() end
    return id
end

function plugin.on_load()
    reset_state()
    hebnix.write_asset("temp/.keep", "")
    hebnix.clear_asset_dir("temp")
    math.randomseed(os.time() + math.floor(mono() * 1000))
    S.device_id = load_device_id()
    CLIENT_VERSION = detect_client_version() or CLIENT_VERSION
    if not hebnix.get_bool("spotify_overlay_disclaimer_ack", false) then
        hebnix.window.open({
            title = "Spotify Overlay",
            width = 400,
            height = 250,
            close_button = false,
            opacity = 1.0,
        })
    end
end

function plugin.on_window(ui)
    ui.heading("Heads up")
    ui.space(4)
    ui.label("This plugin signs in by reading your local Spotify session and")
    ui.label("talking to Spotify the way the desktop app does. It only ever")
    ui.label("touches your own account and sends nothing anywhere else.")
    ui.space(6)
    ui.label("It is unofficial, so use it at your own risk. In rare cases")
    ui.label("Spotify could flag or action an account for non-official access.")
    ui.space(10)
    if ui.button("OK") then
        hebnix.set("spotify_overlay_disclaimer_ack", true)
        hebnix.window.close()
    end
end

function plugin.on_unload()
    if S.device_id then hebnix.ws_close("dealer") end
    hebnix.clear_asset_dir("temp")
    reset_state()
end

function plugin.on_tick()
    local nowt = mono()
    if nowt - S.last_proc_check >= 2.0 then
        S.last_proc_check = nowt
        S.spotify_running = hebnix.process_running("Spotify.exe")
    end

    if S.phase == "init" then
        local path = (os.getenv("LOCALAPPDATA") or "") .. "\\Spotify\\dbrts"
        local bytes = hebnix.read_file(path)
        if not bytes then
            S.phase = "error"
            S.err = "can't read Spotify login (is Spotify installed & logged in?)"
            return
        end
        local pem, rt, user = parse_dbrts(bytes)
        if not pem or not rt then
            S.phase = "error"; S.err = "couldn't parse Spotify login"
            return
        end
        S.pem, S.refresh_token, S.username = pem, rt, user
        S.phase = "need_token"
        send_refresh()
    elseif S.phase == "connect" then
        local t = mono()
        if t - (S.last_connect or 0) >= 3.0 then
            S.last_connect = t
            S.conn_id = nil
            hebnix.ws_connect_async("dealer", DEALER .. (S.access or ""))
            S.phase = "connect_wait"
        end
    elseif S.phase == "poll" then
        local t = mono()
        if S.access and mono() > S.access_exp - 120 and not S.refresh_pending then
            send_refresh()
        end
        if t - S.last_poll >= POLL then
            S.last_poll = t
            poll_cluster()
        end
        if t - S.last_ping >= PING then
            S.last_ping = t
            hebnix.ws_send("dealer", '{"type":"ping"}')
        end
    end
end

local CORNERS = { "Bottom Left", "Bottom Right", "Top Left", "Top Right" }

local function position_ms()
    if not S.now then return 0 end
    local pos = S.now.position_ms or 0
    if S.now.is_playing then pos = pos + math.floor((mono() - S.now.at) * 1000) end
    local dur = S.now.duration_ms or 0
    if dur > 0 and pos > dur then pos = dur end
    return pos
end

local function fmt_time(ms)
    local s = math.max(0, math.floor((ms or 0) / 1000))
    return string.format("%d:%02d", math.floor(s / 60), s % 60)
end

local function scroll_text(draw, s, x, y, size, color, cl, cw)
    s = s or ""
    local tw = hebnix.measure_text(s, size, true)
    if tw <= cw then
        draw.text(x, y, s, { color = color, size = size, bold = true })
        return
    end
    local gap = 40 * (size / 15)
    local period = tw + gap
    local off = (mono() * 30) % period
    draw.text(x - off, y, s .. string.rep(" ", 6) .. s,
        { color = color, size = size, bold = true, clip_x = cl, clip_w = cw })
end

function plugin.on_overlay(draw, w, h)
    if not hebnix.get_bool("spotify_overlay_enabled", true) then return end
    if hebnix.get_bool("spotify_overlay_only_when_open", true) and not S.spotify_running then return end
    if not S.now then return end

    local s = hebnix.get_number("spotify_overlay_scale", 100) / 100
    local p = S.palette or DEFAULT_PALETTE

    local art = 130 * s
    local gap = 16 * s
    local panel_w = 360 * s
    local panel_h = art
    local prog_h = 5 * s
    local prog_gap = 6 * s
    local card_w = art + gap + panel_w
    local card_h = panel_h + prog_gap + prog_h
    local margin = 24 * s

    local corner = hebnix.get_string("spotify_overlay_corner", "Bottom Left")
    local x, y
    if corner == "Bottom Right" then
        x, y = w - card_w - margin, h - card_h - margin
    elseif corner == "Top Left" then
        x, y = margin, margin
    elseif corner == "Top Right" then
        x, y = w - card_w - margin, margin
    else
        x, y = margin, h - card_h - margin
    end

    if hebnix.get_bool("spotify_overlay_show_cover", true) and S.cover_file then
        draw.image("assets/temp/" .. S.cover_file, x, y, art, art, { opacity = 1.0, radius = 10 * s })
    else
        draw.rect(x, y, art, art, { color = p.dark, filled = true, radius = 10 * s })
        draw.text(x + art / 2, y + art / 2 - 14 * s, "\u{266A}",
            { color = p.vibrant, size = 34 * s, halign = "center" })
    end

    local px = x + art + gap
    draw.gradient(px, y, panel_w, panel_h,
        { color = p.grad1, color2 = p.grad2, radius = 12, angle = 135 })

    local pad_x = 18 * s
    local cl = px + pad_x
    local cr = px + panel_w - pad_x
    local cw = cr - cl

    scroll_text(draw, S.now.title or "Unknown", cl, y + 18 * s, 21 * s, p.text, cl, cw)
    scroll_text(draw, S.now.artist or S.now.album or "", cl, y + 47 * s, 15 * s, p.text .. "cc", cl, cw)

    local dur = S.now.duration_ms or 0
    local pos = position_ms()
    local row_cy = y + panel_h - 26 * s
    local time_w = 46 * s
    draw.text(cl, row_cy - 8 * s, fmt_time(pos),
        { color = p.text, size = 13 * s, bold = true })
    draw.text(cr, row_cy - 8 * s, fmt_time(dur),
        { color = p.text, size = 13 * s, halign = "right", bold = true })

    local heights = { 6, 14, 10, 18, 8, 20, 12, 16, 6, 10, 14, 22, 12, 18,
        8, 14, 10, 16, 6, 12, 8, 18, 10, 14 }
    local vl = cl + time_w + 12 * s
    local vr = cr - time_w - 12 * s
    local n = #heights
    local bw = 3 * s
    local t = mono()
    for i = 1, n do
        local amp = S.now.is_playing and (0.55 + 0.45 * math.abs(math.sin(t * 5 + i * 0.7))) or 1.0
        local bh = heights[i] * amp * 1.2 * s
        local bx = vl + (vr - vl - bw) * (i - 1) / (n - 1)
        draw.rect(bx, row_cy - bh / 2, bw, bh, { color = p.accent or p.vibrant, filled = true, radius = 2 })
    end

    local by = y + panel_h + prog_gap
    draw.rect(x, by, card_w, prog_h, { color = "#00000073", filled = true, radius = 2 })
    if dur > 0 then
        local frac = math.max(0, math.min(1, pos / dur))
        draw.rect(x, by, card_w * frac, prog_h, { color = p.vibrant, filled = true, radius = 2 })
    end
end

function plugin.on_settings(ui)
    ui.label("Shows your current Spotify track as an overlay card.")
    if S.phase == "error" then
        ui.colored_label("#e74c3c", "error: " .. tostring(S.err))
    elseif S.now then
        ui.colored_label("#1db954", "playing: " .. (S.now.title or "?"))
    elseif S.phase == "poll" then
        ui.colored_label("#e0a030", "connected, nothing playing")
    else
        ui.colored_label("#e0a030", "connecting to Spotify\u{2026} (" .. S.phase .. ")")
    end
    ui.space(6)
    ui.checkbox("spotify_overlay_enabled", "Show overlay card", true)
    ui.checkbox("spotify_overlay_only_when_open", "Only while the Spotify app is open", true)
    ui.checkbox("spotify_overlay_show_cover", "Show cover art", true)
    ui.combo_box("spotify_overlay_corner", "Position", CORNERS)
    ui.slider("spotify_overlay_scale", "Scale", 60, 160, 100)
    ui.space(6)
    ui.label("Reads your local Spotify login only (see plugin.toml [permissions]).")
    ui.label("The card shows over Rocket League while the game is focused.")
    ui.label("Version 1.0.0")
end

return plugin
