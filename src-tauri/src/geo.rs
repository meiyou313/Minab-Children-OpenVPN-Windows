//! Country lookup for a server IP via ip-api.com, with a 30-day disk cache.
//!
//! Must only be called from a worker thread — it performs blocking network
//! I/O and must never run on the UI thread.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CACHE_FILE: &str = "geo_cache.json";
const CACHE_TTL_SECS: u64 = 30 * 24 * 3600; // 30 days
const LOOKUP_URL: &str = "http://ip-api.com/json";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    code: String, // ISO 3166-1 alpha-2, e.g. "DE"
    at: u64,      // unix seconds when cached
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn load_cache(cache_dir: &Path) -> HashMap<String, CacheEntry> {
    let path = cache_dir.join(CACHE_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return HashMap::new(),
    };
    serde_json::from_str::<HashMap<String, CacheEntry>>(&text).unwrap_or_default()
}

fn save_cache(cache_dir: &Path, cache: &HashMap<String, CacheEntry>) {
    if std::fs::create_dir_all(cache_dir).is_err() {
        return;
    }
    if let Ok(text) = serde_json::to_string_pretty(cache) {
        let _ = std::fs::write(cache_dir.join(CACHE_FILE), text);
    }
}

/// 2-letter country code for an IP (or hostname — ip-api.com resolves it),
/// or `None` when unknown / offline. Cached 30 days in `geo_cache.json`.
pub fn country_code_for_ip(ip: &str, cache_dir: &Path) -> Option<String> {
    let ip = ip.trim();
    if ip.is_empty() {
        return None;
    }
    let mut cache = load_cache(cache_dir);
    if let Some(entry) = cache.get(ip) {
        if now_unix().saturating_sub(entry.at) < CACHE_TTL_SECS {
            return Some(entry.code.clone());
        }
    }
    let code = fetch_country_code(ip)?;
    cache.insert(
        ip.to_string(),
        CacheEntry {
            code: code.clone(),
            at: now_unix(),
        },
    );
    save_cache(cache_dir, &cache);
    Some(code)
}

fn fetch_country_code(ip: &str) -> Option<String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(2))
        .timeout_read(Duration::from_secs(2))
        .timeout_write(Duration::from_secs(2))
        .build();
    let url = format!("{LOOKUP_URL}/{ip}?fields=countryCode");
    let body = agent.get(&url).call().ok()?.into_string().ok()?;
    let value: serde_json::Value = serde_json::from_str(&body).ok()?;
    let code = value.get("countryCode")?.as_str()?;
    if code.len() == 2 && code.chars().all(|c| c.is_ascii_alphabetic()) {
        Some(code.to_ascii_uppercase())
    } else {
        None
    }
}
