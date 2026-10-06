-- toast_demo: hebnix.toast(text) shows over the game and lands in the notifications tab.

local plugin = {}

local DELAY = 10
local pending = {}

function plugin.on_load()
    hebnix.window.open{ title = "Toast Demo", width = 320, height = 360, opacity = 0.92 }
end

function plugin.on_unload()
    hebnix.window.close()
end

function plugin.on_tick()
    local now = hebnix.monotonic_seconds()
    for i = #pending, 1, -1 do
        if now >= pending[i] then
            table.remove(pending, i)
            hebnix.toast("delayed toast, sent " .. DELAY .. "s ago")
        end
    end
end

function plugin.on_window(ui)
    if ui.button("Send toast") then
        hebnix.toast("hello from toast demo")
    end

    ui.separator()
    local text = ui.text_input("toast_text", "toast content")
    if ui.button("Send toast with content") then
        hebnix.toast(text ~= "" and text or "empty content")
    end

    ui.separator()
    -- accent is the left bar, background and text are the card and its text
    if ui.button("Send colored toast") then
        hebnix.toast("custom colors", { accent = "#ff5555", background = "#2a1418", text = "#ffd9d9" })
    end
    -- image is a path inside this plugin's assets folder
    if ui.button("Send toast with image") then
        hebnix.toast("custom image", { image = "icon.png", accent = "#3ddc84" })
    end
    -- duration is seconds, 1.5 to 5, default 4
    if ui.button("Send long toast (5s)") then
        hebnix.toast("stays a bit longer", { duration = 5 })
    end

    ui.separator()
    if ui.button("Send delayed toast (" .. DELAY .. "s)") then
        table.insert(pending, hebnix.monotonic_seconds() + DELAY)
    end
    ui.label("waiting: " .. #pending)

    ui.separator()
    if ui.button("Send 5 toasts at once") then
        for i = 1, 5 do
            hebnix.toast("burst " .. i)
        end
    end
end

return plugin
