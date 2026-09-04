// build script:
//  - embed a per-monitor-v2 dpi manifest (applies before any window exists,
//    dodges the ambiguous default-dpi mode behind the mixed-dpi drag bugs)
//  - embed hebnix.ico as the exe icon
//  - copy the runtime binaries (steam_api64.dll, rlapi-bridge.exe, the
//    required runtime assets next to the built exe so a plain build just runs

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=hebnix.rc");
    println!("cargo:rerun-if-changed=assets/hebnix.ico");

    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        use embed_manifest::{embed_manifest, manifest::DpiAwareness, new_manifest};
        embed_manifest(
            new_manifest("Hebnix.HebnixApp").dpi_awareness(DpiAwareness::PerMonitorV2Only),
        )
        .expect("failed to embed application manifest");

        // icon only (manifest handled above), so no double-manifest clash
        embed_resource::compile("hebnix.rc", embed_resource::NONE)
            .manifest_optional()
            .expect("failed to embed icon resource");
    }

    copy_runtime_binaries();
}

fn copy_runtime_binaries() {
    let Some(profile_dir) = profile_output_dir() else {
        println!("cargo:warning=hebnix: couldn't find build output dir, skipping runtime copy");
        return;
    };

    // crates/hebnix-app -> hebnix_rs -> repo root
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.join("..").join("..");
    let repo_root = workspace_root.join("..");
    let vendor = workspace_root.join("vendor");

    copy_file(
        &vendor.join("steam_api64.dll"),
        &profile_dir,
        "steam_api64.dll missing from hebnix_rs/vendor/ - restore it",
    );
    copy_file(
        &repo_root
            .join("rlapi_bridge")
            .join("dist")
            .join("rlapi-bridge.exe"),
        &profile_dir,
        "rlapi-bridge.exe missing - run rlapi_bridge/build.bat (RLAPI off until then)",
    );
    copy_tree(
        &manifest_dir
            .join("src")
            .join("multiplayer-lan")
            .join("tap-driver"),
        &profile_dir.join("tap-driver"),
        "tap-driver/ missing - Workshop LAN is unavailable",
    );
}

fn copy_file(src: &Path, profile_dir: &Path, missing_hint: &str) {
    println!("cargo:rerun-if-changed={}", src.display());
    if !src.is_file() {
        println!("cargo:warning=hebnix: {missing_hint}");
        return;
    }
    let dst = profile_dir.join(src.file_name().unwrap_or_default());
    if let Err(e) = std::fs::copy(src, &dst) {
        println!("cargo:warning=hebnix: copy {} failed: {e}", src.display());
    }
}

fn copy_tree(src: &Path, dst: &Path, missing_hint: &str) {
    if !src.is_dir() {
        println!("cargo:warning=hebnix: {missing_hint}");
        return;
    }
    let Ok(entries) = std::fs::read_dir(src) else {
        return;
    };
    let _ = std::fs::create_dir_all(dst);
    for entry in entries.flatten() {
        let path = entry.path();
        let destination = dst.join(entry.file_name());
        if path.is_dir() {
            copy_tree(&path, &destination, missing_hint);
        } else if path.is_file() {
            println!("cargo:rerun-if-changed={}", path.display());
            let _ = std::fs::copy(&path, destination);
        }
    }
}

// target/<profile>/ derived from OUT_DIR = target/<profile>/build/<pkg>/out
fn profile_output_dir() -> Option<PathBuf> {
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR")?);
    out_dir.ancestors().nth(3).map(Path::to_path_buf)
}
