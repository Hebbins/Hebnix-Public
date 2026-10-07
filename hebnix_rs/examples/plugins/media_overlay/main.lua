local plugin = {}

local POLL = 0.5
local LRC_GET = "https://lrclib.net/api/get"
local LRC_SEARCH = "https://lrclib.net/api/search"
local LRC_UA = "hebnix-media-overlay (https://github.com/Hebbins)"

local DEFAULT_PALETTE = {
    dominant = "#7a26c8", vibrant = "#ff9de6", light = "#e6c8ff",
    average = "#7a26c8", dark = "#3d1466", text = "#ffffff", accent = "#ff9de6",
    base = "#7a26c8", grad1 = "#8f3bd6", grad2 = "#3d1466",
}

-- source app ids windows hands out for browsers, where "Artist - Title" in the
-- title is the norm and the "artist" is really the channel
local BROWSERS = { "chrome", "msedge", "firefox", "opera", "brave", "vivaldi", "308046b0af4a39cb" }

-- bracketed bits of video titles that only get in the way of a lyrics lookup
local JUNK = { "official", "video", "audio", "lyric", "visuali", "music video", "mv", "hd", "4k", "hq", "explicit", "clean" }

local S = {}
local function reset_state()
    S = {
        last_poll = -999,
        key = nil,
        track_n = 0,
        thumb_rev = -1,
        cover_file = nil,
        palette = DEFAULT_PALETTE,
        lyrics = nil,
        lyric_i = -1,
        lyric_t0 = 0,
        app = nil,
        now = nil,
    }
end

local function mono() return hebnix.monotonic_seconds() end

local function hex(n)
    local t = {}
    for _ = 1, n do t[#t + 1] = string.format("%02x", math.random(0, 255)) end
    return table.concat(t)
end

local function urlencode(s)
    return (s:gsub("[^%w%-%_%.%~]", function(c)
        return string.format("%%%02X", string.byte(c))
    end))
end

local function trim(s) return (s:gsub("^%s+", ""):gsub("%s+$", "")) end

local function is_browser(app)
    app = (app or ""):lower()
    -- firefox-likes register under a hashed id rather than an exe name
    if app:match("^%x+$") and #app == 16 then return true end
    for _, b in ipairs(BROWSERS) do
        if app:find(b, 1, true) then return true end
    end
    return false
end

local function app_name(app)
    if not app or #app == 0 then return "unknown app" end
    return (app:gsub("%.exe$", ""))
end

local function clean_artist(a)
    a = a or ""
    a = a:gsub("%s*%-%s*Topic$", ""):gsub("VEVO$", "")
    return trim(a)
end

local function clean_title(t)
    t = t or ""
    local function strip(group)
        local l = group:lower()
        for _, j in ipairs(JUNK) do
            if l:find(j, 1, true) then return "" end
        end
        return group
    end
    t = t:gsub("%b()", strip):gsub("%b[]", strip)
    t = t:gsub("%s+[fF]eat%.?%s.*$", ""):gsub("%s+[fF]t%.?%s.*$", "")
    t = t:gsub("%s+|%s+.*$", "")
    return trim(t)
end

-- best guess at (artist, title) for lrclib. youtube titles are usually
-- "Artist - Title (Official Video)" with the channel as the artist
local function lyric_query(now)
    local title, artist = now.title or "", clean_artist(now.artist)
    local a, t = title:match("^(.-)%s+%-%s+(.+)$")
    if a and t and (#artist == 0 or is_browser(S.app)
        or a:lower() == artist:lower()) then
        artist, title = a, t
    end
    return clean_artist(artist), clean_title(title)
end

local function parse_lrc(s)
    local out = {}
    for line in (s .. "\n"):gmatch("(.-)\n") do
        local text = line:gsub("%[%d+:%d+%.?%d*%]", ""):gsub("^%s+", ""):gsub("%s+$", "")
        for mm, ss in line:gmatch("%[(%d+):(%d+%.?%d*)%]") do
            out[#out + 1] = { t = tonumber(mm) * 60 + tonumber(ss), text = text }
        end
    end
    table.sort(out, function(a, b) return a.t < b.t end)
    return out
end

local function search_lyrics(n)
    local artist, title = lyric_query(S.now)
    if #title == 0 then return end
    local q = "?track_name=" .. urlencode(title)
    if #artist > 0 then q = q .. "&artist_name=" .. urlencode(artist) end
    hebnix.http_request_async("lrcs:" .. n, "GET", LRC_SEARCH .. q, nil,
        { ["User-Agent"] = LRC_UA })
end

local function request_lyrics()
    if not hebnix.get_bool("media_overlay_lyrics", true) then return end
    if not S.now then return end
    local artist, title = lyric_query(S.now)
    if #title == 0 or #artist == 0 then return end
    local dur = math.floor((S.now.duration_ms or 0) / 1000)
    -- /get wants an exact duration, without one go straight to search
    if dur <= 0 then
        search_lyrics(S.track_n)
        return
    end
    local q = "?artist_name=" .. urlencode(artist)
        .. "&track_name=" .. urlencode(title)
        .. "&album_name=" .. urlencode(S.now.album or "")
        .. "&duration=" .. tostring(dur)
    hebnix.http_request_async("lrc:" .. S.track_n, "GET", LRC_GET .. q, nil,
        { ["User-Agent"] = LRC_UA })
end

-- image::open goes by extension, so name the cover after its magic bytes
local function image_ext(b)
    if b:sub(1, 4) == "\137PNG" then return "png" end
    if b:sub(1, 2) == "\255\216" then return "jpg" end
    if b:sub(1, 4) == "RIFF" and b:sub(9, 12) == "WEBP" then return "webp" end
    if b:sub(1, 2) == "BM" then return "bmp" end
    return nil
end

local function load_cover()
    hebnix.clear_asset_dir("temp")
    S.cover_file = nil
    S.palette = DEFAULT_PALETTE
    local bytes = hebnix.media_thumbnail()
    if not bytes or #bytes == 0 then return end
    local ext = image_ext(bytes)
    if not ext then return end
    local fname = "cover_" .. hex(6) .. "." .. ext
    if hebnix.write_asset("temp/" .. fname, bytes) then
        S.cover_file = fname
        local pal = hebnix.image_palette("temp/" .. fname)
        if pal then S.palette = pal end
    end
end

local function apply_media(m)
    local visible = m and (m.status == "playing" or m.status == "paused" or m.status == "changing")
    if not visible then
        if S.key then
            S.key = nil
            S.now = nil
            S.lyrics = nil
        end
        return
    end

    S.app = m.app
    local key = (m.app or "") .. "\1" .. (m.title or "") .. "\1" .. (m.artist or "")
    local new_track = key ~= S.key
    if new_track then
        S.key = key
        S.track_n = S.track_n + 1
        S.lyrics = nil
        S.lyric_i = -1
        S.now = nil
    end

    if m.thumb_rev ~= S.thumb_rev then
        S.thumb_rev = m.thumb_rev
        load_cover()
    end

    local artist = clean_artist(m.artist)
    if #artist == 0 then artist = clean_artist(m.album_artist) end
    if S.now and S.now.updated == m.updated_unix_ms and S.now.status == m.status then
        S.now.duration_ms = m.duration_ms
    else
        local base = m.position_ms or 0
        if m.is_playing and (m.updated_unix_ms or 0) > 0 then
            local drift = hebnix.unix_millis() - m.updated_unix_ms
            if drift >= 0 and drift < 3600000 then base = base + drift end
        end
        S.now = {
            title = m.title,
            artist = artist,
            album = m.album,
            is_playing = m.is_playing,
            status = m.status,
            position_ms = base,
            duration_ms = m.duration_ms or 0,
            at = mono(),
            updated = m.updated_unix_ms,
        }
    end

    if new_track then request_lyrics() end
end

function plugin.on_http_result(id, status, body, headers)
    if id:sub(1, 4) == "lrc:" then
        local n = tonumber(id:sub(5))
        if S.now and n == S.track_n then
            local synced
            if status == 200 and #body > 0 then
                local ok, j = pcall(hebnix.json_decode, body)
                if ok and type(j) == "table" then synced = j.syncedLyrics end
            end
            if type(synced) == "string" and #synced > 0 then
                S.lyrics = parse_lrc(synced)
            else
                search_lyrics(n)
            end
        end
    elseif id:sub(1, 5) == "lrcs:" then
        local n = tonumber(id:sub(6))
        if S.now and n == S.track_n and status == 200 and #body > 0 then
            local ok, j = pcall(hebnix.json_decode, body)
            if ok and type(j) == "table" then
                for _, item in ipairs(j) do
                    if type(item.syncedLyrics) == "string" and #item.syncedLyrics > 0 then
                        S.lyrics = parse_lrc(item.syncedLyrics)
                        break
                    end
                end
            end
        end
    end
end

function plugin.on_load()
    reset_state()
    hebnix.write_asset("temp/.keep", "")
    hebnix.clear_asset_dir("temp")
    math.randomseed(os.time() + math.floor(mono() * 1000))
end

function plugin.on_unload()
    hebnix.clear_asset_dir("temp")
    reset_state()
end

function plugin.on_tick()
    local t = mono()
    if t - S.last_poll < POLL then return end
    S.last_poll = t
    apply_media(hebnix.media_session())
end

local CORNERS = { "Bottom Left", "Bottom Right", "Top Left", "Top Right" }
local LYRIC_ANIMS = { "Slide", "Fade", "Scale", "None" }
local LYRIC_POS = { "Below card", "Above card" }

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

local function lyric_index(pos_s)
    if not S.lyrics or #S.lyrics == 0 then return 0 end
    local idx = 0
    for i, l in ipairs(S.lyrics) do
        if l.t <= pos_s then idx = i else break end
    end
    return idx
end

local function with_alpha(hex, a)
    a = math.max(0, math.min(255, math.floor(a * 255 + 0.5)))
    return hex:sub(1, 7) .. string.format("%02x", a)
end

local function opposite(hex)
    local r = tonumber(hex:sub(2, 3), 16) or 255
    local g = tonumber(hex:sub(4, 5), 16) or 255
    local b = tonumber(hex:sub(6, 7), 16) or 255
    return string.format("#%02x%02x%02x", 255 - r, 255 - g, 255 - b)
end

local BORDER_OFF = {
    { -1, -1 }, { 0, -1 }, { 1, -1 }, { -1, 0 },
    { 1, 0 }, { -1, 1 }, { 0, 1 }, { 1, 1 },
}

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
    if not hebnix.get_bool("media_overlay_enabled", true) then return end
    if not S.now then return end
    if hebnix.get_bool("media_overlay_hide_paused", false) and not S.now.is_playing then return end
    if hebnix.get_bool("media_overlay_ignore_spotify", false)
        and (S.app or ""):lower():find("spotify", 1, true) then return end

    local s = hebnix.get_number("media_overlay_scale", 100) / 100
    local p = S.palette or DEFAULT_PALETTE

    local art = 130 * s
    local gap = 16 * s
    local panel_w = 360 * s
    local panel_h = art
    local prog_h = 5 * s
    local prog_gap = 6 * s
    local lyrics_on = hebnix.get_bool("media_overlay_lyrics", true)
    local lyr_scale = hebnix.get_number("media_overlay_lyrics_scale", 100) / 100
    local lyr_size = 15 * s * lyr_scale
    local row_h = lyr_size * 1.45
    local block_h = lyrics_on and (row_h * 3) or 0
    local lyr_gap = lyrics_on and (10 * s) or 0
    local above = lyrics_on
        and hebnix.get_string("media_overlay_lyrics_pos", "Below card") == "Above card"
    local top_reserve = above and (block_h + lyr_gap) or 0
    local card_w = art + gap + panel_w
    local card_h = panel_h + prog_gap + prog_h + (lyrics_on and (block_h + lyr_gap) or 0)
    local margin = 24 * s

    local corner = hebnix.get_string("media_overlay_corner", "Bottom Left")
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

    local cy0 = y + top_reserve

    if hebnix.get_bool("media_overlay_show_cover", true) and S.cover_file then
        draw.image("assets/temp/" .. S.cover_file, x, cy0, art, art, { opacity = 1.0, radius = 10 * s })
    else
        draw.rect(x, cy0, art, art, { color = p.dark, filled = true, radius = 10 * s })
        draw.text(x + art / 2, cy0 + art / 2 - 14 * s, "\u{266A}",
            { color = p.vibrant, size = 34 * s, halign = "center" })
    end

    local px = x + art + gap
    draw.gradient(px, cy0, panel_w, panel_h,
        { color = p.grad1, color2 = p.grad2, radius = 12, angle = 135 })

    local pad_x = 18 * s
    local cl = px + pad_x
    local cr = px + panel_w - pad_x
    local cw = cr - cl

    local sub = S.now.artist
    if not sub or #sub == 0 then sub = S.now.album or "" end
    scroll_text(draw, S.now.title or "Unknown", cl, cy0 + 18 * s, 21 * s, p.text, cl, cw)
    scroll_text(draw, sub, cl, cy0 + 47 * s, 15 * s, p.text .. "cc", cl, cw)

    local dur = S.now.duration_ms or 0
    local pos = position_ms()
    local row_cy = cy0 + panel_h - 26 * s
    local time_w = 46 * s
    -- some players (and live streams) report no timeline, skip the clock then
    if dur > 0 then
        draw.text(cl, row_cy - 8 * s, fmt_time(pos),
            { color = p.text, size = 13 * s, bold = true })
        draw.text(cr, row_cy - 8 * s, fmt_time(dur),
            { color = p.text, size = 13 * s, halign = "right", bold = true })
    end

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

    local by = cy0 + panel_h + prog_gap
    draw.rect(x, by, card_w, prog_h, { color = "#00000073", filled = true, radius = 2 })
    if dur > 0 then
        local frac = math.max(0, math.min(1, pos / dur))
        draw.rect(x, by, card_w * frac, prog_h, { color = p.vibrant, filled = true, radius = 2 })
    end

    if lyrics_on and S.lyrics and #S.lyrics > 0 then
        local offset = hebnix.get_number("media_overlay_lyrics_offset", 0)
        local idx = lyric_index((pos + offset) / 1000)
        if idx ~= S.lyric_i then
            S.lyric_i = idx
            S.lyric_t0 = mono()
        end
        local base_color = p.text
        if not hebnix.get_bool("media_overlay_lyrics_auto_color", true) then
            base_color = hebnix.get_string("media_overlay_lyrics_color", "#ffffff")
        end
        local border = hebnix.get_number("media_overlay_lyrics_border", 0) * s
        local border_color = opposite(base_color)
        if not hebnix.get_bool("media_overlay_lyrics_border_auto", true) then
            border_color = hebnix.get_string("media_overlay_lyrics_border_color", "#000000")
        end
        local anim = hebnix.get_string("media_overlay_lyrics_anim", "Slide")
        local p_a = anim == "None" and 1.0 or math.min(1.0, (mono() - S.lyric_t0) / 0.30)

        local region_top = above and y or (by + prog_h + lyr_gap)
        local cx = x + card_w / 2
        local cy = region_top + block_h / 2
        local shift = anim == "Slide" and ((1 - p_a) * row_h) or 0

        local rows = {
            { i = idx, c = cy - row_h, cur = true },
            { i = idx + 1, c = cy, cur = false },
            { i = idx + 2, c = cy + row_h, cur = false },
        }
        for _, r in ipairs(rows) do
            local l = S.lyrics[r.i]
            if l and l.text and #l.text > 0 then
                local size = r.cur and lyr_size or (lyr_size * 0.82)
                local alpha = r.cur and 1.0 or 0.7
                if r.cur and anim == "Fade" then alpha = 0.35 + 0.65 * p_a end
                if r.cur and anim == "Scale" then size = lyr_size * (0.7 + 0.3 * p_a) end
                local ty = r.c - size / 2 + shift
                if border > 0 then
                    local bcol = with_alpha(border_color, alpha)
                    for _, o in ipairs(BORDER_OFF) do
                        draw.text(cx + o[1] * border, ty + o[2] * border, l.text,
                            { color = bcol, size = size, halign = "center", bold = r.cur })
                    end
                end
                draw.text(cx, ty, l.text,
                    { color = with_alpha(base_color, alpha), size = size,
                        halign = "center", bold = r.cur })
            end
        end
    end
end

function plugin.on_settings(ui)
    ui.label("Shows whatever media Windows reports as playing (browser, Spotify, VLC, etc.) as an overlay card.")
    if not hebnix.media_session then
        ui.colored_label("#e74c3c", "error: this Hebnix build has no media_session API")
    elseif S.now then
        ui.colored_label("#1db954", S.now.status .. " (" .. app_name(S.app) .. "): " .. (S.now.title or "?"))
    else
        ui.colored_label("#e0a030", "nothing playing")
    end
    ui.space(6)
    ui.checkbox("media_overlay_enabled", "Show overlay card", true)
    ui.checkbox("media_overlay_hide_paused", "Hide while paused", false)
    ui.checkbox("media_overlay_ignore_spotify", "Ignore Spotify (use Spotify Overlay for it)", false)
    ui.checkbox("media_overlay_show_cover", "Show cover art", true)
    ui.combo_box("media_overlay_corner", "Position", CORNERS)
    ui.slider("media_overlay_scale", "Scale", 60, 160, 100)
    ui.space(6)
    ui.checkbox("media_overlay_lyrics", "Show synced lyrics (lrclib.net)", true)
    ui.combo_box("media_overlay_lyrics_anim", "Lyrics animation", LYRIC_ANIMS)
    ui.combo_box("media_overlay_lyrics_pos", "Lyrics position", LYRIC_POS)
    ui.slider("media_overlay_lyrics_scale", "Lyrics scale", 60, 200, 100)
    ui.slider("media_overlay_lyrics_offset", "Lyrics sync (ms)", -1500, 1500, 0)
    ui.checkbox("media_overlay_lyrics_auto_color", "Auto lyrics colour", true)
    ui.color_picker("media_overlay_lyrics_color", "Lyrics colour", "#ffffff")
    ui.slider("media_overlay_lyrics_border", "Lyrics border", 0, 1.5, 0)
    ui.checkbox("media_overlay_lyrics_border_auto", "Auto border colour (opposite)", true)
    ui.color_picker("media_overlay_lyrics_border_color", "Border colour", "#000000")
    ui.space(6)
end

return plugin
