//! One coordinator for durable state and all synthetic publication. No account API writes.
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;

const SCHEMA: u32 = 1;
const PROVENANCE: &str = "Hebnix.local.synthetic";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyntheticItem {
    pub provenance: String,
    pub created_at: i64,
    pub scope: String,
    pub product: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ItemStore {
    pub schema_version: u32,
    pub generation: u64,
    pub pending_clear: bool,
    #[serde(default)]
    pub clear_committed: bool,
    pub active: BTreeMap<String, SyntheticItem>,
    pub tombstones: BTreeSet<String>,
    pub legacy_ids: BTreeSet<String>,
}
impl Default for ItemStore {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA,
            generation: 0,
            pending_clear: false,
            clear_committed: false,
            active: BTreeMap::new(),
            tombstones: BTreeSet::new(),
            legacy_ids: BTreeSet::new(),
        }
    }
}

pub fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}
pub fn valid_scope(scope: &str) -> bool {
    let fields: Vec<_> = scope.split('|').collect();
    matches!(fields.as_slice(), ["Steam", id, "0"] if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
        || matches!(fields.as_slice(), ["Epic", id, "0"] if valid_id(id))
}
fn validate(store: &mut ItemStore) -> Result<(), String> {
    if store.schema_version != SCHEMA {
        return Err("Unsupported item store schema".into());
    }
    if store.clear_committed
        && (!store.pending_clear || !store.active.is_empty() || !store.legacy_ids.is_empty())
    {
        return Err("Inconsistent committed-clear state; restoration blocked".into());
    }
    // Canonicalize cleanup metadata; never reconstruct products from it.
    store.tombstones = store
        .tombstones
        .iter()
        .filter(|id| valid_id(id))
        .map(|id| id.to_ascii_lowercase())
        .collect();
    store.legacy_ids = store
        .legacy_ids
        .iter()
        .filter(|id| valid_id(id))
        .map(|id| id.to_ascii_lowercase())
        .collect();
    store.tombstones.extend(store.legacy_ids.iter().cloned());
    let mut active = BTreeMap::new();
    for (id, item) in &store.active {
        let canonical = id.to_ascii_lowercase();
        let p = &item.product;
        if !valid_id(id)
            || item.provenance != PROVENANCE
            || (!item.scope.is_empty() && !valid_scope(&item.scope))
            || p["InstanceID"]
                .as_str()
                .map(str::to_ascii_lowercase)
                .as_deref()
                != Some(&canonical)
            || !p["ProductID"].is_i64()
            || !p["SeriesID"].is_i64()
            || !p["Attributes"].is_array()
            || !p["AddedTimestamp"].is_i64()
            || !p["UpdatedTimestamp"].is_i64()
            || !p["TradeHold"].is_i64()
        {
            return Err("Invalid synthetic product in item store; restoration blocked".into());
        }
        if !store.tombstones.contains(&canonical) {
            let mut item = item.clone();
            item.product["InstanceID"] = json!(canonical);
            if active.insert(canonical, item).is_some() {
                return Err("Conflicting duplicate item IDs; restoration blocked".into());
            }
        }
    }
    store.active = active;
    Ok(())
}

pub(super) fn atomic_save(path: &Path, value: &ItemStore) -> Result<(), String> {
    let parent = path.parent().ok_or("Item store has no parent directory")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let temporary = path.with_extension(format!("{}.tmp", rand::thread_rng().r#gen::<u128>()));
    let result = (|| {
        let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        drop(file);
        if path.exists() {
            let backup = path.with_extension("json.bak");
            std::fs::copy(path, &backup).map_err(|e| e.to_string())?;
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(backup)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
        }
        replace(&temporary, path)
    })();
    let _ = std::fs::remove_file(temporary);
    result
}

#[cfg(windows)]
pub(super) fn replace(source: &Path, target: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    use windows::core::PCWSTR;
    let source: Vec<_> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<_> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(target.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(|e| e.to_string())
}
#[cfg(not(windows))]
pub(super) fn replace(source: &Path, target: &Path) -> Result<(), String> {
    std::fs::rename(source, target).map_err(|e| e.to_string())?;
    std::fs::File::open(target.parent().unwrap())
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())
}

pub(super) fn load(base: &Path) -> Result<ItemStore, String> {
    let path = base.join("synthetic_items.json");
    let mut value = if path.exists() {
        serde_json::from_slice::<ItemStore>(&std::fs::read(&path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?
    } else {
        if path.with_extension("json.bak").exists() {
            return Err("Missing authoritative store; preserve backup for recovery".into());
        }
        ItemStore::default()
    };
    // Legacy tracking remains relevant for removal even after a primary store exists.
    for name in ["spawned_item_ids.json", "spawned_item_ids.json.bak"] {
        let legacy = base.join(name);
        if legacy.exists() {
            let ids: Vec<String> =
                serde_json::from_slice(&std::fs::read(legacy).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            if !value.clear_committed {
                value.legacy_ids.extend(
                    ids.into_iter()
                        .filter(|id| valid_id(id))
                        .map(|id| id.to_ascii_lowercase())
                        .filter(|id| !value.tombstones.contains(id)),
                );
            }
        }
    }
    validate(&mut value)?;
    Ok(value)
}
