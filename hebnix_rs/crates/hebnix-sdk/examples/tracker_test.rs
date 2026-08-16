//! diagnostic: live tracker.gg fetch (needs the bundled curl-impersonate).
//! run: cargo run -p hebnix-sdk --example tracker_test

use hebnix_sdk::tracker::TrackerClient;

fn main() {
    let client = TrackerClient::default();
    match client.fetch("Steam|76561197960287930|0", "Rabscuttle") {
        Ok(stats) => {
            println!("handle   = {}", stats.platform_user_handle);
            println!("error    = {:?}", stats.error);
            println!("ranks    = {}", stats.ranks.len());
            println!("season   = {}", stats.current_season);
            if let Some(lifetime) = &stats.lifetime {
                println!("wins     = {}", lifetime.wins);
            }
        }
        Err(e) => println!("fetch refused: {e}"),
    }

    // direct platform+identifier lookup (the hebnix.fetch_profile_async path)
    match client.fetch_profile("steam", "76561198838703744") {
        Ok(stats) => {
            println!("--- fetch_profile(steam, 76561198838703744) ---");
            println!("handle   = {}", stats.platform_user_handle);
            println!("error    = {:?}", stats.error);
            println!("ranks    = {}", stats.ranks.len());
        }
        Err(e) => println!("fetch_profile refused: {e}"),
    }
}
