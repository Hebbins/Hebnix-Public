//! diagnostic: live eos token gen for steam + epic.
//! run (steam running + owning RL, and/or epic launcher open):
//!   cargo run -p hebnix-sdk --example eos_test
//! steam path needs steam_api64.dll: set HEBNIX_STEAM_API_DLL or drop the dll
//! next to the built example exe.

use hebnix_sdk::eos::{self, EOSToken, Platform};

fn show(label: &str, token: &Option<EOSToken>) {
    println!("\n=== {label} ===");
    match token {
        Some(t) => {
            let preview: String = t.access_token.chars().take(60).collect();
            println!("access_token : {preview}...");
            println!("account_id   : {}", t.account_id);
            println!("expires_at   : {}", t.expires_at);
            println!("platform     : {}", t.platform);
            println!("expired?     : {}", t.expired());
            println!("refresh_token: {} chars", t.refresh_token.len());
        }
        None => println!("(no token)"),
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    println!("=== EOS token generator (Rust port) ===");

    let steam = eos::get_eos_token(Platform::Steam);
    show("STEAM", &steam);

    let epic = eos::get_eos_token(Platform::Epic);
    show("EPIC", &epic);

    // Exercise refresh + save/load if we got a Steam token.
    if let Some(t) = &steam {
        if !t.refresh_token.is_empty() {
            let refreshed = eos::load_from_refresh(&t.refresh_token);
            show("STEAM (refreshed)", &refreshed);
        }
        let path = std::env::temp_dir().join("EOS_token.txt");
        if eos::save_to(t, &path).is_ok() {
            println!("\nsaved token to {}", path.display());
            let reloaded = eos::load_from(&path);
            println!("reload ok?    : {}", reloaded.is_some());
        }
    }
}
