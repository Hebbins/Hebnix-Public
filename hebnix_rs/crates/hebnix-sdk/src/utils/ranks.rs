//! rank helpers.

use crate::utils::constants::{DIVISIONS, RANK_TIERS};

/// rank name (e.g. "Diamond I") to tier_id (0-22)
pub fn get_tier_id(rank_name: &str) -> usize {
    RANK_TIERS.iter().position(|t| *t == rank_name).unwrap_or(0)
}

/// division name (e.g. "Division III") to id (1-4), 0 if unknown
pub fn get_div_id(div_name: &str) -> u32 {
    DIVISIONS
        .iter()
        .find(|(n, _)| *n == div_name)
        .map(|(_, id)| *id)
        .unwrap_or(0)
}

/// tier_id to display name
pub fn get_tier_name(tier_id: usize) -> &'static str {
    RANK_TIERS.get(tier_id).copied().unwrap_or("Unranked")
}

/// division id (1-4) to display name
pub fn get_div_name(div_id: u32) -> &'static str {
    DIVISIONS
        .iter()
        .find(|(_, id)| *id == div_id)
        .map(|(n, _)| *n)
        .unwrap_or("Division I")
}

/// shorten a rank for compact display. "Grand Champion II"=GC2,
/// "Diamond III"=D3, "Supersonic Legend"=SSL.
pub fn shorten_rank(rank_str: &str) -> String {
    let s = rank_str.trim();
    if s.is_empty() {
        return "Unranked".to_string();
    }
    let lower = s.to_lowercase();
    if lower == "supersonic legend" {
        return "SSL".to_string();
    }
    if lower == "unranked" {
        return "Unranked".to_string();
    }
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() >= 2 {
        let last = parts[parts.len() - 1].to_uppercase();
        let num = match last.as_str() {
            "I" => "1",
            "II" => "2",
            "III" => "3",
            other => other,
        };
        if s.contains("Grand Champion") {
            return format!("GC{num}");
        }
        let first_char = parts[0].chars().next().unwrap_or('?').to_uppercase();
        return format!("{first_char}{num}");
    }
    s.to_string()
}

/// sortable key (higher = better): (tier_id, div_id)
pub fn rank_sort_key(tier_id: i64, div_id: i64) -> (i64, i64) {
    (tier_id, div_id)
}

/// division icon color set for a tier: 1=bronze 2=silver 3=gold 4=platinum
/// 5=diamond 6=champion 7=gc/ssl.
pub fn get_div_color_id(tier_id: i64) -> u32 {
    match tier_id {
        t if t <= 0 => 7,
        1..=3 => 1,
        4..=6 => 2,
        7..=9 => 3,
        10..=12 => 4,
        13..=15 => 5,
        16..=18 => 6,
        _ => 7,
    }
}
