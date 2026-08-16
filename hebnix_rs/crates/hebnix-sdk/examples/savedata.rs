//! diagnostic: the .save and what the accessors make of it.
//! run: cargo run -p hebnix-sdk --example savedata
//! --raw dumps one full sample of every object type, use it after an RL patch.

use std::collections::BTreeMap;

fn raw_dump(path: &std::path::Path) {
    let raw = hebnix_sdk::save_file::parse_savedata(path, false).expect("parse failed");
    println!("objects: {}", raw.objects.len());
    println!("\n-- root property stream --");
    println!(
        "{}",
        serde_json::to_string_pretty(&raw.properties).unwrap_or_default()
    );

    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut first: BTreeMap<String, &serde_json::Value> = BTreeMap::new();
    for o in &raw.objects {
        let t = o
            .get("__type")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        *counts.entry(t.clone()).or_insert(0) += 1;
        first.entry(t).or_insert(o);
    }
    for (t, o) in &first {
        println!("\n-- {t} (x{}) --", counts[t]);
        println!("{}", serde_json::to_string_pretty(o).unwrap_or_default());
    }
}

fn main() {
    let Some(path) = hebnix_sdk::save_file::find_save_file(None) else {
        println!("no .save file found");
        return;
    };
    println!("save: {}", path.display());

    if std::env::args().any(|a| a == "--raw") {
        raw_dump(&path);
        return;
    }

    let save = match hebnix_sdk::save_file::load(&path, true) {
        Ok(s) => s,
        Err(e) => {
            println!("parse failed: {e}");
            return;
        }
    };
    println!(
        "engine {} licensee {}",
        save.header.engine_version, save.header.licensee_version
    );
    println!("objects: {}", save.objects.len());

    if let Some(p) = save.profile() {
        println!(
            "\nprofile      : {} (title {})",
            p.profile_name, p.player_title
        );
    }
    if let Some(club) = save.club_id() {
        println!("club id      : {club}");
    }
    if let Some(xp) = save.xp() {
        println!(
            "level        : {} ({} xp into level, {} total)",
            xp.level, xp.xp, xp.total_xp
        );
    }

    match hebnix_sdk::utils::system_settings::read(None) {
        Some(s) => println!(
            "\nwindow mode  : {} (ini, live) {}x{}",
            s.window_mode.as_str(),
            s.res_width,
            s.res_height
        ),
        None => println!("\nwindow mode  : TASystemSettings.ini not found"),
    }

    if let Some(v) = save.video() {
        println!(
            "window mode  : {} (save, stale until exit)",
            v.window_mode.as_str()
        );
        println!(
            "resolution   : {} ({}x{})",
            v.resolution, v.res_width, v.res_height
        );
        println!("max fps      : {}", v.max_fps);
        for (k, val) in &v.options {
            println!("  {k:<14} {val}");
        }
    }

    if let Some(c) = save.camera() {
        println!(
            "\ncamera       : fov {} height {} angle {} dist {} stiff {} swivel {} transition {}",
            c.fov, c.height, c.angle, c.distance, c.stiffness, c.swivel_speed, c.transition_speed
        );
        println!("ball cam dflt: {}", c.prefers_secondary_camera);
    }
    if let Some(g) = save.gameplay() {
        println!(
            "gamepad      : deadzone {} dodge {} steer {} aerial {}",
            g.controller_deadzone, g.dodge_deadzone, g.steering_sensitivity, g.aerial_sensitivity
        );
    }
    if let Some(ff) = save.force_feedback_scale() {
        println!("vibration    : {ff}");
    }
    if let Some(d) = save.gameplay_display() {
        println!(
            "fx/colours   : {} / {}",
            d.effect_intensity, d.stat_event_display_level
        );
    }
    if let Some(s) = save.sound() {
        println!(
            "volumes      : master {} sound {} music {} ambient {} crowd {}",
            s.master_volume, s.sound_volume, s.music_volume, s.ambient_volume, s.crowd_volume
        );
    }
    if let Some(v) = save.voice() {
        println!(
            "voice        : ptt {} output {}",
            v.push_to_talk, v.output_volume
        );
    }
    if let Some(m) = save.matchmaking() {
        println!(
            "matchmaking  : {:?} in {:?}",
            m.quick_match_playlists, m.quick_match_regions
        );
    }

    let skills = save.skills();
    if !skills.is_empty() {
        println!("\nskills:");
        let mut ids: Vec<&i64> = skills.keys().collect();
        ids.sort();
        for id in ids {
            let s = &skills[id];
            println!(
                "  playlist {:<4} {:<20} {} matches",
                s.playlist_id,
                hebnix_sdk::utils::get_tier_name(s.tier.max(0) as usize),
                s.matches_played
            );
        }
    }

    if let Some(set) = save.equipped_loadout_set() {
        println!("\nequipped set : {}", set.name);
        println!("  blue slots : {:?}", set.blue.products);
        println!(
            "  paint      : team {} custom {}",
            set.blue.team_paint.team_color_id, set.blue.team_paint.custom_color_id
        );
    }
    if let Some(b) = save.banner() {
        println!(
            "banner       : product {} colour {}",
            b.product_id, b.selected_color
        );
    }
    if let Some(b) = save.avatar_border() {
        println!(
            "border       : product {} colour {}",
            b.product_id, b.selected_color
        );
    }

    println!("\ninventory    : {} items", save.inventory().len());
    println!("favourites   : {}", save.favorite_instance_ids().len());
    println!("loadout sets : {}", save.loadout_sets().len());
    println!("quick chats  : {}", save.quick_chats().len());
    println!(
        "recent       : {} players, {} game ids",
        save.recent_players().len(),
        save.recent_game_ids().len()
    );
    println!("observed     : {} players", save.observed_players().len());
    println!("ui values    : {}", save.ui_values().len());

    let notifs = save.notifications();
    println!("notifications: {}", notifs.len());
    if let Some(n) = notifs.first() {
        println!(
            "  first      : {} \"{}\" shown {}",
            n.notification_id, n.title, n.pop_up_shown
        );
    }

    if let Some(stats) = save.stats() {
        println!(
            "\nstats ({} ids, {} product counters):",
            stats.stats.len(),
            stats.product_stats.len()
        );
        for id in [
            "Win",
            "Loss",
            "Goal",
            "Assist",
            "Save",
            "Shot",
            "MVP",
            "Demolish",
            "TimePlayed",
        ] {
            println!("  {id:<12} {:?}", stats.values(id));
        }
        println!("  all ids: {}", stats.stat_ids().join(" "));
    }

    if let Some(a) = save.achievements() {
        println!("\nachievements (named lifetime totals):");
        println!(
            "  matches      {} played, {} won, best streak {}",
            a.game_events_played, a.game_events_won, a.games_won_in_a_row
        );
        println!(
            "  ranked {} unranked {} private {} exhibition {}",
            a.ranked_matches_played,
            a.unranked_matches_played,
            a.private_matches_played,
            a.exhibition_matches_played
        );
        println!(
            "  goal saves {} shots on goal {} best mvp score {}",
            a.goal_saves, a.goal_shots_any, a.highest_mvp_score
        );
        println!(
            "  driven {:.0} km, boost {:.0}s, on wall {:.0}s",
            a.total_drive_distance_km, a.total_boost_time, a.total_time_on_wall
        );
        println!(
            "  {} maps played, {} cars used, {} cars collected",
            a.levels_played.len(),
            a.cars_played.len(),
            a.cars_collected.len()
        );
    }

    let packs = save.training_packs();
    if !packs.is_empty() {
        println!("training packs: {} tracked", packs.len());
    }

    if let Some(prefs) = save.map_prefs() {
        println!(
            "\nmap prefs    : {} liked, {} disliked",
            prefs.liked().len(),
            prefs.disliked().len()
        );
        if let Some(m) = &prefs.selected_freeplay_map {
            println!("freeplay map : {m}");
        }
    }
}
