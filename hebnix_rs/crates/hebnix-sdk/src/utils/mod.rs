//! shared helpers: ranks, platform id parsing, psynet config api, game ini.

pub mod constants;
pub mod platforms;
pub mod psynet;
pub mod ranks;
pub mod system_settings;

pub use constants::{
    DIVISIONS, PLATFORM_SLUGS, PLATFORM_TAGS, RANK_TIERS, SAVE_PATH_EPIC, SAVE_PATH_STEAM,
};
pub use platforms::{get_platform_slug, get_platform_tag, is_bot, parse_primary_id};
pub use ranks::{
    get_div_color_id, get_div_id, get_div_name, get_tier_id, get_tier_name, rank_sort_key,
    shorten_rank,
};
