// build script:
//  - embed a per-monitor-v2 dpi manifest (applies before any window exists,
//    dodges the ambiguous default-dpi mode behind the mixed-dpi drag bugs)
//  - embed hebnix.ico as the exe icon plus a per-bin version block
//  - copy the runtime binaries (steam_api64.dll, rlapi-bridge.exe) and the
//    required runtime assets next to the built exe so a plain build just runs

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=hebnix.rc");
    println!("cargo:rerun-if-changed=assets/hebnix.ico");

    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        use embed_manifest::{embed_manifest, manifest::DpiAwareness, new_manifest};

        embed_manifest(
            new_manifest("Hebnix.HebnixApp")
                .dpi_awareness(DpiAwareness::PerMonitorV2Only),
        )
        .expect("failed to embed application manifest");

        embed_version_resources();
    }

    embed_locales();
    copy_runtime_binaries();
}

/// bakes every locales/<code>/*.ftl into the exe. adding a language is just
/// adding a folder, nothing in the rust code needs to change.
fn embed_locales() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("locales");
    println!("cargo:rerun-if-changed={}", root.display());

    let mut out = String::from("pub const EMBEDDED_FTL: &[(&str, &str)] = &[\n");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();

    for dir in dirs {
        let Some(code) = dir.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        println!("cargo:rerun-if-changed={}", dir.display());
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("ftl"))
            .collect();
        files.sort();
        for file in files {
            println!("cargo:rerun-if-changed={}", file.display());
            out.push_str(&format!(
                "    ({:?}, include_str!({:?})),\n",
                code,
                file.to_string_lossy()
            ));
        }
    }
    out.push_str("];\n");

    let out_path = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("locales_embedded.rs");
    std::fs::write(out_path, out).expect("failed to write locales_embedded.rs");
}

fn embed_version_resources() {
    let version = app_version();

    let mut parts: Vec<u16> = version
        .split(['.', '-', '+'])
        .filter_map(|p| p.parse().ok())
        .collect();

    parts.resize(4, 0);

    for (bin, description) in [
        ("hebnix", "Hebnix"),
        ("hebnix-lite", "Hebnix Lite"),
    ] {
        let rc = write_version_rc(bin, description, &version, &parts);

        embed_resource::compile_for(rc, [bin], embed_resource::NONE)
            .manifest_optional()
            .expect("failed to embed version resource");
    }
}

fn app_version() -> String {
    let app_rs = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("app.rs");

    println!("cargo:rerun-if-changed={}", app_rs.display());

    std::fs::read_to_string(&app_rs)
        .unwrap_or_default()
        .lines()
        .find(|line| {
            line.trim_start()
                .starts_with("pub const APP_VERSION")
        })
        .and_then(|line| line.split('"').nth(1))
        .map(str::to_owned)
        .unwrap_or_else(|| {
            std::env::var("CARGO_PKG_VERSION")
                .unwrap_or_else(|_| "0.0.0".into())
        })
}

fn write_version_rc(
    bin: &str,
    description: &str,
    version: &str,
    parts: &[u16],
) -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let icon = manifest_dir.join("assets").join("hebnix.ico");

    let source =
        std::fs::read_to_string(manifest_dir.join("hebnix.rc"))
            .unwrap_or_default();

    let base = source.replace(
        "\"assets/hebnix.ico\"",
        &format!(
            "\"{}\"",
            icon.display()
                .to_string()
                .replace('\\', "\\\\")
        ),
    );

    let out = PathBuf::from(
        std::env::var_os("OUT_DIR").expect("OUT_DIR"),
    )
    .join(format!("{bin}.rc"));

    let body = format!(
        r#"{base}
1 VERSIONINFO
FILEVERSION {major},{minor},{patch},{build}
PRODUCTVERSION {major},{minor},{patch},{build}
FILEOS 0x40004L
FILETYPE 0x1L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904b0"
        BEGIN
            VALUE "CompanyName", "Hebnix"
            VALUE "FileDescription", "{description}"
            VALUE "FileVersion", "{version}"
            VALUE "InternalName", "{bin}"
            VALUE "OriginalFilename", "{bin}.exe"
            VALUE "ProductName", "Hebnix"
            VALUE "ProductVersion", "{version}"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x409, 1200
    END
END
"#,
        major = parts[0],
        minor = parts[1],
        patch = parts[2],
        build = parts[3],
    );

    std::fs::write(&out, body)
        .expect("failed to write generated version resource");

    out
}

fn copy_runtime_binaries() {
    let Some(profile_dir) = profile_output_dir() else {
        println!(
            "cargo:warning=hebnix: couldn't find build output dir, skipping runtime copy"
        );
        return;
    };

    // crates/hebnix-app -> hebnix_rs -> repo root
    let manifest_dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    let workspace_root =
        manifest_dir.join("..").join("..");

    let repo_root =
        workspace_root.join("..");

    let vendor =
        workspace_root.join("vendor");

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

    let sidecar_dir = workspace_root.join("sidecar");
    for (file_name, missing_hint) in [
        (
            "tailscaled.exe",
            "tailscaled.exe missing from hebnix_rs/sidecar/ - see sidecar/README.md (Workshop LAN multiplayer is unavailable without it)",
        ),
        (
            "tailscale.exe",
            "tailscale.exe missing from hebnix_rs/sidecar/ - see sidecar/README.md (Workshop LAN multiplayer is unavailable without it)",
        ),
        (
            "wintun.dll",
            "wintun.dll missing from hebnix_rs/sidecar/ - see sidecar/README.md (Workshop LAN multiplayer is unavailable without it)",
        ),
        (
            "WinDivert.dll",
            "WinDivert.dll missing from hebnix_rs/sidecar/ - the beacon capture (multiplayer-lan/beacon.rs) needs it next to the exe. Official release: https://github.com/basil00/WinDivert/releases",
        ),
        (
            "WinDivert64.sys",
            "WinDivert64.sys missing from hebnix_rs/sidecar/ - the signed driver WinDivert.dll loads, same source as WinDivert.dll above",
        ),
    ] {
        copy_file(&sidecar_dir.join(file_name), &profile_dir, missing_hint);
    }
}

fn copy_file(
    src: &Path,
    profile_dir: &Path,
    missing_hint: &str,
) {
    println!("cargo:rerun-if-changed={}", src.display());

    if !src.is_file() {
        println!("cargo:warning=hebnix: {missing_hint}");
        return;
    }

    let dst = profile_dir.join(
        src.file_name().unwrap_or_default(),
    );

    if let Err(e) = std::fs::copy(src, &dst) {
        println!(
            "cargo:warning=hebnix: copy {} failed: {e}",
            src.display()
        );
    }
}


// target/<profile>/ derived from
// OUT_DIR = target/<profile>/build/<pkg>/out
fn profile_output_dir() -> Option<PathBuf> {
    let out_dir =
        PathBuf::from(std::env::var_os("OUT_DIR")?);

    out_dir
        .ancestors()
        .nth(3)
        .map(Path::to_path_buf)
}