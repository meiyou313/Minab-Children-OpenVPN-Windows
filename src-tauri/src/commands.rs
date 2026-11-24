//! Tauri commands (frontend -> Rust) and the event payloads sent back.
//!
//! Command / event contract (frontend uses camelCase args, e.g.
//! `invoke('connect', { profileId })`):
//!
//! Commands:
//! * `list_profiles()` -> `Profile[]`
//! * `import_config_texts({ contents, names, username, password, group })`
//!   -> `ImportResult { imported, errors }`
//! * `start_ping_test({ profileIds? })` -> `string` (starts immediately,
//!   results arrive as `ping-result` events)
//! * `connect({ profileId })` -> `string`
//! * `disconnect()` -> `string`
//! * `get_status()` -> `VpnStatus`
//! * `delete_profile({ id })` -> `string`
//! * `set_favorite({ id, favorite })` -> `string`
//! * `set_group({ id, group })` -> `string`
//! * `get_usage()` -> `Usage`
//!
//! Events emitted to the frontend:
//! * `vpn-state`     { state, profileId, message }
//! * `ping-result`   { profileId, ms, ok }
//! * `bytecount`     { bytesIn, bytesOut }

use crate::geo;
use crate::ovpn;
use crate::ping::{self, PingResult, PingTarget};
use crate::store::{Profile, ProfileStore, Usage};
use crate::AppState;
use serde::Serialize;
use std::net::ToSocketAddrs;
use tauri::{AppHandle, Emitter, Manager, State};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub imported: usize,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VpnStatus {
    pub state: String, // "disconnected" | "connecting" | "connected" | "error"
    pub profile_id: Option<String>,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VpnStatePayload {
    pub state: String,
    pub profile_id: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BytecountPayload {
    pub bytes_in: u64,
    pub bytes_out: u64,
}

fn profiles_mut<'a>(state: &'a State<'_, AppState>) -> std::sync::MutexGuard<'a, ProfileStore> {
    state.profiles.lock().unwrap_or_else(|e| e.into_inner())
}

#[tauri::command]
pub fn list_profiles(state: State<'_, AppState>) -> Vec<Profile> {
    state
        .profiles
        .lock()
        .map(|store| store.list())
        .unwrap_or_default()
}

#[tauri::command]
pub fn import_config_texts(
    app: AppHandle,
    state: State<'_, AppState>,
    contents: Vec<String>,
    names: Vec<String>,
    username: String,
    password: String,
    group: Option<String>,
) -> Result<ImportResult, String> {
    let username = if username.trim().is_empty() {
        None
    } else {
        Some(username)
    };
    let password = if password.trim().is_empty() {
        None
    } else {
        Some(password)
    };

    let mut imported = 0usize;
    let mut errors: Vec<String> = Vec::new();
    let mut geo_targets: Vec<(String, String, u16)> = Vec::new();

    for (i, text) in contents.iter().enumerate() {
        let file_name = names
            .get(i)
            .cloned()
            .unwrap_or_else(|| format!("config-{}.ovpn", i + 1));
        let display_name = file_name
            .trim_end_matches(".ovpn")
            .trim_end_matches(".OVPN")
            .to_string();
        match ovpn::parse_ovpn(text) {
            Ok(parsed) => {
                let name = if display_name.is_empty() {
                    parsed.remote_host.clone()
                } else {
                    display_name
                };
                let profile = Profile::new(
                    name,
                    group.clone(),
                    parsed.remote_host.clone(),
                    parsed.remote_port,
                    parsed.proto.clone(),
                    username.clone(),
                    password.clone(),
                    text.clone(),
                );
                let id = profile.id.clone();
                {
                    let mut store = profiles_mut(&state);
                    store.add(profile)?;
                }
                geo_targets.push((id, parsed.remote_host, parsed.remote_port));
                imported += 1;
            }
            Err(e) => errors.push(format!("{file_name}: {e}")),
        }
    }

    // Country lookup runs in the background: resolve each new remote host
    // to an IP, look it up (30-day cache), and store the country code.
    // Never blocks the UI thread or the import result.
    if !geo_targets.is_empty() {
        let handle = app.clone();
        std::thread::spawn(move || {
            let dir = match handle.path().app_data_dir() {
                Ok(d) => d,
                Err(_) => return,
            };
            for (id, host, port) in geo_targets {
                let ip = resolve_host_ip(&host, port);
                let ip = match ip {
                    Some(ip) => ip,
                    None => continue,
                };
                if let Some(code) = geo::country_code_for_ip(&ip, &dir) {
                    if let Ok(mut store) =
                        handle.state::<AppState>().profiles.lock()
                    {
                        let _ = store.set_country_code(&id, code);
                    }
                }
            }
        });
    }

    Ok(ImportResult { imported, errors })
}

/// Resolve a remote host to an IP string (already an IP passes through).
fn resolve_host_ip(host: &str, port: u16) -> Option<String> {
    if host.parse::<std::net::IpAddr>().is_ok() {
        return Some(host.to_string());
    }
    (host, port)
        .to_socket_addrs()
        .ok()?
        .next()
        .map(|addr| addr.ip().to_string())
}

#[tauri::command]
pub fn start_ping_test(
    app: AppHandle,
    state: State<'_, AppState>,
    profile_ids: Option<Vec<String>>,
) -> Result<String, String> {
    let targets: Vec<PingTarget> = {
        let store = state
            .profiles
            .lock()
            .map_err(|_| "Profile store lock poisoned".to_string())?;
        store
            .list()
            .into_iter()
            .filter(|p| match &profile_ids {
                Some(ids) => ids.iter().any(|id| id == &p.id),
                None => true,
            })
            .map(|p| PingTarget {
                profile_id: p.id,
                host: p.remote_host,
                port: p.remote_port,
                proto: p.proto,
            })
            .collect()
    };
    if targets.is_empty() {
        return Ok("No profiles to test".to_string());
    }
    let count = targets.len();
    let handle = app.clone();
    std::thread::spawn(move || {
        ping::start_ping_test(targets, move |result: PingResult| {
            // Persist the result, then forward it to the frontend.
            if let Ok(mut store) = handle.state::<AppState>().profiles.lock() {
                let _ = store.set_ping(&result.profile_id, result.ms);
            }
            let _ = handle.emit("ping-result", result);
        });
    });
    Ok(format!("Ping test started for {count} profile(s)"))
}

#[tauri::command]
pub fn connect(
    app: AppHandle,
    state: State<'_, AppState>,
    profile_id: String,
) -> Result<String, String> {
    let profile = {
        let store = state
            .profiles
            .lock()
            .map_err(|_| "Profile store lock poisoned".to_string())?;
        store
            .get(&profile_id)
            .ok_or_else(|| format!("Profile not found: {profile_id}"))?
    };
    let resource_dir = app.path().resource_dir().ok();
    let name = profile.name.clone();
    // Clone the manager out of the lock: connect() waits up to 10 s for the
    // management interface, and holding the AppState lock that long would
    // block get_status()/disconnect() for the whole wait.
    let manager = state
        .vpn
        .lock()
        .map(|guard| guard.clone())
        .map_err(|_| "VPN lock poisoned".to_string())?;
    manager
        .connect(
            &profile.id,
            &name,
            &profile.config_text,
            profile.username.as_deref(),
            profile.password.as_deref(),
            resource_dir,
        )
        .map_err(|e| e)?;
    // `last_connected_at` is stamped by the vpn-state sink in lib.rs once
    // the tunnel actually reaches CONNECTED.
    Ok(format!("Connecting to {name}"))
}

#[tauri::command]
pub fn disconnect(state: State<'_, AppState>) -> Result<String, String> {
    let manager = state
        .vpn
        .lock()
        .map(|guard| guard.clone())
        .map_err(|_| "VPN lock poisoned".to_string())?;
    manager.disconnect()?;
    Ok("Disconnected".to_string())
}

#[tauri::command]
pub fn get_status(state: State<'_, AppState>) -> VpnStatus {
    let (vpn_state, profile_id, bytes_in, bytes_out, message) = state
        .vpn
        .lock()
        .map(|guard| guard.clone())
        .map(|m| m.status_snapshot())
        .unwrap_or((
            crate::openvpn::VpnState::Error,
            None,
            0,
            0,
            "VPN lock poisoned".to_string(),
        ));
    VpnStatus {
        state: vpn_state.as_str().to_string(),
        profile_id,
        bytes_in,
        bytes_out,
        message,
    }
}

#[tauri::command]
pub fn delete_profile(state: State<'_, AppState>, id: String) -> Result<String, String> {
    let mut store = profiles_mut(&state);
    if store.remove(&id)? {
        Ok("ok".to_string())
    } else {
        Err(format!("Profile not found: {id}"))
    }
}

#[tauri::command]
pub fn set_favorite(
    state: State<'_, AppState>,
    id: String,
    favorite: bool,
) -> Result<String, String> {
    let mut store = profiles_mut(&state);
    if store.set_favorite(&id, favorite)? {
        Ok("ok".to_string())
    } else {
        Err(format!("Profile not found: {id}"))
    }
}

#[tauri::command]
pub fn set_group(
    state: State<'_, AppState>,
    id: String,
    group: Option<String>,
) -> Result<String, String> {
    let mut store = profiles_mut(&state);
    if store.set_group(&id, group)? {
        Ok("ok".to_string())
    } else {
        Err(format!("Profile not found: {id}"))
    }
}

#[tauri::command]
pub fn get_usage(state: State<'_, AppState>) -> Usage {
    state
        .usage
        .lock()
        .map(|store| store.snapshot())
        .unwrap_or_default()
}
