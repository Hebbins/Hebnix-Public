use rand::Rng;

pub const PAINTS: [&str; 18] = [
    "Default (no colour)", "Crimson", "Lime", "Black", "Sky Blue", "Cobalt",
    "Burnt Sienna", "Forest Green", "Purple", "Pink", "Orange", "Grey",
    "Titanium White", "Saffron", "Gold", "Rose Gold", "White Gold", "Onyx",
];

#[derive(Debug, Clone)]
pub struct ItemSpawnRequest {
    pub product_id: i64,
    pub series_id: i64,
    pub quality: usize,
    pub paint: usize,
    pub certification: usize,
    pub quantity: usize,
}

pub fn reward_message(request: &ItemSpawnRequest, psy_time: i64) -> Result<(String, Vec<String>), String> {

    let mut products = Vec::with_capacity(request.quantity);
    let mut instance_ids = Vec::with_capacity(request.quantity);
    for _ in 0..request.quantity {
        let instance_id = format!("{:032x}", rand::thread_rng().r#gen::<u128>());
        instance_ids.push(instance_id.clone());
        let mut attributes = vec![serde_json::json!({"Key":"Quality", "Value":request.quality})];

        if request.paint > 0 {
            attributes.push(serde_json::json!({"Key":"Painted", "Value":request.paint}));
        }
        if request.certification > 0 {
            attributes.push(serde_json::json!({"Key":"Certified", "Value":request.certification}));
        }
        products.push(serde_json::json!({
            "AddedTimestamp": psy_time,
            "UpdatedTimestamp": psy_time,
            "InstanceID": instance_id,
            "ProductID": request.product_id,
            "SeriesID": request.series_id,
            "TradeHold": -2,
            "Attributes": attributes

        }));
    }
    let body = serde_json::to_string(&serde_json::json!({"RocketPassInfo":{"TierLevel":0,"bOwnsPremium":false,"XPMultiplier":0.0},"ProductData":products,"RewardDrops":[],"ChallengeRewards":[],"CurrencyDrops":[],"Source":"","MatchGUID":""})).map_err(|e| e.to_string())?;
    let sig = crate::spoofer::rules::psy_response_signature(&psy_time.to_string(), body.as_bytes());
    Ok((format!(
        "PsyService: Reward/RewardResult\r\nPsyServiceVersion: 2\r\nPsyTime: {psy_time}\r\nPsySig: {sig}\r\n\r\n{body}"
    ), instance_ids))
}


/// Instance IDs created by Hebnix. This is separate from Rocket League saves.
pub struct SpawnedItemLedger {
    path: std::path::PathBuf,
    ids: std::sync::Mutex<Vec<String>>,
}

impl SpawnedItemLedger {
    pub fn new(base_dir: &std::path::Path) -> Self {
        let path = base_dir.join("spawned_item_ids.json");
        let read_ids = |path: &std::path::Path| std::fs::read(path).ok()
            .and_then(|bytes| serde_json::from_slice::<Vec<String>>(&bytes).ok());
        let ids = read_ids(&path)
            .or_else(|| read_ids(&path.with_extension("json.bak")))
            .unwrap_or_default()
            .into_iter()
            .filter(|id| id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .collect();
        Self { path, ids: std::sync::Mutex::new(ids) }
    }

    fn save(&self, ids: &[String]) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(ids).map_err(|error| error.to_string())?;
        let temporary = self.path.with_extension(format!("json.{}.tmp", std::process::id()));
        std::fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
        if self.path.is_file() {
            std::fs::copy(&self.path, self.path.with_extension("json.bak"))
                .map_err(|error| error.to_string())?;
            std::fs::remove_file(&self.path).map_err(|error| error.to_string())?;
        }
        std::fs::rename(temporary, &self.path).map_err(|error| error.to_string())
    }

    pub fn record(&self, new_ids: &[String]) -> Result<(), String> {
        let mut ids = self.ids.lock().map_err(|_| "spawned item ledger lock poisoned")?;
        let old_len = ids.len();
        ids.extend(new_ids.iter().cloned());
        if let Err(error) = self.save(&ids) {
            ids.truncate(old_len);
            return Err(error);
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn snapshot(&self) -> Vec<String> {
        self.ids.lock().map(|ids| ids.clone()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reward_ids_match_the_products_sent_to_rocket_league() {
        let request = ItemSpawnRequest {
            product_id: 42, series_id: 1, quality: 0, paint: 0,
            certification: 0, quantity: 2,
        };
        let (message, ids) = reward_message(&request, 1_700_000_000).unwrap();
        let (_, body) = message.split_once("\r\n\r\n").unwrap();
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        let sent_ids = body["ProductData"].as_array().unwrap().iter()
            .map(|product| product["InstanceID"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(sent_ids, ids);
        assert_ne!(ids[0], ids[1]);
    }

    #[test]
    fn ledger_retains_spawned_ids_across_restart() {
        let dir = std::env::temp_dir().join(format!("hebnix-spawn-ledger-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = "00000000000000000000000000000001".to_string();
        let second = "00000000000000000000000000000002".to_string();
        let ledger = SpawnedItemLedger::new(&dir);
        ledger.record(&[first.clone(), second.clone()]).unwrap();
        assert_eq!(SpawnedItemLedger::new(&dir).snapshot(), vec![first, second]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
