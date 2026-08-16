//! diagnostic: auth to psynet via the rlapi bridge + make a few live requests.
//! needs rlapi-bridge.exe (HEBNIX_RLAPI_BRIDGE or next to the example) and, for
//! steam, steam_api64.dll (HEBNIX_STEAM_API_DLL) with steam running + owning RL.
//!   cargo run -p hebnix-sdk --example rlapi_test

use hebnix_sdk::eos::{self, Platform};
use hebnix_sdk::rlapi::{self, RlApi};

fn preview(v: &serde_json::Value) -> String {
    let s = serde_json::to_string(v).unwrap_or_default();
    s.chars().take(400).collect()
}

fn run(platform: Platform) {
    println!("\n================ {} ================", platform.as_str());

    let Some(token) = eos::get_eos_token(platform) else {
        println!("(no EOS token, skipping)");
        return;
    };
    println!("EOS account_id = {}", token.account_id);
    if !token.steam_id.is_empty() {
        println!("EOS steam_id   = {}", token.steam_id);
    }

    let mut api = match RlApi::connect_with_token(&token, platform) {
        Ok(api) => api,
        Err(e) => {
            println!("connect failed: {e}");
            return;
        }
    };
    println!("ping           = {}", api.ping());

    // Our own PlayerID for the authenticated platform.
    let self_id = match platform {
        Platform::Steam => rlapi::player_id(platform, &token.steam_id),
        Platform::Epic => rlapi::player_id(platform, &token.account_id),
    };
    println!("self PlayerID  = {self_id}");

    match api.get_player_skill(&self_id) {
        Ok(v) => println!("GetPlayerSkill = {}", preview(&v)),
        Err(e) => println!("GetPlayerSkill failed: {e}"),
    }
    match api.get_population() {
        Ok(v) => println!("GetPopulation  = {}", preview(&v)),
        Err(e) => println!("GetPopulation failed: {e}"),
    }

    api.close();
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    println!("=== RLAPI bridge test ===");
    run(Platform::Steam);
    run(Platform::Epic);
}
