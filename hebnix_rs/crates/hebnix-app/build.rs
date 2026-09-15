// build script:
//  - embed a per-monitor-v2 dpi manifest (applies before any window exists,
//    dodges the ambiguous default-dpi mode behind the mixed-dpi drag bugs)
//  - embed hebnix.ico as the exe icon
// Runtime support programs are embedded in the executable and extracted to
// %AppData%\Hebnix on startup.

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
}
