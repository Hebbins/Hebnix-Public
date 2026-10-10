//! Count and remove tracked items. No spawning, replay, or match transport.
mod save_cleanup;
mod store;

use std::collections::BTreeSet;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use store::{ItemStore, atomic_save, load};

pub struct ItemStatus {
    pub active: usize,
    pub legacy: usize,
    pub clearing: bool,
    pub can_cancel: bool,
    pub text: String,
}
struct Session {
    // Windows exclusive handles prevent Item Swapper from publishing while clearing.
    _locks: Vec<File>,
    stores: Vec<(PathBuf, ItemStore)>,
}
pub struct Cleaner {
    bases: Vec<PathBuf>,
    session: Mutex<Option<Session>>,
    message: Mutex<String>,
}
impl Cleaner {
    pub fn new(hebnix: &Path) -> Self {
        let mut bases = vec![hebnix.to_owned()];
        if let Some(roaming) = dirs::data_dir() {
            let swapper = roaming.join("Item Swapper").join("data");
            if swapper != hebnix {
                bases.push(swapper);
            }
        }
        Self {
            bases,
            session: Mutex::new(None),
            message: Mutex::new(String::new()),
        }
    }
    pub fn status(&self) -> ItemStatus {
        let mut active = BTreeSet::new();
        let mut legacy = BTreeSet::new();
        let mut removed = BTreeSet::new();
        let mut clearing = self.session.lock().unwrap().is_some();
        let mut can_cancel = true;
        let mut text = self.message.lock().unwrap().clone();
        for base in &self.bases {
            match load(base) {
                Ok(store) => {
                    // Imported copies can remain in Hebnix. A tombstone in either
                    // authoritative store suppresses their stale counts, except
                    // legacy IDs which are still awaiting actual cleanup there.
                    removed.extend(store.tombstones.difference(&store.legacy_ids).cloned());
                    active.extend(store.active.keys().cloned());
                    legacy.extend(store.legacy_ids);
                    clearing |= store.pending_clear;
                    can_cancel &= !store.clear_committed;
                }
                Err(error) => text = format!("Cannot read tracked items: {error}"),
            }
        }
        active.retain(|id| !removed.contains(id));
        legacy.retain(|id| !removed.contains(id));
        ItemStatus {
            active: active.len(),
            legacy: legacy.len(),
            clearing,
            can_cancel,
            text,
        }
    }
    fn acquire(&self) -> Result<Session, String> {
        let mut locks = Vec::new();
        let mut stores = Vec::new();
        // Lock all destinations before reading or modifying any store.
        for base in &self.bases {
            std::fs::create_dir_all(base).map_err(|e| e.to_string())?;
            let mut options = std::fs::OpenOptions::new();
            options.read(true).write(true).create(true).truncate(false);
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                options.share_mode(0);
            }
            locks.push(options.open(base.join("items.lock")).map_err(|_| {
                "Close Item Spawner before removing items, then try again.".to_string()
            })?);
        }
        for base in &self.bases {
            stores.push((base.join("synthetic_items.json"), load(base)?));
        }
        Ok(Session {
            _locks: locks,
            stores,
        })
    }
    pub fn begin_clear(&self) -> Result<(), String> {
        let mut slot = self.session.lock().map_err(|_| "Cleanup lock poisoned")?;
        if slot.is_none() {
            *slot = Some(self.acquire()?);
        }
        for (path, store) in &mut slot.as_mut().unwrap().stores {
            store.pending_clear = true;
            atomic_save(path, store)?;
        }
        *self.message.lock().unwrap() = "Close Rocket League to remove tracked items.".into();
        Ok(())
    }
    pub fn cancel_clear(&self) -> Result<(), String> {
        let mut slot = self.session.lock().map_err(|_| "Cleanup lock poisoned")?;
        if slot.is_none() {
            *slot = Some(self.acquire()?);
        }
        if slot
            .as_ref()
            .unwrap()
            .stores
            .iter()
            .any(|(_, s)| s.clear_committed)
        {
            return Err(
                "Items already removed from the store; Check Again to finish save cleanup.".into(),
            );
        }
        for (path, store) in &mut slot.as_mut().unwrap().stores {
            store.pending_clear = false;
            store.generation = store
                .generation
                .checked_add(1)
                .ok_or("Generation exhausted")?;
            atomic_save(path, store)?;
        }
        *slot = None;
        *self.message.lock().unwrap() = "Removal cancelled; tracked items retained.".into();
        Ok(())
    }
    pub fn finish_clear(&self) -> Result<bool, String> {
        self.finish_with(
            || Ok(!hebnix_sdk::process::detector::rocket_league_running_checked()?),
            save_cleanup::clear_saved_instances,
        )
    }
    fn finish_with(
        &self,
        mut closed: impl FnMut() -> Result<bool, String>,
        cleanup: impl FnOnce(&BTreeSet<String>, u64) -> Result<String, String>,
    ) -> Result<bool, String> {
        let mut slot = self.session.lock().map_err(|_| "Cleanup lock poisoned")?;
        if slot.is_none() {
            *slot = Some(self.acquire()?);
        }
        let session = slot.as_mut().unwrap();
        if !session.stores.iter().any(|(_, s)| s.pending_clear) {
            return Err("Begin removal first.".into());
        }
        if !closed()? {
            return Ok(false);
        }
        let mut targets = BTreeSet::new();
        let mut generation = 0;
        for (path, store) in &mut session.stores {
            store.pending_clear = true;
            store.clear_committed = true;
            store.generation = store
                .generation
                .checked_add(1)
                .ok_or("Generation exhausted")?;
            store.tombstones.extend(store.active.keys().cloned());
            store.tombstones.extend(store.legacy_ids.iter().cloned());
            store.active.clear();
            store.legacy_ids.clear();
            atomic_save(path, store)?;
            targets.extend(store.tombstones.iter().cloned());
            generation = generation.max(store.generation);
        }
        if !closed()? {
            return Ok(false);
        }
        let summary = cleanup(&targets, generation)?;
        if !closed()? {
            return Ok(false);
        }
        for (path, store) in &mut session.stores {
            store.pending_clear = false;
            store.clear_committed = false;
            atomic_save(path, store)?;
        }
        // If the game reopened during the final writes, retain the durable block.
        match closed() {
            Ok(true) => {}
            result => {
                for (path, store) in &mut session.stores {
                    store.pending_clear = true;
                    store.clear_committed = true;
                    atomic_save(path, store)?;
                }
                return result.map(|_| false);
            }
        }
        *slot = None;
        *self.message.lock().unwrap() = format!("Tracked items removed. {summary}");
        Ok(true)
    }
}

