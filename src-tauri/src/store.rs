//! Profile + usage persistence (JSON files in the Tauri app-data dir).
//!
//! SECURITY NOTE (first pass, same limitation as the Android full-backup
//! file): profile passwords are stored in **plaintext** in the local
//! `profiles.json`. That is acceptable for a v0.1 local prototype but must
//! be replaced by Windows Credential Manager storage in a later pass
//! before any wide distribution.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Time helpers (no chrono dependency — std only)
// ---------------------------------------------------------------------------

/// Current UTC time as ISO 8601, e.g. `2026-11-24T06:54:46Z`.
pub fn now_iso_utc() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86400) as i64;
    let secs_of_day = secs % 86400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60
    )
}

/// `YYYY-MM-DD` (UTC) for today.
pub fn today_date_string() -> String {
    now_iso_utc()[..10].to_string()
}

/// `YYYY-MM-DD` (UTC) for `days_ago` days before today.
pub fn date_string_days_ago(days_ago: u64) -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86400) as i64 - days_ago as i64;
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Howard Hinnant's civil-from-days algorithm (days since 1970-01-01).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ---------------------------------------------------------------------------
// Profiles
// ---------------------------------------------------------------------------

/// One imported `.ovpn` server profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub id: String, // uuid v4
    pub name: String,
    pub group: Option<String>,
    pub remote_host: String,
    pub remote_port: u16,
    pub proto: String, // "udp" | "tcp"
    pub username: Option<String>,
    /// Plaintext for this first pass — see the module-level SECURITY NOTE.
    pub password: Option<String>,
    pub country_code: Option<String>, // ISO 3166-1 alpha-2, e.g. "DE"
    pub ping_ms: Option<u32>,
    pub ping_at: Option<String>, // ISO 8601 UTC
    pub favorite: bool,
    pub last_connected_at: Option<String>, // ISO 8601 UTC
    /// Original `.ovpn` text (already materialized where possible), used to
    /// rebuild the temp config at connect time.
    pub config_text: String,
}

impl Profile {
    pub fn new(
        name: String,
        group: Option<String>,
        remote_host: String,
        remote_port: u16,
        proto: String,
        username: Option<String>,
        password: Option<String>,
        config_text: String,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            name,
            group,
            remote_host,
            remote_port,
            proto,
            username,
            password,
            country_code: None,
            ping_ms: None,
            ping_at: None,
            favorite: false,
            last_connected_at: None,
            config_text,
        }
    }
}

/// JSON-backed profile store (`profiles.json` in the app-data dir).
pub struct ProfileStore {
    dir: PathBuf,
    profiles: Vec<Profile>,
}

impl ProfileStore {
    pub fn new(dir: PathBuf) -> Self {
        let mut store = Self {
            dir,
            profiles: Vec::new(),
        };
        store.profiles = store.load_from_disk().unwrap_or_default();
        store
    }

    fn file_path(&self) -> PathBuf {
        self.dir.join("profiles.json")
    }

    fn load_from_disk(&self) -> Result<Vec<Profile>, String> {
        let path = self.file_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        if text.trim().is_empty() {
            return Ok(Vec::new());
        }
        serde_json::from_str::<Vec<Profile>>(&text).map_err(|e| e.to_string())
    }

    fn save(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let text = serde_json::to_string_pretty(&self.profiles).map_err(|e| e.to_string())?;
        std::fs::write(self.file_path(), text).map_err(|e| e.to_string())
    }

    pub fn list(&self) -> Vec<Profile> {
        self.profiles.clone()
    }

    pub fn get(&self, id: &str) -> Option<Profile> {
        self.profiles.iter().find(|p| p.id == id).cloned()
    }

    pub fn add(&mut self, profile: Profile) -> Result<(), String> {
        self.profiles.push(profile);
        self.save()
    }

    pub fn remove(&mut self, id: &str) -> Result<bool, String> {
        let before = self.profiles.len();
        self.profiles.retain(|p| p.id != id);
        if self.profiles.len() == before {
            return Ok(false);
        }
        self.save()?;
        Ok(true)
    }

    #[allow(dead_code)] // Spec'd store API; used by later passes (edit UI).
    pub fn update(&mut self, profile: Profile) -> Result<bool, String> {
        for slot in self.profiles.iter_mut() {
            if slot.id == profile.id {
                *slot = profile;
                self.save()?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn set_favorite(&mut self, id: &str, favorite: bool) -> Result<bool, String> {
        self.mutate(id, |p| p.favorite = favorite)
    }

    pub fn set_group(&mut self, id: &str, group: Option<String>) -> Result<bool, String> {
        self.mutate(id, |p| p.group = group)
    }

    pub fn set_ping(&mut self, id: &str, ms: Option<u32>) -> Result<bool, String> {
        let at = now_iso_utc();
        self.mutate(id, |p| {
            p.ping_ms = ms;
            p.ping_at = Some(at.clone());
        })
    }

    pub fn set_country_code(&mut self, id: &str, code: String) -> Result<bool, String> {
        self.mutate(id, |p| p.country_code = Some(code))
    }

    pub fn set_last_connected(&mut self, id: &str) -> Result<bool, String> {
        let at = now_iso_utc();
        self.mutate(id, |p| p.last_connected_at = Some(at.clone()))
    }

    fn mutate<F: FnOnce(&mut Profile)>(
        &mut self,
        id: &str,
        f: F,
    ) -> Result<bool, String> {
        for slot in self.profiles.iter_mut() {
            if slot.id == id {
                f(slot);
                self.save()?;
                return Ok(true);
            }
        }
        Ok(false)
    }
}

// ---------------------------------------------------------------------------
// Usage counters
// ---------------------------------------------------------------------------

/// Daily byte totals, persisted in `usage.json` in the app-data dir.
///
/// Honest limitation for this pass: per-day history starts empty when the
/// app is first run. `today` / `week` / `month` are computed from the
/// daily buckets recorded since first use; there is no backfill of older
/// sessions. Counters accumulate from OpenVPN `>BYTECOUNT` deltas of each
/// session plus the stored running total.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub today_bytes: u64,
    pub week_bytes: u64,
    pub month_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct UsageData {
    #[serde(default)]
    total_bytes: u64,
    #[serde(default)]
    days: HashMap<String, u64>, // "YYYY-MM-DD" -> bytes (in + out)
}

pub struct UsageStore {
    dir: PathBuf,
    data: UsageData,
}

impl UsageStore {
    pub fn new(dir: PathBuf) -> Self {
        let mut store = Self {
            dir,
            data: UsageData::default(),
        };
        store.data = store.load_from_disk().unwrap_or_default();
        store
    }

    fn file_path(&self) -> PathBuf {
        self.dir.join("usage.json")
    }

    fn load_from_disk(&self) -> Result<UsageData, String> {
        let path = self.file_path();
        if !path.exists() {
            return Ok(UsageData::default());
        }
        let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        if text.trim().is_empty() {
            return Ok(UsageData::default());
        }
        serde_json::from_str::<UsageData>(&text).map_err(|e| e.to_string())
    }

    fn save(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let text = serde_json::to_string_pretty(&self.data).map_err(|e| e.to_string())?;
        std::fs::write(self.file_path(), text).map_err(|e| e.to_string())
    }

    /// Add `n` bytes (in + out) to today's bucket and the running total.
    pub fn add_bytes(&mut self, n: u64) {
        if n == 0 {
            return;
        }
        let today = today_date_string();
        *self.data.days.entry(today).or_insert(0) += n;
        self.data.total_bytes += n;
        let _ = self.save();
    }

    pub fn snapshot(&self) -> Usage {
        let today = self
            .data
            .days
            .get(&today_date_string())
            .copied()
            .unwrap_or(0);
        let week = (0..7u64)
            .map(|d| {
                self.data
                    .days
                    .get(&date_string_days_ago(d))
                    .copied()
                    .unwrap_or(0)
            })
            .sum();
        let month = (0..30u64)
            .map(|d| {
                self.data
                    .days
                    .get(&date_string_days_ago(d))
                    .copied()
                    .unwrap_or(0)
            })
            .sum();
        Usage {
            today_bytes: today,
            week_bytes: week,
            month_bytes: month,
        }
    }
}
