-- rlapi_demo: pokes the eos + rlapi (psynet) plugin api.
--
-- on load it kicks off an eos token fetch + a psynet population request, polls
-- both in on_tick and logs once. also shows live status in a floating window.
-- reference for plugins wanting first-party RL data (skills, pop, profiles..).

local plugin = {}

local eos_key = nil
local eos_done = false
local pop_key = nil
local pop_done = false
local skill_key = nil
local skill_done = false
local status = "starting…"
local pop_total = nil
local my_id = nil

function plugin.on_load()
    hebnix.log("RLAPI Demo loaded; detected platform = '" .. hebnix.detected_platform() .. "'")

    -- 1) EOS token (optional, RLAPI acquires one internally too).
    eos_key = hebnix.eos_token_async()          -- platform defaults to detected
    hebnix.log("  eos_token_async -> key=" .. tostring(eos_key))

    -- 2) PsyNet population (no body needed).
    pop_key = hebnix.rlapi_request_async("Population/GetPopulation v1")
    hebnix.log("  rlapi_request_async(Population/GetPopulation v1) -> key=" .. tostring(pop_key))

    hebnix.window.open{ title = "RLAPI Demo", width = 320, height = 200, opacity = 0.92 }
end

function plugin.on_tick()
    -- EOS token result
    if eos_key and not eos_done then
        local res = hebnix.eos_result(eos_key)
        if type(res) == "table" then
            eos_done = true
            my_id = res.account_id
            hebnix.log("  EOS token: account_id=" .. tostring(res.account_id)
                .. " steam_id=" .. tostring(res.steam_id)
                .. " expires=" .. tostring(res.expires_at))
            -- Now that we know our id, ask for our own skills. A non-empty
            -- steam_id means this is a Steam token; otherwise it's Epic.
            local pid
            if res.steam_id and res.steam_id ~= "" then
                pid = "Steam|" .. res.steam_id .. "|0"
            else
                pid = "Epic|" .. res.account_id .. "|0"
            end
            skill_key = hebnix.rlapi_request_async("Skills/GetPlayerSkill v1", { PlayerID = pid })
            hebnix.log("  requesting skills for " .. pid .. " -> key=" .. tostring(skill_key))
        elseif res == false then
            eos_done = true
            hebnix.log("  EOS token: FAILED")
        end
    end

    -- Population result
    if pop_key and not pop_done then
        local res = hebnix.rlapi_result(pop_key)
        if type(res) == "table" then
            pop_done = true
            if res.ok and res.result and res.result.Playlists then
                local total = 0
                for _, p in ipairs(res.result.Playlists) do
                    total = total + (p.PlayerCount or 0)
                end
                pop_total = total
                hebnix.log("  Population OK: " .. #res.result.Playlists
                    .. " playlists, " .. total .. " players online")
            else
                hebnix.log("  Population FAILED: " .. tostring(res.error))
            end
        end
    end

    -- Skill result
    if skill_key and not skill_done then
        local res = hebnix.rlapi_result(skill_key)
        if type(res) == "table" then
            skill_done = true
            if res.ok and res.result and res.result.Skills then
                hebnix.log("  Skills OK: " .. #res.result.Skills .. " playlist entries")
                for _, s in ipairs(res.result.Skills) do
                    hebnix.log(string.format("    playlist %d: MMR %.1f, %d matches",
                        s.Playlist or -1, s.MMR or 0, s.MatchesPlayed or 0))
                end
            else
                hebnix.log("  Skills FAILED: " .. tostring(res.error))
            end
        end
    end

    if eos_done and pop_done and (skill_done or not skill_key) then
        status = "done"
    else
        status = "connected=" .. tostring(hebnix.rlapi_connected())
    end
end

function plugin.on_window(ui)
    ui.heading("RLAPI Demo")
    ui.label("Platform: " .. hebnix.detected_platform())
    ui.label("Status: " .. status)
    if pop_total then ui.label("Players online: " .. pop_total) end
    if my_id then ui.label("Account: " .. my_id) end
end

return plugin
