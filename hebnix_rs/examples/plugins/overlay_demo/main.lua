-- overlay_demo: reference for the game overlay (on_overlay).
--
-- the overlay is a transparent click-through canvas drawn over RL while the
-- game's focused, updates ~20x/s. everything's drawn with the draw table:
--
--   draw.line(x1, y1, x2, y2, {color="#rrggbb[aa]", width=1})
--   draw.rect(x, y, w, h,     {color=, width=, filled=false})
--   draw.circle(x, y, radius, {color=, width=, filled=false})
--   draw.text(x, y, "str",    {color=, size=14, halign="left"|"center"|"right"})
--   draw.polygon({{x,y},...}, {color=})   -- filled convex polygon
--
-- Colors accept 6-digit (#RRGGBB) or 8-digit (#RRGGBBAA) hex for opacity.

local plugin = {}

-- Live data fed from game events, rendered on the overlay.
local goals = { [0] = 0, [1] = 0 }   -- team 0 (blue) / team 1 (orange)
local clock = nil                    -- seconds remaining, if known
local last_goal_by = nil
local ball_speed = 0

function plugin.on_load()
    hebnix.log("Overlay Demo loaded, focus Rocket League to see it.")
end

function plugin.on_game_event(event_type, event)
    local d = event.data or {}
    if event_type == "GoalScored" then
        local team = (d.Scorer and d.Scorer.TeamNum) or 0
        goals[team] = (goals[team] or 0) + 1
        last_goal_by = d.Scorer and d.Scorer.Name or "?"
    elseif event_type == "ClockUpdatedSeconds" then
        clock = d.TimeSeconds
    elseif event_type == "BallHit" then
        ball_speed = (d.Ball and d.Ball.PostHitSpeed) or ball_speed
    elseif event_type == "UpdateState" then
        if d.Game and d.Game.Ball then
            ball_speed = d.Game.Ball.Speed or ball_speed
        end
    elseif event_type == "GameLeft" or event_type == "MatchEnded" then
        goals = { [0] = 0, [1] = 0 }
        clock = nil
        last_goal_by = nil
        ball_speed = 0
    end
end

local function fmt_clock(secs)
    if not secs then return "--:--" end
    secs = math.max(0, math.floor(secs))
    return string.format("%d:%02d", math.floor(secs / 60), secs % 60)
end

function plugin.on_overlay(draw, w, h)
    local cx = w / 2

    -- Scoreboard pill, centered near the top (like a real HUD).
    local pill_w, pill_h = 220, 46
    local px, py = cx - pill_w / 2, 24
    draw.rect(px, py, pill_w, pill_h, { color = "#0b0b0bcc", filled = true })
    draw.rect(px, py, pill_w, pill_h, { color = "#ffffff40", width = 1 })
    -- team color chips
    draw.rect(px + 8, py + 8, 10, pill_h - 16, { color = "#3aa0ff", filled = true })
    draw.rect(px + pill_w - 18, py + 8, 10, pill_h - 16, { color = "#ff8a3a", filled = true })
    draw.text(px + 54, py + 6, tostring(goals[0]), { color = "#3aa0ff", size = 26 })
    draw.text(cx, py + 4, fmt_clock(clock), { color = "#ffffff", size = 20, halign = "center" })
    draw.text(px + pill_w - 54, py + 6, tostring(goals[1]),
        { color = "#ff8a3a", size = 26, halign = "right" })

    -- Center crosshair.
    draw.circle(cx, h / 2, 26, { color = "#2ecc7160", width = 2 })
    draw.line(cx - 30, h / 2, cx - 10, h / 2, { color = "#2ecc71", width = 2 })
    draw.line(cx + 10, h / 2, cx + 30, h / 2, { color = "#2ecc71", width = 2 })
    draw.line(cx, h / 2 - 30, cx, h / 2 - 10, { color = "#2ecc71", width = 2 })
    draw.line(cx, h / 2 + 10, cx, h / 2 + 30, { color = "#2ecc71", width = 2 })

    -- Ball-speed bar, bottom center (0..6000 uu/s scale).
    local bar_w, bar_h = 300, 14
    local bx, by = cx - bar_w / 2, h - 60
    local frac = math.min(1.0, ball_speed / 6000)
    draw.rect(bx, by, bar_w, bar_h, { color = "#000000aa", filled = true })
    draw.rect(bx, by, bar_w * frac, bar_h, { color = "#f1c40f", filled = true })
    draw.rect(bx, by, bar_w, bar_h, { color = "#ffffff40", width = 1 })
    draw.text(cx, by - 18, string.format("Ball %d uu/s", math.floor(ball_speed)),
        { color = "#ffffff", size = 13, halign = "center" })

    -- Info line, bottom-left.
    local info = "Overlay Demo   " .. w .. "x" .. h
    if last_goal_by then info = info .. "   last goal: " .. last_goal_by end
    draw.text(16, h - 28, info, { color = "#cccccc", size = 13 })

    -- Team-colored corner triangles to show polygon + alpha.
    draw.polygon({ { 0, 0 }, { 60, 0 }, { 0, 60 } }, { color = "#3aa0ff70" })
    draw.polygon({ { w, 0 }, { w - 60, 0 }, { w, 60 } }, { color = "#ff8a3a70" })
end

function plugin.on_settings(ui)
    ui.heading("Overlay Demo")
    ui.label("Focus Rocket League and this HUD draws over it:")
    ui.label("- scoreboard + clock (live from StatsAPI)")
    ui.label("- crosshair, ball-speed bar, corner markers")
    ui.space(6)
    ui.label("It's click-through, so it never steals game input.")
    ui.space(6)
    ui.label(string.format("Score %d - %d   Ball %d uu/s", goals[0], goals[1],
        math.floor(ball_speed)))
end

return plugin
