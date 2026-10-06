# Hebnix English strings. This file is the source of truth for every locale.
#
# - keys are stable ids, never use the english text as the id
# - { $name } is a variable, translators must keep the same variables
# - plurals use a selector and always need a *[other] default
# - see locales/README.md for the translation workflow

## language picker
language-draft-suffix = draft
settings-language-label = Language:
settings-language-auto = Auto (system)

## main tabs
tab-console = Console
tab-workshop = Maps
tab-spoofer = Spoofer
tab-patcher = Items
tab-colours = Colours
tab-settings = Settings
tab-plugins = Plugins
tab-rlapi = Experimental
tab-about = About

## shared buttons
btn-browse = Browse
btn-refresh = Refresh
btn-reset = Reset
btn-open-folder = Open Folder

## action button
action-start-rocket-league = Start Rocket League
action-restart-rocket-league = Restart Rocket League
action-close-rocket-league = Close Rocket League
action-open-hebnix-folder = Open Hebnix Folder
action-open-plugins-folder = Open Plugins Folder
action-reload-plugins = Reload Plugins
action-fix-epic-connection = Fix Epic Connection
action-settings-intro = Choose the actions available for each Rocket League state.
action-settings-hint = Drag the :: handle to reorder. The first checked action is the main button action.
action-state-closed = State: Rocket League Closed
action-state-open = State: Rocket League Open
action-keep-one = At least one action must remain selected.

## tray
tray-show = Show
tray-hide = Hide
tray-close = Close

## settings: navigation
settings-subtab-hebnix = Hebnix Settings
settings-subtab-plugin = Plugin Settings
settings-nav-interface = Interface
settings-nav-directories = Directories & Files
settings-nav-system = System
settings-nav-discord = Discord
settings-nav-action-button = Action Button
settings-heading-interface = Interface Configuration
settings-heading-directories = Directories & Files Configuration
settings-heading-system = System Configuration
settings-heading-discord = Discord
settings-heading-action-button = Action Button

## settings: interface
settings-keybind-label = Open/Close Keybind:
settings-keybind-listening = Listening...
settings-keybind-set = Set Keybind
settings-theme-label = Theme:
settings-open-fonts-folder = Open Fonts Folder
settings-opacity-label = Window Opacity:
console-themes-rescanned = Themes directory rescanned and updated.
console-theme-changed = Theme changed to '{ $theme }'.
console-theme-failed = Theme '{ $theme }' failed to load. Defaulted to Dark mode. Check theme_errors.txt

## settings: directories
settings-dirs-autodetected = (auto-detected from the running game)
settings-rl-folder-label = Rocket League Folder:
settings-statsapi-ini-label = DefaultStatsAPI.ini:
filter-ini-files = INI files
settings-not-found = Not Found
settings-set-value = Set { $value }
settings-ini-restart-note = Changes to the ini apply after restarting Rocket League.
settings-rl-launch-heading = Rocket League Launch
settings-rl-launch-desc = Tells Hebnix how to restart Rocket League - used by the Restart Rocket League button and Workshop LAN's Host/Join. Leave as auto-detect unless you use Heroic.
settings-rl-mode-unconfigured = Auto-detect (Steam or Epic)
settings-rl-mode-steam-native = Real Steam game
settings-rl-mode-epic-direct = Epic Games Launcher
settings-rl-mode-steam-shortcut = Non-Steam shortcut to Heroic
settings-rl-mode-heroic-direct = Heroic directly
settings-rl-current-setup = Current setup: { $mode }
settings-rl-launch-setup-button = Rocket League Launch Setup...

## settings: discord
discord-enable = Enable Discord Rich Presence
discord-rl-only = Limit Discord RPC to Rocket League only
discord-message-label = Message:
discord-game-state = Game State
discord-show-score = Show score
discord-show-map = Show map
discord-show-gamemode = Show gamemode
discord-custom = Custom
discord-custom-hint = Custom message
discord-custom-disabled-note = The custom message is disabled while Game State is selected.

## settings: system
system-start-with-windows = Start with Windows:
console-startup-failed = Failed to update startup entry: { $error }
system-start-in-tray = Start in Tray:
system-close-to-tray = Close to Tray:
system-p2p = P2P File Sharing:
system-p2p-note = Turning this off stops other players in Workshop multiplayer from downloading maps from your PC. Custom maps that aren't on the Hebnix Workshop can then no longer be downloaded by other players.
system-suppress-left = Suppress Left Alerts:
system-fullscreen-warning = Fullscreen Warning:
system-fullscreen-note = Warns when the game is fullscreen and the overlay can't draw.
system-allow-draw-focus = Allow Draw on Hebnix Focus:
system-limit-hotkey = Limit Hotkey to Hebnix/Rocket League:
system-statsapi-rate = StatsAPI Rate Warning:
system-statsapi-note = Warns when PacketSendRate isn't 20. Rates of 10 and under always warn.
system-default-tab = Default Tab:

## settings: plugins page
plugin-settings-none = No Plugins with Settings Enabled
plugin-settings-go = Go to Plugins

## update and changelog
update-required-title = Update Required
update-required-heading = Hebnix v{ $version } is required
update-locked-note = Hebnix is locked until the required update is installed.
update-downloading = Downloading & Installing...
update-button = Update Hebnix
update-required-banner = A required Hebnix update is available
changelog-title = Change Log
changelog-heading = Hebnix v{ $version }
changelog-added = ADDED:
changelog-fixed = FIXED:
changelog-removed = REMOVED:

## dialogs
btn-ok = OK
launch-notice-body = You must start Rocket League at least once with Hebnix running to use this function.

## discord presence (shown to other people on Discord)
discord-presence-playing = Playing Rocket League
discord-presence-in-game = In Rocket League
discord-presence-main-menu = Main menu
discord-presence-in-match = In a match
discord-presence-score = Score: { $score }
discord-button-website = Hebnix Website
discord-button-community = Hebnix Discord

## lite app (wording that differs from the full app)
lite-update-required-banner = A required Hebnix Lite update is available
lite-update-required-heading = Hebnix Lite v{ $version } is required
lite-update-locked-note = Hebnix Lite is locked until the required update is installed.
lite-system-start-hidden = Start Hidden:
lite-system-start-hidden-hover = Starts minimised. Use the toggle hotkey to show it.
lite-system-fullscreen-note = Warns when the game is fullscreen and overlays cannot draw.
lite-system-statsapi-note = Warns when PacketSendRate is not 20.
lite-refresh-statsapi = Refresh StatsAPI values

## items tab
items-active-changes =
    { $count ->
        [one] { $count } active change
       *[other] { $count } active changes
    }
console-swapper-restored =
    Restored { $count ->
        [one] { $count } active swap
       *[other] { $count } active swaps
    }.

## app.rs
presets-presets = Presets
presets-save-and-restore-named-collections-of = Save and restore named collections of your current item changes.
presets-name = Name:
presets-include-patches = Include patches
presets-save-preset = Save Preset
presets-import-json = Import JSON
presets-json = JSON
presets-no-presets-saved = No presets saved.
presets-preset-contents-are-shown-above-applying = Preset contents are shown above; applying a preset will be enabled alongside the patch restore/apply operations.
spoofer-spoofer-settings = Spoofer Settings
spoofer-name-title = Name & Title
spoofer-rank = Rank
spoofer-friends = Friends
spoofer-enable-spoofer = Enable Spoofer
spoofer-account-proxy = Account Proxy
spoofer-running = Running
spoofer-stopped = Stopped
spoofer-psynet-proxy = PsyNet Proxy
spoofer-installed = Installed
spoofer-remove = Remove
spoofer-missing = Missing
spoofer-install-certificate = Install Certificate
spoofer-refresh-status = Refresh Status
spoofer-name-title-spoofing = Name & Title Spoofing
spoofer-enable-username-spoof = Enable Username Spoof
spoofer-use-recent-spoofed-name = Use recent spoofed name
spoofer-titles = Titles
spoofer-title-to-replace = Title to replace:
spoofer-filter = Filter:
spoofer-search-titles = Search titles...
spoofer-clear = Clear
spoofer-all = All
spoofer-copy-title = Copy title:
spoofer-choose-a-title = Choose a title...
spoofer-this-title-is-already-being-replaced = This title is already being replaced
spoofer-no-titles-added = No titles added
spoofer-glow = Glow
spoofer-replaces = replaces
spoofer-remove-this-title = Remove this title
spoofer-friend-spoofing = Friend Spoofing
spoofer-enable-friend-name-spoof = Enable Friend Name Spoof
spoofer-search = Search:
spoofer-revert-all = Revert All
spoofer-spoofed-name = Spoofed Name
spoofer-rank-spoofing = Rank Spoofing
spoofer-enable-rank-spoofing = Enable Rank Spoofing
spoofer-mmr = MMR
overlay-order-drag-to-reorder-renders-based-on = Drag to reorder. Renders based on number order.
plugins-install-plugin = Install Plugin
plugins-delete-plugin = Delete plugin
plugins-no-plugins-installed-drop-a-plugin = No plugins installed. Drop a plugin folder into plugins/.
about-hebnix = Hebnix
about-built-with-help-from-the-community = Built with help from the community
    Contributors:
    xplodingeggo
statsapi-notice-set-20 = Set 20
web-port-notice-set-49124 = Set 49124
web-port-notice-dismiss = Dismiss
spawner-enable-prompt-read-the-tutorial-tab-before-enabling = Read the Tutorial tab before enabling Item Spawner.
spawner-enable-prompt-rocket-league-must-be-closed-first = Rocket League must be closed first. Item spawning is temporary.
spawner-enable-prompt-continue = Continue
spawner-enable-prompt-cancel = Cancel
colour-admin-prompt-windows-denied-write-access-to-tagame = Windows denied write access to TAGame.upk. Restart Hebnix as Administrator, then return to Colours and apply again.
colour-admin-prompt-restart-as-administrator = Restart as Administrator
plugin-delete-prompt-yes = Yes
plugin-delete-prompt-no = No
item-action-prompt-rocket-league-must-be-closed-to = Rocket League must be closed to swap items, patch, or change colours.
item-action-prompt-quit-rocket-league = Quit Rocket League
item-action-prompt-close-prompt = Close Prompt
owned-proxy-prompt-hebnix-needs-its-local-proxy-to = Hebnix needs its local proxy to read your Rocket League inventory.
    The response is observed locally and is not modified or uploaded.
owned-proxy-prompt-the-hebnix-proxy-certificate-will-also = The Hebnix proxy certificate will also be installed.
owned-proxy-prompt-enable-proxy = Enable Proxy
fullscreen-notice-rocket-league-is-set-to-fullscreen = Rocket League is set to Fullscreen, so the overlay won't draw over it.
    Switch the game's video settings to Borderless or Windowed.
fullscreen-notice-never-show-again = Never show again
rl-launch-setup-how-do-you-actually-launch-rocket = How do you actually launch Rocket League? This decides how the Restart Rocket League button and Workshop LAN's Host/Join work. Auto-detect (the default) works for real Steam and Epic Games Launcher installs already - only pick something else here if you use Heroic.
rl-launch-setup-auto-detect-steam-or-epic-games = Auto-detect (Steam or Epic Games Launcher)
rl-launch-setup-non-steam-shortcut-that-opens-heroic = Non-Steam shortcut that opens Heroic
rl-launch-setup-heroic-directly-no-steam-or-epic = Heroic directly, no Steam or Epic Games Launcher involved
rl-launch-setup-steam-app-id-252950-is-rocket = Steam App ID (252950 is Rocket League's own real listing):
rl-launch-setup-scan-steam-shortcuts-for-heroic = Scan Steam shortcuts for Heroic
rl-launch-setup-no-candidates-found-yet-click-scan = No candidates found yet - click Scan, or enter the ID manually below.
rl-launch-setup-steam-shortcut-id = Steam shortcut ID:
rl-launch-setup-heroic-binary-path-e-g-the = Heroic binary path (e.g. the full path to Heroic.exe):
rl-launch-setup-epic-catalog-app-name-sugar-is = Epic catalog app name (Sugar is Rocket League's, same for everyone):
rl-launch-setup-runner = Runner:
rl-launch-setup-save = Save
install-modal-plugin-archives = Plugin Archives
hebnix-install-back = < Back
hebnix-install-search-plugins = 🔍 Search plugins...
hebnix-install-fetching-plugins = Fetching plugins...
hebnix-install-disable = Disable
hebnix-install-install = Install
app-items-swapper = Items Swapper
app-items-spawner = Items Spawner
app-enable-items-spawner = Enable Items Spawner
app-read-tutorial-before-enabling = Read Tutorial before enabling.
app-tutorial = Tutorial
app-item-spawner-tutorial = Item Spawner Tutorial
app-rocket-league-must-be-closed-before = Rocket League must be closed before you enable the Item Spawner.
app-if-you-close-hebnix-while-rocket = If you close Hebnix while Rocket League is open, its network watchdog stays active until the game exits.
app-if-you-have-problems-connecting-to = If you have problems connecting to Epic, open Hebnix while Rocket League is closed, enable Item Spawner and disable it again, then open Rocket League.
app-spawned-items-can-persist-locally-or = Spawned items can persist locally or disappear, including mid match.
app-disabling-item-spawner-stops-interceptio = Disabling Item Spawner stops interception but does not edit Rocket League account saves.
app-start-rocket-league-to-use-the = Start Rocket League to use the item categories.
app-loading-item-catalogs = Loading item catalogs...
app-reload-catalogs = Reload Catalogs
app-ball-patcher = Ball Patcher
app-boost-patcher = Boost Patcher
app-decal-patcher = Decal Patcher
app-active = Active
app-active-items = Active Items
app-no-patches-or-swaps-are-active = No patches or swaps are active.
app-replaced-the-default-ball = Replaced the default ball
app-restore = Restore
app-replaced-the-default-boost-meter = Replaced the default boost meter
app-rlapi = RLAPI
app-ball-appearance = Ball Appearance
app-car-patcher = Car Patcher
app-wheel-alignment = Wheel Alignment
app-hebnix-has-initialised-statsapi-please-r = Hebnix has initialised StatsAPI. Please restart Rocket League to initialise Hebnix.
presets-delete = Delete
spoofer-requires-admin-for-hosts-file-intercepti = Requires Admin for hosts-file interception
spoofer-for-username-friends-and-rank-spoofing = For Username, Friends and Rank Spoofing
spoofer-for-title-and-rank-spoofing = For Title and Rank Spoofing
spoofer-add-title-to-list = Add title to list
spoofer-enabled-titles = Enabled titles:
spoofer-start-rocket-league-to-populate-friends = Start Rocket League to populate friends list
plugins-reload = Reload
hebnix-settings-packetsendrate = PacketSendRate:
hebnix-settings-port = Port:
hebnix-settings-webport = WebPort:
statsapi-notice-statsapi-configuration = StatsAPI configuration
statsapi-notice-edit-it-by-hand-or-restart = Edit it by hand, or restart Hebnix as administrator.
statsapi-notice-turn-this-off-in-settings-system = Turn this off in Settings > System.
web-port-notice-webport-configuration = WebPort configuration
spawner-enable-prompt-read-the-item-spawner-tutorial = Read the Item Spawner tutorial
admin-prompt-administrator-required = Administrator required
plugin-delete-prompt-delete-plugin = Delete plugin?
item-action-prompt-rocket-league-is-open = Rocket League is open
owned-proxy-prompt-enable-owned-item-catalog = Enable owned-item catalog
fullscreen-notice-overlay-unavailable = Overlay unavailable
launch-path-notice-rocket-league = Rocket League
rl-launch-setup-rocket-league-launch-setup = Rocket League Launch Setup
rl-launch-setup-non-steam-shortcuts-can-t-pass = Non-Steam shortcuts can't pass launch arguments (a Steam limitation). Workshop LAN's -multihome relaunch will launch Heroic directly instead, bypassing Steam for that session.
install-modal-install-from-hebnix = ☁
    
    Install from Hebnix
install-modal-install-from-zip = 📁
    
    Install from .ZIP
hebnix-install-no-plugins-found = No plugins found.
hebnix-install-enable = Enable
hebnix-install-installing = Installing...
hebnix-install-next = Next >
hebnix-install-prev = < Prev
app-restore-all = Restore All

## epic_connection.rs
epic-connection-rocket-league-must-close-to-repair = Rocket League must close to repair the Epic connection. Close it now?
epic-connection-epic-connection-repaired = Epic Connection Repaired

## lite_app.rs
overlay-order-drag-to-reorder-the-top-one = Drag to reorder. The top one draws over the ones below it.
install-modal-plugin-archive = Plugin archive
install-modal-search-plugins = Search plugins
about-hebnix-lite = Hebnix Lite
notices-don-t-show-again = Don't show again
notices-no-game-data-reaches-plugins-until = No game data reaches plugins until this is fixed.
notices-set-packetsendrate-to-20 = Set PacketSendRate to 20
notices-later = Later
notices-fullscreen-warning = Fullscreen warning

## patcher.rs
patch-source-selector-catalog = Catalog
patch-source-selector-local = Local

## patcher/ball.rs
render-ball = Ball
ball-name-or-author = Name or author...
ball-search = Search
ball-zip-archives = ZIP Archives
ball-show-applied = Show Applied
render-confirm-deletion = Confirm Deletion
ball-restore-original = Restore Original
ball-import-zip = Import ZIP
ball-no-balls-found-in-the-balls = No balls found in the /balls/ directory.
ball-no-balls-match-your-search = No balls match your search.
ball-previous = Previous
ball-next = Next
ball-no-image = No Image
ball-apply = Apply

## patcher/car_patcher.rs
tab-search-local-patches = Search local patches
tab-no-local-car-patches-import-a = No local car patches. Import a ZIP containing a JSON manifest and UPK.
tab-these-packages-can-safely-replace-their = These packages can safely replace their resolved stock body UPK.
tab-not-supported = Not supported
tab-kept-for-reference-applying-these-profil = Kept for reference; applying these profiles is disabled.

## patcher/catalog.rs
render-refreshing-catalog = Refreshing catalog...
render-view-downloaded = View Downloaded
card-failed-to-load-image = Failed to load image
card-no-image = No image
card-loading-image = Loading image...

## patcher/decal_patcher.rs
tab-restore-every-patched-decal = Restore every patched decal
tab-retry-loading-catalog = Retry Loading Catalog
tab-active-decal-patches = Active decal patches
tab-each-item-lists-api-decals-for = Each item lists API decals for its matching car. Patched targets stay locked until restored.
tab-restore-every-patch-using-this-decal = Restore every patch using this decal before deleting it
tab-legacy-this-will-restore-all-decals-to = This will restore ALL decals to their original state.
tab-legacy-search-decals = Search decals...
tab-legacy-this-decal-s-bodyid-does-not = This decal's BodyID does not match a car in the catalogs
tab-legacy-decal-to-replace = Decal to replace:
tab-legacy-no-decals-available-check-skins-json = No decals available - check skins.json
tab-legacy-select-an-imported-decal-first = Select an imported decal first
tab-legacy-no-decal-applied-to-this-skin = No decal applied to this skin
tab-confirm-restore-all-decals = Confirm Restore All Decals
tab-legacy-confirm-restore-all = Confirm Restore All
tab-legacy-available-decals = Available Decals
tab-legacy-no-decals-found = No decals found
tab-legacy-target-skin = Target Skin
tab-legacy-no-decals-match-the-filter = No decals match the filter
tab-legacy-apply-decal = Apply Decal
tab-legacy-select-a-decal-from-the-left = Select a decal from the left panel

## patcher/swapper.rs
thumbnail-status-retry-previews = Retry previews
resolved-items-preparing-items = Preparing items...
active-swaps-item-swaps = Item swaps
active-swaps-original-replacement = Original → replacement
tab-show-only-owned-replacements = Show only owned replacements
tab-waiting-for-rocket-league-inventory = Waiting for Rocket League inventory...
tab-car = Car:
tab-search-cars = Search cars...
tab-match-selected-car = Match Selected Car
tab-limit-replacement-decals-to-the-selected = Limit replacement decals to the selected car
tab-search-items = Search items...
tab-no-owned-replacement-is-available = No owned replacement is available
spawn-catalog-could-not-be-loaded = Catalog could not be loaded
spawn-no-spawnable-items-match-the-search = No spawnable items match the search.
spawn-paint = Paint
spawn-spawn = Spawn

## patcher/wheel_alignment.rs
render-filter-cars = Filter cars…
render-car-catalog-is-not-available-yet = Car catalog is not available yet.
render-negative-tops-inward-positive-tops-outwa = Negative: tops inward. Positive: tops outward.
render-positive-up-negative-down-values-are = Positive: up. Negative: down. Values are model units.
render-mirrored-adjustment-positive-moves-both = Mirrored adjustment: positive moves both sides outward; negative moves inward.
render-close-rocket-league-before-changing-alig = Close Rocket League before changing alignment.
render-revert-settings = Revert settings
render-revert-to-0 = Revert to 0
render-close-rocket-league-before-restoring-ali = Close Rocket League before restoring alignment.
render-no-cars-adjusted-yet = No cars adjusted yet.
render-apply-alignment = Apply alignment

## ui/console.rs
render-enter-system-command = Enter system command...

## ui/rlapi.rs
rlapi-enable-before-launching-rocket-league-to = Enable before launching Rocket League to capture its session. Requests share the game's connection.
rlapi-disabling-stops-new-requests-the-relay = Disabling stops new requests. The relay stays running until Rocket League exits, even if you close this window.
rlapi-endpoint = Endpoint
rlapi-namespace-endpoint-v1 = Namespace/Endpoint v1
rlapi-payload-json = Payload (JSON)
rlapi-waiting-for-response = Waiting for response…
rlapi-response = Response
rlapi-send-request = Send request
rlapi-format-json = Format JSON
rlapi-copy-response = Copy response

## ui/workshop.rs
my-slot-everyone-has-it = everyone has it
player-maps-hasn-t-replaced-any-map = Hasn't replaced any map.
player-maps-imported-not-on-the-workshop = (imported, not on the Workshop)
player-maps-you-have-it = you have it
player-maps-from-the-cdn = From the CDN
player-maps-from-this-player = From this player
multiplayer-help-button-using-a-vpn-proxy-or-having = Using a VPN/proxy, or having issues? Read this
multiplayer-help-button-opens-the-workshop-multiplayer-help-page = Opens the Workshop multiplayer help page on GitHub
render-browse-maps = Browse Maps
render-background-changer = Background Changer
render-import-map = Import Map
render-multiplayer = Multiplayer
render-can-t-find-the-map-you = Can't find the map you want? You can download maps straight from the Steam Workshop in the Import Map tab.
render-no-hubcap-api-key-the-import = No Hubcap API key? The Import Map tab also lists the RL Workshop Archive, where anyone can request a Workshop map.
render-can-t-find-a-map = Can't find a map?
render-map-to-replace = Map To Replace:
multiplayer-workshop-multiplayer = Workshop Multiplayer
multiplayer-connects-you-to-the-private-workshop = Connects you to the private Workshop network. Host or join from inside Rocket League's own LAN match screen once it's up.
multiplayer-hide-my-ip-from-other-players = Hide my IP from other players (adds ping)
multiplayer-sends-all-multiplayer-traffic-through-ta = Sends all multiplayer traffic through Tailscale's relay servers instead of straight to other players, so they never see your real IP address. Matches will have higher ping.
multiplayer-workshop-multiplayer-requires-hebnix-to = Workshop multiplayer requires Hebnix to run as administrator.
multiplayer-back = Back
multiplayer-setup = Setup
multiplayer-connecting-to-the-private-workshop-netwo = Connecting to the private Workshop network...
multiplayer-step-1-start-rocket-league-on = Step 1: Start Rocket League on the Workshop network.
multiplayer-step-1-waiting-for-rocket-league = Step 1: Waiting for Rocket League to apply the Workshop address.
multiplayer-rocket-league-was-not-started-with = Rocket League was not started with the Workshop network address.
multiplayer-rocket-league-must-restart-because-multi = Rocket League must restart because multihome is fixed when the game starts.
multiplayer-starting-the-workshop-relay = Starting the Workshop relay...
multiplayer-ready-host-or-join-from-rocket = Ready. Host or join from Rocket League's own LAN match screen - make sure both sides have the same Workshop map installed.
multiplayer-blocks-last-until-you-disconnect = Blocks last until you disconnect.
multiplayer-unblock = Unblock
multiplayer-relaying-to-the-workshop-network = Relaying to the Workshop network.
multiplayer-disconnect = Disconnect
maps-in-use-maps-in-use = Maps in use
maps-in-use-everyone-has-to-replace-the-same = Everyone has to replace the same in-game map with the same Workshop map, e.g. Utopia Retro replaced with the same map on every PC.
maps-in-use-no-other-players-found-yet = No other players found yet.
maps-in-use-maps-installed-from-another-player-come = Maps installed from another player come straight from their PC. Only install from players you trust.
maps-in-use-block-player = Block player
maps-in-use-hide-this-player-s-maps-and = Hide this player's maps and refuse their connections until you disconnect
maps-in-use-no-players-match-that-search = No players match that search.
maps-in-use-installing-the-map = Installing the map...
peer-install-warning-hebnix-only-checks-that-the-file = Hebnix only checks that the file is a map package. Only install maps from players you trust.
render-prev = << Prev
render-next = Next >>
render-offboard-map = Offboard Map
multiplayer-connect = Connect
maps-in-use-you = You
peer-install-warning-install-from-another-player = Install from another player?
card-failed-to-load = Failed to load
card-no-image-available = No Image Available

## ui/workshop/archive.rs
render-no-api-key-use-the-rl = No API key? Use the RL Workshop Archive
render-if-you-can-t-get-a = If you can't get a Hubcap API key (or can't be bothered), request the map on the RL Workshop Archive instead. Paste its Steam Workshop link there and it's usually added within 5-10 minutes. Then download it below, or download the .zip from the site and import it with "Choose map file..." above.
render-open-the-rl-workshop-archive-request = Open the RL Workshop Archive (request a map)
render-hide-list = Hide list
render-search-name-author-or-tag = Search name, author or tag...
render-show-archive-maps = Show archive maps
render-refresh-list = Refresh list
render-download = Download

## ui/workshop/background_changer.rs
render-keep-an-arena-s-gameplay-and = Keep an arena's gameplay and networking, but borrow another arena's fog, sky, buildings, and distant scenery.
render-approved-sources-are-filtered-to-keep = Approved sources are filtered to keep only sky, atmosphere, buildings, and distant scenery; donor arena geometry is removed.
render-custom-backgrounds-are-loaded-from-path = Custom .udk/.upk backgrounds are loaded from { $path }
render-background-folder = Background Folder
render-apply-background-to-all-unchanged-maps = Apply Background to All Unchanged Maps
render-all-installed-maps-already-have-background-changes = All installed maps already have background changes.
render-applying-background-to-map-count = Applying background to maps… { $done }/{ $total }
render-apply-background-to-all-maps = Apply Background to All Maps
render-applying-to-all-maps-may-take-a-long-time = This can take a long time. Maps are processed one at a time and appear in Active Changes as they finish.
render-bulk-background-map-counts = { $targets } maps will be changed. { $skipped } existing changes will be skipped.
render-existing-background-changes-will-not-be-overwritten = Existing map background changes will not be overwritten.
render-apply-to-all = Apply to All
render-cancel = Cancel
render-close-rocket-league-before-applying-or = Close Rocket League before applying or restoring a background. Changes load when the game starts.
render-no-supported-arena-packages-were-found = No supported arena packages were found in the configured Rocket League folder.
render-scan-again = Scan Again
render-when-you-play = When you play:
render-search-maps = Search maps...
render-no-maps-match-the-filter = No maps match the filter.
render-use-the-fog-sky-background-from = Use the fog + sky + background from:
render-search-backgrounds = Search backgrounds...
render-no-background = No background
render-no-backgrounds-match-the-filter = No backgrounds match the filter.
render-choose-two-different-arenas = Choose two different arenas.
render-active-changes = Active Changes
render-no-arena-backgrounds-are-changed = No arena backgrounds are changed.
render-arena = Arena
render-borrowed-background = Borrowed background

## ui/workshop/local_import.rs
pick-map-file-rocket-league-map-or-map-zip = Rocket League map or map zip
import-vdf-steam-workshop-item = Steam workshop item
render-import-a-map = Import a map
render-add-a-map-that-isn-t = Add a map that isn't on the Workshop. Players on your Workshop network can download it from you.
render-step-1-choose-the-map-file = Step 1: Choose the map file
render-a-upk-or-udk-file-or = A .upk or .udk file, or a .zip with the map inside (like the ones from the RL Workshop Archive).
render-find-it-under-browse-maps-in = Find it under Browse Maps, in View Downloaded.
render-choose-map-file = Choose map file...
render-step-2-details = Step 2: Details
render-import-details-from-a-vdf-file = Import details from a VDF file...
render-name = Name
render-author = Author
render-description = Description
render-choose-image = Choose image...
render-image = Image
render-optional-banner-shown-on-the-map = Optional banner shown on the map card.
render-import-map-2 = Import map

## ui/workshop/steam_download.rs
render-download-from-the-steam-workshop = Download from the Steam Workshop
render-paste-a-rocket-league-workshop-link = Paste a Rocket League workshop link or id and Hebnix downloads the map and imports it. This needs your own Hubcap API key and the .NET runtime.
render-hubcap-api-key = Hubcap API key
render-paste-your-key = paste your key
render-get-a-key = Get a key
render-workshop-link-or-id = Workshop link or id
render-https-steamcommunity-com-sharedfiles-fil = https://steamcommunity.com/sharedfiles/filedetails/?id=...
render-enter-a-hubcap-api-key-first = Enter a Hubcap API key first.
render-that-doesn-t-look-like-a = That doesn't look like a workshop link or id.
render-to-install-it-with-winget-run = To install it with winget, run this in a terminal:
render-run-in-terminal = Run in terminal
render-copy-command = Copy command
render-when-it-finishes-click-download-map = When it finishes, click Download map again.
render-download-map = Download map

## app.rs
plugin-settings-display-name-configuration = { $display_name } Configuration
about-version-app-version-a-safe-eac = Version { $version }
    
    A safe, EAC-compliant Mod Loader for Rocket League with integrated Spoofer & Item Changer.
    
    hebnix.com
    
    Built by Hebbins & nixvio64.
    
    Press { $hotkey } to show/hide Hebnix.
statsapi-notice-couldn-t-write-the-file-err = Couldn't write the file:
    { $err }
plugin-delete-prompt-are-you-sure-you-want-to = Are you sure you want to delete { $plugin_name }?
rl-launch-setup-heroic-binary-rl-launch-draft = Heroic binary: { $heroic_binary }
hebnix-install-by-author = by { $author }
hebnix-install-page-display-page-of-total-pages = Page { $display_page } of { $total_pages }
plugin-windows-window-error-e = window error: { $e }
app-catalog-download-failed-error = Catalog download failed: { $error }
app-replaced-target-label = Replaced { $target_label }
app-replaced-target-for-its-bodyid-car = Replaced { $target } for its BodyID car
overlay-order-overlay-order-rows-layers = Overlay order ({ $rows } layers)

## epic_connection.rs
epic-connection-could-not-repair-the-epic-connection = Could not repair the Epic connection: { $error }

## lite_app.rs
install-modal-page-install-modal-of-total-pages = Page { $page } of { $total_pages }
about-version-app-version-a-safe-eac-2 = Version { $version }
    
    A safe, EAC-compliant Mod Loader for Rocket League.
    
    hebnix.com
    
    Built by Hebbins & nixvio64.
    
    Press { $hotkey } to show/hide Hebnix Lite.
plugin-windows-window-error-error = window error: { $error }

## patcher/ball.rs
render-are-you-sure-you-want-to = Are you sure you want to delete '{ $ball_to_delete }'?
ball-page-page-of-pages = Page { $page } of { $pages }

## patcher/car_patcher.rs
tab-supported-patches-supported = Supported patches ({ $supported })
tab-patching-target-to-car = Patching: { $target } to { $car }

## patcher/catalog.rs
card-number-downloads = { $number } downloads
card-by-field = by { $field }

## patcher/decal_patcher.rs
tab-patched-into-target-display-name = patched into { $target_display_name }
tab-delete-decal-from-local-decals = Delete '{ $decal }' from local decals?
tab-legacy-page-page-of-total-pages = Page { $page } of { $total_pages }
tab-legacy-set-to-car-name-skin-name = Set to { $car_name }
    { $skin_name }
    Applied decal: { $applied }
tab-legacy-are-you-sure-you-want-to = Are you sure you want to delete '{ $decal_to_delete }'?
tab-legacy-car-car = Car: { $car }

## patcher/swapper.rs
thumbnail-status-failures-previews-unavailable-items-rema = { $failures } previews unavailable; items remain selectable
active-swaps-replaced-target-name = Replaced { $target_name }
tab-owned-ids-owned-product-ids-captured = { $owned_ids } owned product IDs captured
tab-search-category = Search { $category }...
tab-page-page-of-total-pages-filtered = Page { $page } of { $total_pages }  ({ $filtered } items)
tab-set-as-active = Set as { $active }
spawn-id-item = ID: { $item }

## patcher/wheel_alignment.rs
render-edited-cars-edited = Edited cars ({ $edited })

## ui/workshop.rs
my-slot-problems-of-offers-players-differ = { $problems } of { $offers } players differ
render-remove-the-imported-map-name-its = Remove the imported map '{ $name }'? Its file and image are deleted, and you'd have to import it again.
render-are-you-sure-you-want-to-2 = Are you sure you want to delete '{ $name }' from your downloaded cache?
multiplayer-detected-lan-match-on-map-name = Detected LAN match on map { $name }.
multiplayer-tunnel-if-sent-session-received-session2 = Tunnel: { $state } · sent { $sent } · received { $received }
multiplayer-latest-flow = Latest: { $flow }
peer-install-warning-name-will-be-downloaded-straight-from = { $name } will be downloaded straight from the other player's PC, not from the Hebnix Workshop.
multiplayer-blocked-players-blocked = Blocked players ({ $blocked })
maps-in-use-players-offers = Players ({ $offers })

## ui/workshop/archive.rs
render-shown-of-maps-maps = { $shown } of { $maps } maps
render-by-map = by { $map }
render-imported-map-find-it-under-browse = Imported { $map }. Find it under Browse Maps, in View Downloaded.

## ui/workshop/local_import.rs
render-imported-name = Imported { $name }.
render-file-file = File: { $file }

## app.rs
enable-rlapi-run-hebnix-as-administrator-then-enable = Run Hebnix as administrator, then enable RLAPI capture.
rlapi-enabling-capture = Enabling capture…
handle-messages-no-maps-found = No maps found.
handle-messages-failed-to-load-maps = Failed to load maps.
handle-messages-invalid-api-response-format = Invalid API response format.
execute-command-rocket-league-is-not-running = Rocket League is not running.
execute-command-can-t-read-the-match-the = Can't read the match, the stats api is not answering.
execute-command-not-in-a-game = Not in a game.
execute-command-enabled = Enabled
execute-command-disabled = Disabled
statsapi-notice-quit-hebnix = Quit Hebnix
statsapi-notice-proceed = Proceed
admin-prompt-item-spawner-needs-administrator-access = Item Spawner needs Administrator access to update the hosts file. Hebnix will restart as Administrator.
admin-prompt-this-action-requires-hebnix-to-be = This action requires Hebnix to be run as Administrator.

## dpi_fix.rs

## lite_app.rs
handle-messages-plugin-catalog-returned-an-invalid-respo = Plugin catalog returned an invalid response.
execute-command-unknown = Unknown

## patcher/ball.rs
spawn-restore-thread-no-backups-found-to-restore = No backups found to restore.
spawn-restore-thread-restore-hit-an-unexpected-internal-error = Restore hit an unexpected internal error and was aborted.
spawn-apply-thread-no-matching-inline-ball-mips-were = No matching inline ball mips were found in the current packages.

## patcher/car_patcher.rs
import-zip-no-usable-custom-cars-were-found = No usable custom cars were found. The ZIP must contain a JSON manifest and its matching UPK file.

## patcher/catalog.rs
catalog-loading-catalog = Loading catalog...
catalog-showing-cached-catalog = Showing cached catalog.
poll-no-catalog-items-found = No catalog items found.
poll-failed-to-load-catalog = Failed to load catalog.
render-no-downloaded-items-match-your-search = No downloaded items match your search.
render-no-catalog-items-match-your-search = No catalog items match your search.
card-downloading = Downloading...
card-import = Import

## patcher/decal_patcher.rs
prepare-specific-decal-carrier-preparing-a-full-colour-decal-carrier = Preparing a full-colour decal carrier...
patch-decal-on-skin-loading-upk-keys = Loading UPK keys...
patch-decal-on-skin-finding-decal-textures = Finding decal textures...
patch-decal-on-skin-patching-blankskin = Patching BlankSkin...
patch-decal-on-skin-encrypting-patched-upk = Encrypting patched UPK...
patch-decal-on-skin-decal-patch-complete = Decal patch complete
load-car-skins-the-skins-catalog-has-not-been = The skins catalog has not been downloaded
load-car-skins-no-cars-object-found-in-skins = No 'cars' object found in skins.json
apply-decal-to-skin-this-decal-target-already-has-an = This decal target already has an applied decal; restore it first
restore-decal-from-skin-restoring-original-files = Restoring original files...
tab-no-decals-found-in-the-decals = No decals found in the /decals/ directory.
tab-no-decals-match-your-search = No decals match your search.
tab-no-api-decals-for-this-car = No API decals for this car
tab-decal-to-patch = Decal to patch...
tab-working = Working...

## patcher/swapper.rs
label-antennas = Antennas
label-anthems = Anthems
label-borders = Borders
label-bodies = Bodies
label-boosts = Boosts
label-engines = Engines
label-goals = Goals
label-finishes = Finishes
label-banners = Banners
label-decals = Decals
label-toppers = Toppers
label-trails = Trails
label-wheels = Wheels
tab-no-applied-items-match-the-search = No applied items match the search.
tab-no-items-match-the-search = No items match the search.
tab-replace-with-decal = Replace with decal
tab-replace-item = Replace item
tab-manual-mode = Manual Mode
tab-walkthrough-mode = Walkthrough Mode
tab-walkthrough-find-item = Select an item you want in game
tab-walkthrough-select-owned = Select an item you own and want to replace
tab-walkthrough-selected = Selected: { $item }
tab-walkthrough-back = Back
tab-walkthrough-no-replacements = No compatible items match the search and filters.
tab-walkthrough-applied-swap = Applied Swap

## patcher/wheel_alignment.rs
build-this-car-s-skeleton-is-unsupported = This car's skeleton is unsupported: four standard wheel anchors are required
drop-alignment-reverted-and-removed-from-the = Alignment reverted and removed from the modified list.
drop-alignment-reverted-to-captured-baseline = Alignment reverted to captured baseline; prior swaps/custom placement retained.
drop-alignment-applied-and-verified-test-stee = Alignment applied and verified. Test steering and jumping in-game.
render-alignment-worker-stopped-inspect-backup = Alignment worker stopped; inspect backup before retrying
render-camber-degrees = Camber degrees
render-up-down = Up / Down
render-left-right-track-width = Left / Right (track width)
render-preparing-alignment = Preparing alignment…

## ui/rlapi.rs
rlapi-custom-endpoint = Custom endpoint

## ui/workshop.rs
download-map-file-no-valid-upk-or-udk-map = No valid .upk or .udk map file found inside the downloaded archive.
default-connect-then-host-or-join-inside = Connect, then host or join inside Rocket League's own LAN match screen.
multiplayer-closing-rocket-league-start-it-again = Closing Rocket League. Start it again once it has exited.
multiplayer-disconnected = Disconnected.
multiplayer-run-hebnix-as-administrator-to-start = Run Hebnix as administrator to start the relay.
start-tailnet-setting-up-the-private-workshop-network = Setting up the private Workshop network...
finish-tailnet-started-connected-to-the-private-workshop-networ = Connected to the private Workshop network.
set-tailnet-ip-ready-on-the-private-workshop-network = Ready on the private Workshop network.
launch-multiplayer-the-workshop-network-is-not-ready = The Workshop network is not ready yet.
launch-multiplayer-starting-rocket-league-on-the-workshop = Starting Rocket League on the Workshop network...
finish-multiplayer-launch-rocket-league-is-starting-on-the = Rocket League is starting on the Workshop network.
start-relay-the-workshop-network-is-not-ready = The Workshop network is not ready.
start-relay-starting-the-workshop-lan-relay = Starting the Workshop LAN relay...
finish-relay-started-relaying-host-or-join-from-rocket = Relaying - host or join from Rocket League's own LAN match screen.
shutdown-multiplayer-rocket-league-closed-keeping-the-session = Rocket League closed. Keeping the session open in case it comes back...
tick-shutdown-grace-rocket-league-reconnected = Rocket League reconnected.
tick-shutdown-grace-workshop-multiplayer-stopped-because-roc = Workshop multiplayer stopped because Rocket League closed.

## ui/workshop/archive.rs
download-and-import-unpacking-the-zip = Unpacking the zip...
download-and-import-downloading-the-preview-image = Downloading the preview image...
download-and-import-saving-the-map = Saving the map...

## ui/workshop/background_changer.rs
default-choose-the-arena-you-want-to = Choose the arena you want to play, then choose the fog, sky, and background to borrow.
launch-applying-fog-sky-and-background = Applying fog, sky, and background…
launch-removing-fog-sky-and-background = Removing fog, sky, and background…
launch-restoring-the-original-arena = Restoring the original arena…
launch-restoring-all-original-arenas = Restoring all original arenas…
render-remove-background = Remove Background
render-apply-background = Apply Background

## ui/workshop/steam_download.rs
fetch-manifest-the-hubcap-api-key-was-rejected = The Hubcap API key was rejected. Check it and try again.
download-item-looking-up-the-workshop-item-on = Looking up the workshop item on Steam...
download-item-requesting-the-download-manifest = Requesting the download manifest...
download-item-downloading-the-map = Downloading the map...
start-saving-the-map-its-details-and = Saving the map, its details and image...

## manual additions
admin-prompt-owned-filter-needs-proxy =
    Filtering replacements by ownership needs the Hebnix proxy.
    Hebnix will restart as Administrator, enable the proxy, and build your owned-item catalog.

## patcher/boost_patcher.rs
tab-boost-meter-patcher = Boost Meter Patcher
tab-are-you-sure-you-want-to = Are you sure you want to delete '{ $boost_to_delete }'?
tab-no-boost-meters-found-in-the = No boost meters found in the /boosts/ directory.
tab-no-boost-meters-match-your-search = No boost meters match your search.

## patcher/colours.rs
render-stadium-hud-and-garage-palette-colours = Stadium, HUD and garage palette colours on one page.
render-stadium-colours = Stadium colours
render-apply-stadium-colours = Apply stadium colours
render-banners-flags-and-field-lines = Banners, flags and field lines.
render-defaults = Defaults
render-swap-teams = Swap teams
render-hud-colours = HUD colours
render-apply-hud-colours = Apply HUD colours
render-boost-meter-and-scoreboard = Boost meter and scoreboard.
render-match-stadium = Match stadium
render-extended-colour-palette = Extended colour palette
render-add-pure-white-to-pure-black = Add pure white to pure black to the darkest garage row
render-only-you-see-these-colours-disable = Only you see these colours. Disable this and apply again to put the original palette row back.
render-heatseeker-ball-glow = Heatseeker ball glow
render-override-the-maximum-speed-glow-locally = Override the maximum-speed glow locally
render-changes-the-high-speed-bloom-and = Changes the high-speed bloom and trail only; team colouring is left untouched.
render-pristine-backup-backup-name = Pristine backup: { $backup }
ball-appearance-change-the-ball-s-shape-locally = Change the ball's shape locally while keeping its original physics and hitbox.
ball-appearance-experimental-visual-swap-collision-and-p = Experimental visual swap. Collision and physics are preserved.
ball-appearance-original-ball = Original ball
ball-appearance-replace-appearance-with = Replace appearance with
ball-appearance-include-replacement-colours-experimental = Include replacement colours (experimental)
ball-appearance-copies-the-colour-texture-special-shader = Copies the colour texture; special shaders and effects stay original.
ball-appearance-colours-are-available-for-normal-selecte = Colours are available for Normal → selected balls. This pair swaps shape only.
ball-appearance-applied-super-super2 = Applied: { $original } → { $replacement }
ball-appearance-restore-this-ball-before-choosing-anothe = Restore this ball before choosing another appearance.
render-apply-colours = Apply Colours
ball-appearance-apply-ball-appearance = Apply Ball Appearance
ball-appearance-restore-ball-appearance = Restore Ball Appearance

## colours (manual)
colours-blue-team = Blue team
colours-orange-team = Orange team
colours-max-speed = Max speed
colours-status-choose = Choose the stadium, HUD and garage palette colours, then apply them together.
colours-status-applying = Applying stadium, HUD and garage palette colours…
colours-status-restoring = Restoring original colours…
colours-status-preparing-ball = Preparing and validating visual ball swaps…
colours-status-restoring-ball = Restoring original ball geometry…
colours-status-applied = Colours applied. Start Rocket League to see them.
colours-status-restored = Original stadium, HUD and garage palette colours restored.
colours-status-ball-applied = Ball shape applied. Restart Rocket League to test it.
colours-status-ball-restored = Original ball shape restored.
spoofer-certificate-status = Certificate Status:
spoofer-certificate-required-title = Certificate Required
spoofer-certificate-required-message = The Certificate must be installed first
multiplayer-checking-launch-command = Checking the Rocket League launch command...
maps-none-replaced-yet = You haven't replaced any map yet.

speed-patch-title = Animation Speed Patch
speed-patch-description = Scales the pan and spin speeds of animated skins and decals when their package is written. Static decals are unaffected.
speed-patch-multiplier = Speed
speed-patch-roundtrip-mismatch = Speed patch round-trip mismatch
app-speed-patch = Animation Speed
speed-patch-items-page = Show a speed picker on the Items page
speed-patch-items-page-hint = Adds a speed slider to each decal and swap on the Items page. It is applied with that item and 1x leaves the item unchanged.
speed-patch-oneoff-title = Patch a package
speed-patch-oneoff-hint = Scale the animation speed of one package in CookedPCConsole. A backup is made first and Restore puts it back.
speed-patch-choose = Choose package...
speed-patch-no-package = No package chosen
speed-patch-patch = Patch
speed-patch-outside-cooked = The package must be directly inside { $folder }
speed-patch-nothing = { $file } has no animated parameters to scale.
speed-patch-done = { $file }: scaled { $count } animation values to { $speed }x
speed-patch-no-backup = No backup found for { $file }
speed-patch-restored = Restored { $file }
