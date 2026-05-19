use crate::api::settings;
use crate::error::{AppError, AppResult};
use crate::types::IpcResult;
use serde_json::{json, Value};
use uuid::Uuid;

// ── API profile helpers ───────────────────────────────────────────────────────

const APP_NAME: &str = "auto-claude-ui";
const API_PROFILES_SUBDIR: &str = "auto-claude";
const API_PROFILES_FILE: &str = "profiles.json";

/// `<userData>/auto-claude/profiles.json` — mirrors Electron's path.
fn api_profiles_path() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|d| {
        d.join(APP_NAME)
            .join(API_PROFILES_SUBDIR)
            .join(API_PROFILES_FILE)
    })
}

pub(crate) fn read_api_profiles() -> Value {
    let path = match api_profiles_path() {
        Some(p) => p,
        None => return json!({ "profiles": [], "activeProfileId": null, "version": 1 }),
    };
    if !path.exists() {
        return json!({ "profiles": [], "activeProfileId": null, "version": 1 });
    }
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({ "profiles": [], "activeProfileId": null, "version": 1 }))
}

fn write_api_profiles(data: &Value) -> AppResult<()> {
    let path = api_profiles_path()
        .ok_or_else(|| AppError::new("no_app_data", "Cannot resolve app data directory"))?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AppError::new("create_dir_failed", e.to_string()))?;
    }

    let text = serde_json::to_string_pretty(data)
        .map_err(|e| AppError::new("serialize_failed", e.to_string()))?;
    std::fs::write(&path, &text).map_err(|e| AppError::new("write_failed", e.to_string()))?;

    // Restrict to owner-read/write only (0600) — file contains API keys.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        let _ = std::fs::set_permissions(&path, perms);
    }

    Ok(())
}

fn now_ms_profiles() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub(crate) fn read_profiles() -> Value {
    let path = match settings::settings_path() {
        Ok(p) => p,
        Err(_) => return json!({ "profiles": [], "activeProfileId": "" }),
    };
    let s = settings::read_settings_at(&path);
    s.get("claudeProfiles")
        .cloned()
        .unwrap_or_else(|| json!({ "profiles": [], "activeProfileId": "" }))
}

fn save_profiles(data: &Value) -> AppResult<()> {
    let path = settings::settings_path()?;
    let patch = json!({ "claudeProfiles": data });
    settings::write_settings_at_with_patch(&path, patch)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn claude_profiles_get() -> AppResult<IpcResult<Value>> {
    let data = tokio::task::spawn_blocking(read_profiles)
        .await
        .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))?;
    Ok(IpcResult::ok(data))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn claude_profile_save(profile: Value) -> AppResult<IpcResult<Value>> {
    let saved = tokio::task::spawn_blocking(move || -> AppResult<Value> {
        let mut data = read_profiles();
        let profiles = data
            .get_mut("profiles")
            .and_then(|v| v.as_array_mut())
            .ok_or_else(|| {
                AppError::new("invalid_profiles", "profiles field missing or not array")
            })?;

        // If profile has an id, update existing; otherwise add new
        let profile_id = profile
            .get("id")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| Uuid::new_v4().to_string());

        let mut profile = profile.clone();
        if let Some(obj) = profile.as_object_mut() {
            obj.insert("id".to_string(), json!(profile_id));
        }

        if let Some(existing) = profiles
            .iter_mut()
            .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(&profile_id))
        {
            *existing = profile.clone();
        } else {
            profiles.push(profile.clone());
        }

        save_profiles(&data)?;
        Ok(profile)
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))??;
    Ok(IpcResult::ok(saved))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn claude_profile_delete(profile_id: String) -> AppResult<IpcResult<()>> {
    tokio::task::spawn_blocking(move || -> AppResult<()> {
        let mut data = read_profiles();
        if let Some(profiles) = data.get_mut("profiles").and_then(|v| v.as_array_mut()) {
            profiles.retain(|p| p.get("id").and_then(|v| v.as_str()) != Some(&profile_id));
        }
        save_profiles(&data)
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))??;
    Ok(IpcResult::ok(()))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn claude_profile_rename(
    profile_id: String,
    new_name: String,
) -> AppResult<IpcResult<()>> {
    tokio::task::spawn_blocking(move || -> AppResult<()> {
        let mut data = read_profiles();
        if let Some(profiles) = data.get_mut("profiles").and_then(|v| v.as_array_mut()) {
            for p in profiles.iter_mut() {
                if p.get("id").and_then(|v| v.as_str()) == Some(&profile_id) {
                    if let Some(obj) = p.as_object_mut() {
                        obj.insert("name".to_string(), json!(new_name));
                    }
                }
            }
        }
        save_profiles(&data)
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))??;
    Ok(IpcResult::ok(()))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn claude_profile_set_active(profile_id: String) -> AppResult<IpcResult<()>> {
    tokio::task::spawn_blocking(move || -> AppResult<()> {
        let mut data = read_profiles();
        if let Some(obj) = data.as_object_mut() {
            obj.insert("activeProfileId".to_string(), json!(profile_id));
        }
        save_profiles(&data)
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))??;
    Ok(IpcResult::ok(()))
}

// Stub commands for complex profile operations (require terminal/keychain)
#[tauri::command(rename_all = "camelCase")]
pub async fn claude_profile_switch(
    _terminal_id: String,
    _profile_id: String,
) -> AppResult<IpcResult<()>> {
    Ok(IpcResult {
        success: false,
        data: None,
        error: Some("profile_switch_not_ported".to_string()),
    })
}

#[tauri::command(rename_all = "camelCase")]
pub async fn claude_profile_initialize(_profile_id: String) -> AppResult<IpcResult<()>> {
    // Initialization now happens automatically during the /login flow — no-op.
    Ok(IpcResult::ok(()))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn claude_profile_set_token(
    profile_id: String,
    token: String,
    email: Option<String>,
) -> AppResult<IpcResult<()>> {
    tokio::task::spawn_blocking(move || -> AppResult<()> {
        let mut data = read_profiles();
        let profiles = data
            .get_mut("profiles")
            .and_then(|v| v.as_array_mut())
            .ok_or_else(|| AppError::new("invalid_profiles", "profiles field missing"))?;

        let found = profiles.iter_mut().any(|p| {
            if p.get("id").and_then(|v| v.as_str()) == Some(&profile_id) {
                p["oauthToken"] = json!(token);
                p["isAuthenticated"] = json!(true);
                if let Some(ref e) = email {
                    p["email"] = json!(e);
                }
                true
            } else {
                false
            }
        });

        if !found {
            return Err(AppError::new("profile_not_found", format!("Profile {profile_id} not found")));
        }
        save_profiles(&data)
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))??;

    Ok(IpcResult::ok(()))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn claude_profile_authenticate(profile_id: String) -> AppResult<IpcResult<Value>> {
    let result = tokio::task::spawn_blocking(move || -> AppResult<Value> {
        // Resolve config directory from profile, defaulting to ~/.claude
        let profiles_data = read_profiles();
        let profiles = profiles_data.get("profiles").and_then(|v| v.as_array());

        let config_dir_raw = profiles
            .and_then(|ps| {
                ps.iter().find(|p| p.get("id").and_then(|v| v.as_str()) == Some(&profile_id))
            })
            .and_then(|p| p.get("configDir").and_then(|v| v.as_str()))
            .map(String::from)
            .unwrap_or_else(|| "~/.claude".to_string());

        // Expand leading ~ to home directory
        let config_dir = if config_dir_raw.starts_with('~') {
            dirs::home_dir()
                .map(|h| h.join(&config_dir_raw[2..]))
                .unwrap_or_else(|| std::path::PathBuf::from(&config_dir_raw))
        } else {
            std::path::PathBuf::from(&config_dir_raw)
        };

        std::fs::create_dir_all(&config_dir)
            .map_err(|e| AppError::new("create_dir_failed", e.to_string()))?;

        // Back up .claude.json if it contains OAuth credentials
        let claude_json = config_dir.join(".claude.json");
        let claude_json_bak = config_dir.join(".claude.json.bak");
        if claude_json.exists() {
            if let Ok(content) = std::fs::read_to_string(&claude_json) {
                if let Ok(parsed) = serde_json::from_str::<Value>(&content) {
                    if parsed.get("oauthAccount").is_some() {
                        let _ = std::fs::remove_file(&claude_json_bak);
                        let _ = std::fs::rename(&claude_json, &claude_json_bak);
                    }
                }
            }
        }

        let terminal_id = format!(
            "claude-login-{}-{}",
            profile_id,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        );

        Ok(json!({
            "terminalId": terminal_id,
            "configDir": config_dir.to_string_lossy(),
        }))
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))??;

    Ok(IpcResult::ok(result))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn claude_profile_verify_auth(profile_id: String) -> AppResult<IpcResult<Value>> {
    let data = tokio::task::spawn_blocking(move || -> Value {
        let profiles_data = read_profiles();
        let profiles = profiles_data.get("profiles").and_then(|v| v.as_array());

        if let Some(profiles) = profiles {
            for p in profiles {
                if p.get("id").and_then(|v| v.as_str()) == Some(&profile_id) {
                    // Check oauth token presence
                    let has_token = p
                        .get("oauthToken")
                        .and_then(|v| v.as_str())
                        .map(|t| !t.is_empty())
                        .unwrap_or(false);

                    // Check configDir credentials if present
                    let config_dir_ok = p
                        .get("configDir")
                        .and_then(|v| v.as_str())
                        .map(|dir| {
                            std::path::Path::new(dir).join(".credentials.json").exists()
                                || std::path::Path::new(dir).join("credentials.json").exists()
                        })
                        .unwrap_or(false);

                    let authenticated = has_token || config_dir_ok;
                    let email = p.get("email").and_then(|v| v.as_str()).map(String::from);
                    return json!({ "authenticated": authenticated, "email": email });
                }
            }
        }
        json!({ "authenticated": false })
    })
    .await
    .unwrap_or_else(|_| json!({ "authenticated": false }));
    Ok(IpcResult::ok(data))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn claude_auto_switch_get() -> AppResult<IpcResult<Value>> {
    let path = settings::settings_path()?;
    let s = settings::read_settings_at(&path);
    let auto_switch = s.get("claudeAutoSwitch").cloned().unwrap_or_else(|| {
        json!({
            "enabled": false,
            "proactiveSwapEnabled": false,
            "usageCheckInterval": 30000,
            "sessionThreshold": 95,
            "weeklyThreshold": 99,
        })
    });
    Ok(IpcResult::ok(auto_switch))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn claude_auto_switch_update(settings: Value) -> AppResult<IpcResult<()>> {
    let path = settings::settings_path()?;
    let patch = json!({ "claudeAutoSwitch": settings });
    settings::write_settings_at_with_patch(&path, patch)?;
    Ok(IpcResult::ok(()))
}

// ── API profile commands ──────────────────────────────────────────────────────

#[tauri::command(rename_all = "camelCase")]
pub async fn api_profiles_get() -> AppResult<IpcResult<Value>> {
    let data = tokio::task::spawn_blocking(read_api_profiles)
        .await
        .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))?;
    Ok(IpcResult::ok(data))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn api_profile_save(profile: Value) -> AppResult<IpcResult<Value>> {
    let saved = tokio::task::spawn_blocking(move || -> AppResult<Value> {
        let mut store = read_api_profiles();
        let profiles = store
            .get_mut("profiles")
            .and_then(|v| v.as_array_mut())
            .ok_or_else(|| AppError::new("invalid_store", "profiles field missing"))?;

        let now = now_ms_profiles();
        let mut p = profile.clone();
        let obj = p
            .as_object_mut()
            .ok_or_else(|| AppError::new("invalid_profile", "profile must be an object"))?;

        // Validate `kind` discriminator (Phase 6d). Default to "anthropic" so
        // legacy profiles round-trip with an explicit kind on disk.
        match obj.get("kind").and_then(|v| v.as_str()) {
            None => {
                obj.insert("kind".into(), json!("anthropic"));
            }
            Some("anthropic") | Some("codex") => {}
            Some(_) => {
                return Err(AppError::new(
                    "invalid_kind",
                    "kind must be 'anthropic' or 'codex'",
                ))
            }
        }

        let id = obj
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from)
            .unwrap_or_else(|| Uuid::new_v4().to_string());

        obj.insert("id".into(), json!(id));
        obj.entry("createdAt").or_insert_with(|| json!(now));
        obj.insert("updatedAt".into(), json!(now));

        profiles.push(p.clone());
        write_api_profiles(&store)?;
        Ok(p)
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))??;
    Ok(IpcResult::ok(saved))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn api_profile_update(profile: Value) -> AppResult<IpcResult<Value>> {
    let updated = tokio::task::spawn_blocking(move || -> AppResult<Value> {
        let profile_id = profile
            .get("id")
            .and_then(|v| v.as_str())
            .map(String::from)
            .ok_or_else(|| AppError::new("missing_id", "profile.id is required"))?;

        // Validate `kind` if present (Phase 6d).
        if let Some(k) = profile.get("kind").and_then(|v| v.as_str()) {
            if k != "anthropic" && k != "codex" {
                return Err(AppError::new(
                    "invalid_kind",
                    "kind must be 'anthropic' or 'codex'",
                ));
            }
        }

        let mut store = read_api_profiles();
        let profiles = store
            .get_mut("profiles")
            .and_then(|v| v.as_array_mut())
            .ok_or_else(|| AppError::new("invalid_store", "profiles field missing"))?;

        let now = now_ms_profiles();
        let mut found = false;
        let mut result = profile.clone();

        for p in profiles.iter_mut() {
            if p.get("id").and_then(|v| v.as_str()) == Some(&profile_id) {
                // Merge: keep existing fields not in the update payload.
                if let (Some(existing), Some(incoming)) = (p.as_object_mut(), profile.as_object()) {
                    for (k, v) in incoming {
                        existing.insert(k.clone(), v.clone());
                    }
                    existing.insert("updatedAt".into(), json!(now));
                }
                result = p.clone();
                found = true;
                break;
            }
        }

        if !found {
            return Err(AppError::new(
                "not_found",
                format!("profile {} not found", profile_id),
            ));
        }

        write_api_profiles(&store)?;
        Ok(result)
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))??;
    Ok(IpcResult::ok(updated))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn api_profile_delete(profile_id: String) -> AppResult<IpcResult<()>> {
    tokio::task::spawn_blocking(move || -> AppResult<()> {
        let mut store = read_api_profiles();

        if let Some(profiles) = store.get_mut("profiles").and_then(|v| v.as_array_mut()) {
            profiles.retain(|p| p.get("id").and_then(|v| v.as_str()) != Some(&profile_id));
        }

        // Clear activeProfileId if it pointed to the deleted profile.
        if store.get("activeProfileId").and_then(|v| v.as_str()) == Some(&profile_id) {
            if let Some(obj) = store.as_object_mut() {
                obj.insert("activeProfileId".into(), json!(null));
            }
        }

        write_api_profiles(&store)
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))??;
    Ok(IpcResult::ok(()))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn api_profile_set_active(profile_id: Option<String>) -> AppResult<IpcResult<()>> {
    tokio::task::spawn_blocking(move || -> AppResult<()> {
        let mut store = read_api_profiles();
        if let Some(obj) = store.as_object_mut() {
            obj.insert("activeProfileId".into(), json!(profile_id));
        }
        write_api_profiles(&store)
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))??;
    Ok(IpcResult::ok(()))
}

/// Tests connectivity to a custom API endpoint by probing `/v1/models`.
/// Falls back to a minimal `/v1/messages` POST if the models endpoint returns 404.
#[tauri::command(rename_all = "camelCase")]
pub async fn api_profile_test_connection(
    base_url: String,
    api_key: String,
) -> AppResult<IpcResult<Value>> {
    let normalized = base_url.trim_end_matches('/').to_string();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| AppError::new("http_client_failed", e.to_string()))?;

    // Try GET /v1/models first.
    let models_res = client
        .get(format!("{}/v1/models", normalized))
        .header("x-api-key", &api_key)
        .header("anthropic-version", "2023-06-01")
        .send()
        .await;

    match models_res {
        Ok(r) if r.status().is_success() => Ok(IpcResult::ok(
            json!({ "success": true, "message": "Connection successful" }),
        )),
        Ok(r) if r.status().as_u16() == 404 => {
            // Endpoint doesn't expose /v1/models — probe /v1/messages instead.
            let messages_res = client
                .post(format!("{}/v1/messages", normalized))
                .header("x-api-key", &api_key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .body(r#"{"model":"claude-haiku-4-5","max_tokens":1,"messages":[{"role":"user","content":"hi"}]}"#)
                .send()
                .await;

            match messages_res {
                Ok(r) if matches!(r.status().as_u16(), 200 | 400 | 422) => Ok(IpcResult::ok(
                    json!({ "success": true, "message": "Connection successful" }),
                )),
                Ok(r) if r.status().as_u16() == 401 => Ok(IpcResult::ok(
                    json!({ "success": false, "errorType": "auth", "message": "Authentication failed — check your API key" }),
                )),
                Ok(r) => Ok(IpcResult::ok(
                    json!({ "success": false, "errorType": "endpoint", "message": format!("Unexpected status {}", r.status()) }),
                )),
                Err(e) => Ok(IpcResult::ok(
                    json!({ "success": false, "errorType": "network", "message": e.to_string() }),
                )),
            }
        }
        Ok(r) if r.status().as_u16() == 401 => Ok(IpcResult::ok(
            json!({ "success": false, "errorType": "auth", "message": "Authentication failed — check your API key" }),
        )),
        Ok(r) => Ok(IpcResult::ok(
            json!({ "success": false, "errorType": "endpoint", "message": format!("Unexpected status {}", r.status()) }),
        )),
        Err(e) if e.is_timeout() => Ok(IpcResult::ok(
            json!({ "success": false, "errorType": "timeout", "message": "Connection timed out" }),
        )),
        Err(e) => Ok(IpcResult::ok(
            json!({ "success": false, "errorType": "network", "message": e.to_string() }),
        )),
    }
}

/// Fetches available models from `{baseUrl}/v1/models`.
#[tauri::command(rename_all = "camelCase")]
pub async fn api_profile_discover_models(
    base_url: String,
    api_key: String,
) -> AppResult<IpcResult<Value>> {
    let normalized = base_url.trim_end_matches('/').to_string();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| AppError::new("http_client_failed", e.to_string()))?;

    let res = client
        .get(format!("{}/v1/models", normalized))
        .header("x-api-key", &api_key)
        .header("anthropic-version", "2023-06-01")
        .send()
        .await
        .map_err(|e| AppError::new("request_failed", e.to_string()))?;

    if !res.status().is_success() {
        return Ok(IpcResult::ok(json!({ "models": [] })));
    }

    let body: Value = res.json().await.unwrap_or_else(|_| json!({ "data": [] }));

    // Anthropic returns `{ "data": [{ "id": "...", "display_name": "..." }] }`.
    let models: Vec<Value> = body
        .get("data")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|m| {
            let id = m.get("id")?.as_str()?.to_string();
            let display_name = m
                .get("display_name")
                .and_then(|v| v.as_str())
                .unwrap_or(&id)
                .to_string();
            Some(json!({ "id": id, "display_name": display_name }))
        })
        .collect();

    Ok(IpcResult::ok(json!({ "models": models })))
}

// ── Provider accounts ─────────────────────────────────────────────────────────

fn user_data_dir() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|d| d.join(APP_NAME))
}

fn provider_accounts_path() -> Option<std::path::PathBuf> {
    user_data_dir().map(|d| d.join("provider-accounts.json"))
}

fn read_provider_accounts() -> Value {
    let path = match provider_accounts_path() {
        Some(p) => p,
        None => return json!([]),
    };
    if !path.exists() {
        return json!([]);
    }
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    serde_json::from_str(&raw).unwrap_or_else(|_| json!([]))
}

fn write_provider_accounts(accounts: &Value) -> Result<(), String> {
    let path = provider_accounts_path().ok_or_else(|| "no_data_dir".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let raw = serde_json::to_string_pretty(accounts).map_err(|e| e.to_string())?;
    std::fs::write(&path, raw).map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn provider_account_save(account: Value) -> AppResult<IpcResult<Value>> {
    let mut accounts = read_provider_accounts();
    let arr = accounts
        .as_array_mut()
        .ok_or_else(|| AppError::new("invalid_state", "accounts not array"))?;
    let id = Uuid::new_v4().to_string();
    let mut entry = account;
    entry["id"] = json!(id);
    arr.push(entry.clone());
    write_provider_accounts(&json!(arr)).map_err(|e| AppError::new("write_failed", e))?;
    Ok(IpcResult::ok(entry))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn provider_account_update(id: String, account: Value) -> AppResult<IpcResult<Value>> {
    let mut accounts = read_provider_accounts();
    let arr = accounts
        .as_array_mut()
        .ok_or_else(|| AppError::new("invalid_state", "accounts not array"))?;
    let pos = arr
        .iter()
        .position(|a| a.get("id").and_then(|v| v.as_str()) == Some(&id))
        .ok_or_else(|| AppError::new("not_found", format!("account {id} not found")))?;
    let mut updated = account;
    updated["id"] = json!(id);
    arr[pos] = updated.clone();
    write_provider_accounts(&json!(arr)).map_err(|e| AppError::new("write_failed", e))?;
    Ok(IpcResult::ok(updated))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn provider_account_delete(id: String) -> AppResult<IpcResult<()>> {
    let mut accounts = read_provider_accounts();
    let arr = accounts
        .as_array_mut()
        .ok_or_else(|| AppError::new("invalid_state", "accounts not array"))?;
    arr.retain(|a| a.get("id").and_then(|v| v.as_str()) != Some(&id));
    write_provider_accounts(&json!(arr)).map_err(|e| AppError::new("write_failed", e))?;
    Ok(IpcResult::ok(()))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn provider_account_set_order(ids: Vec<String>) -> AppResult<IpcResult<()>> {
    let accounts = read_provider_accounts();
    let arr = accounts
        .as_array()
        .ok_or_else(|| AppError::new("invalid_state", "accounts not array"))?;
    let mut reordered: Vec<Value> = Vec::new();
    for id in &ids {
        if let Some(a) = arr
            .iter()
            .find(|a| a.get("id").and_then(|v| v.as_str()) == Some(id.as_str()))
        {
            reordered.push(a.clone());
        }
    }
    for a in arr {
        let aid = a.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if !ids.iter().any(|id| id == aid) {
            reordered.push(a.clone());
        }
    }
    write_provider_accounts(&json!(reordered)).map_err(|e| AppError::new("write_failed", e))?;
    Ok(IpcResult::ok(()))
}

// ── Usage cache ───────────────────────────────────────────────────────────────

fn usage_cache_path() -> Option<std::path::PathBuf> {
    user_data_dir().map(|d| d.join("usage-cache.json"))
}

fn read_usage_cache() -> Value {
    let path = match usage_cache_path() {
        Some(p) => p,
        None => return json!({}),
    };
    if !path.exists() {
        return json!({});
    }
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    serde_json::from_str(&raw).unwrap_or_else(|_| json!({}))
}

fn write_usage_cache(cache: &Value) -> Result<(), String> {
    let path = usage_cache_path().ok_or_else(|| "no_data_dir".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let raw = serde_json::to_string_pretty(cache).map_err(|e| e.to_string())?;
    std::fs::write(&path, raw).map_err(|e| e.to_string())
}

// ── Profile priority ──────────────────────────────────────────────────────────

fn profile_priority_path() -> Option<std::path::PathBuf> {
    user_data_dir().map(|d| d.join("profile-priority.json"))
}

fn read_profile_priority() -> Vec<String> {
    let path = match profile_priority_path() {
        Some(p) => p,
        None => return vec![],
    };
    if !path.exists() {
        return vec![];
    }
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    serde_json::from_str::<Vec<String>>(&raw).unwrap_or_default()
}

fn write_profile_priority(order: &[String]) -> Result<(), String> {
    let path = profile_priority_path().ok_or_else(|| "no_data_dir".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let raw = serde_json::to_string_pretty(order).map_err(|e| e.to_string())?;
    std::fs::write(&path, raw).map_err(|e| e.to_string())
}

// ── Usage monitoring commands ─────────────────────────────────────────────────

#[tauri::command(rename_all = "camelCase")]
pub async fn usage_request_update(
    app: tauri::AppHandle,
    profile_id: String,
) -> AppResult<IpcResult<()>> {
    use tauri::Emitter;
    let cache = read_usage_cache();
    let entry = cache.get(&profile_id).cloned().unwrap_or(json!(null));
    let _ = app.emit(
        "profile:usage:updated",
        json!({ "profileId": profile_id, "usage": entry }),
    );
    Ok(IpcResult::ok(()))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn usage_request_all(app: tauri::AppHandle) -> AppResult<IpcResult<()>> {
    use tauri::Emitter;
    let cache = read_usage_cache();
    let _ = app.emit("profile:all_usage_updated", &cache);
    Ok(IpcResult::ok(()))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn usage_fetch_claude(
    app: tauri::AppHandle,
    profile_id: String,
) -> AppResult<IpcResult<Value>> {
    use tauri::Emitter;
    let api_key = settings::settings_path()
        .ok()
        .map(|p| settings::read_settings_at(&p))
        .and_then(|s| {
            s.get("claudeProfiles")
                .and_then(|v| v.as_array())
                .and_then(|arr| {
                    arr.iter()
                        .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(&profile_id))
                        .and_then(|p| p.get("apiKey"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
        })
        .unwrap_or_default();

    if api_key.is_empty() {
        return Ok(IpcResult::ok(json!(null)));
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| AppError::new("http_client_failed", e.to_string()))?;

    let res = client
        .get("https://api.anthropic.com/v1/usage")
        .header("x-api-key", &api_key)
        .header("anthropic-version", "2023-06-01")
        .send()
        .await
        .map_err(|e| AppError::new("request_failed", e.to_string()))?;

    let usage: Value = res.json().await.unwrap_or(json!(null));

    let mut cache = read_usage_cache();
    if let Some(obj) = cache.as_object_mut() {
        obj.insert(profile_id.clone(), usage.clone());
    }
    let _ = write_usage_cache(&cache);
    let _ = app.emit(
        "profile:usage:updated",
        json!({ "profileId": profile_id, "usage": usage }),
    );

    Ok(IpcResult::ok(usage))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn profile_get_best_available() -> AppResult<IpcResult<Value>> {
    let cache = read_usage_cache();
    let priority = read_profile_priority();
    let profiles: Vec<Value> = settings::settings_path()
        .ok()
        .map(|p| settings::read_settings_at(&p))
        .and_then(|s| {
            s.get("claudeProfiles")
                .and_then(|v| v.as_array())
                .cloned()
        })
        .unwrap_or_default();

    for id in &priority {
        if let Some(p) = profiles
            .iter()
            .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(id.as_str()))
        {
            let rate_limited = cache
                .get(id)
                .and_then(|u| u.get("rateLimited"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !rate_limited {
                return Ok(IpcResult::ok(p.clone()));
            }
        }
    }

    let best = profiles
        .into_iter()
        .find(|p| p.get("active").and_then(|v| v.as_bool()).unwrap_or(false))
        .unwrap_or(json!(null));

    Ok(IpcResult::ok(best))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn profile_get_priority_order() -> AppResult<IpcResult<Vec<String>>> {
    Ok(IpcResult::ok(read_profile_priority()))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn profile_set_priority_order(order: Vec<String>) -> AppResult<IpcResult<()>> {
    write_profile_priority(&order).map_err(|e| AppError::new("write_failed", e))?;
    Ok(IpcResult::ok(()))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn profile_retry_with(
    app: tauri::AppHandle,
    profile_id: String,
    payload: Value,
) -> AppResult<IpcResult<()>> {
    use tauri::Emitter;
    let _ = app.emit(
        "profile:proactive_swap",
        json!({ "profileId": profile_id, "payload": payload }),
    );
    Ok(IpcResult::ok(()))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn check_claude_auth(_project_id: String) -> AppResult<IpcResult<Value>> {
    // Check if claude binary is reachable
    let cli_found = {
        let paths = crate::api::claude_code::paths_to_probe();
        paths.iter().any(|(p, _)| p.exists())
    };

    if !cli_found {
        return Ok(IpcResult::ok(json!({
            "authenticated": false,
            "error": "Claude CLI not found. Please install it first."
        })));
    }

    // Check if the active profile has valid credentials
    let authenticated = tokio::task::spawn_blocking(|| -> bool {
        let data = read_profiles();
        let active_id = data.get("activeProfileId").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let profiles = data.get("profiles").and_then(|v| v.as_array());

        if let Some(ps) = profiles {
            for p in ps {
                let id = p.get("id").and_then(|v| v.as_str()).unwrap_or("");
                if id != active_id && !active_id.is_empty() {
                    continue;
                }
                // Check token presence
                let has_token = p.get("oauthToken").and_then(|v| v.as_str()).map(|t| !t.is_empty()).unwrap_or(false);
                // Check credentials file
                let config_dir_ok = p.get("configDir").and_then(|v| v.as_str()).map(|dir| {
                    let dir_path = if dir.starts_with('~') {
                        dirs::home_dir().map(|h| h.join(&dir[2..])).unwrap_or_default()
                    } else {
                        std::path::PathBuf::from(dir)
                    };
                    dir_path.join(".claude.json").exists() || dir_path.join("credentials.json").exists()
                }).unwrap_or(false);
                // Check default ~/.claude.json
                let default_ok = dirs::home_dir()
                    .map(|h| h.join(".claude").join(".claude.json").exists() || h.join(".claude.json").exists())
                    .unwrap_or(false);
                if has_token || config_dir_ok || default_ok {
                    return true;
                }
            }
        }
        // If no profiles, check default location
        dirs::home_dir()
            .map(|h| h.join(".claude").join(".claude.json").exists() || h.join(".claude.json").exists())
            .unwrap_or(false)
    })
    .await
    .unwrap_or(false);

    Ok(IpcResult::ok(json!({ "authenticated": authenticated })))
}
