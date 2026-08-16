//! shared constants.

/// rank tiers (index = tier_id)
pub const RANK_TIERS: [&str; 23] = [
    "Unranked",
    "Bronze I",
    "Bronze II",
    "Bronze III",
    "Silver I",
    "Silver II",
    "Silver III",
    "Gold I",
    "Gold II",
    "Gold III",
    "Platinum I",
    "Platinum II",
    "Platinum III",
    "Diamond I",
    "Diamond II",
    "Diamond III",
    "Champion I",
    "Champion II",
    "Champion III",
    "Grand Champion I",
    "Grand Champion II",
    "Grand Champion III",
    "Supersonic Legend",
];

/// division names (1-based)
pub const DIVISIONS: [(&str, u32); 4] = [
    ("Division I", 1),
    ("Division II", 2),
    ("Division III", 3),
    ("Division IV", 4),
];

/// platform slugs for tracker.gg
pub const PLATFORM_SLUGS: [(&str, &str); 5] = [
    ("steam", "steam"),
    ("epic", "epic"),
    ("xboxone", "xbl"),
    ("ps4", "psn"),
    ("switch", "switch"),
];

/// short display tags per platform
pub const PLATFORM_TAGS: [(&str, &str); 5] = [
    ("steam", "[Steam]"),
    ("epic", "[Epic]"),
    ("xboxone", "[Xbox]"),
    ("ps4", "[PSN]"),
    ("switch", "[Switch]"),
];

/// save paths relative to Documents
pub const SAVE_PATH_STEAM: &str = "My Games/Rocket League/TAGame/SaveData/DBE_Production";
pub const SAVE_PATH_EPIC: &str = "My Games/Rocket League/TAGame/SaveDataEpic/DBE_Production";
