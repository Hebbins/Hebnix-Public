//! platform id helpers for PrimaryId strings.

use crate::utils::constants::{PLATFORM_SLUGS, PLATFORM_TAGS};

/// is this PrimaryId a bot: empty, contains "unknown", or no "|"
pub fn is_bot(primary_id: &str) -> bool {
    if primary_id.is_empty() {
        return true;
    }
    if primary_id.to_lowercase().contains("unknown") {
        return true;
    }
    !primary_id.contains('|')
}

/// parse a PrimaryId into (platform, user_id, splitscreen_id). e.g.
/// "Steam|123456789|0" gives ("steam","123456789","0"). None if unrecognised.
pub fn parse_primary_id(primary_id: &str) -> Option<(String, String, String)> {
    if is_bot(primary_id) {
        return None;
    }
    let parts: Vec<&str> = primary_id.split('|').collect();
    if parts.len() < 2 {
        return None;
    }
    let platform = parts[0].to_lowercase();
    let uid = parts[1].to_string();
    let ss = parts.get(2).unwrap_or(&"0").to_string();
    Some((platform, uid, ss))
}

/// short display tag: "[Steam]", "[BOT]", "[?]"
pub fn get_platform_tag(primary_id: &str) -> &'static str {
    if is_bot(primary_id) {
        return "[BOT]";
    }
    let plat = primary_id.split('|').next().unwrap_or("").to_lowercase();
    PLATFORM_TAGS
        .iter()
        .find(|(k, _)| *k == plat)
        .map(|(_, v)| *v)
        .unwrap_or("[?]")
}

/// tracker.gg api slug for a PrimaryId's platform ("epic" fallback)
pub fn get_platform_slug(primary_id: &str) -> &'static str {
    if is_bot(primary_id) {
        return "epic";
    }
    let plat = primary_id.split('|').next().unwrap_or("").to_lowercase();
    PLATFORM_SLUGS
        .iter()
        .find(|(k, _)| *k == plat)
        .map(|(_, v)| *v)
        .unwrap_or("epic")
}
