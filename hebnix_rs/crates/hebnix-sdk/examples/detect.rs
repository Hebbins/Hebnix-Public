//! diagnostic: what the process detector actually sees.
//! run: cargo run -p hebnix-sdk --example detect

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

fn main() {
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    println!("total processes: {}", sys.processes().len());
    for (pid, p) in sys.processes() {
        let name = p.name().to_string_lossy().to_string();
        if name.to_lowercase().contains("rocket") {
            println!("MATCH pid={pid} name={name:?} exe={:?}", p.exe());
        }
    }
    println!(
        "is_rocket_league_running() = {}",
        hebnix_sdk::process::is_rocket_league_running()
    );
    println!(
        "find_rocket_league() = {:?}",
        hebnix_sdk::process::find_rocket_league()
    );
    println!(
        "is_rocket_league_focused() = {}",
        hebnix_sdk::process::is_rocket_league_focused()
    );
}
