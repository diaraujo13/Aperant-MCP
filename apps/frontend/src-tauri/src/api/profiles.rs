use crate::api::settings;
use crate::error::{AppError, AppResult};
use crate::types::IpcResult;
use serde_json::{json, Value};
use uuid::Uuid;

fn read_profiles() -> Value {
    let path = match settings::settings_path() {
        Ok(p) => p,
        Err(_) => return json!({ "profiles": [], "activeProfileId": "" }),
    };
    let s = settings::read_settings_at(&path);
    s.get("claudeProfiles").cloned().unwrap_or_else(|| json!({ "profiles": [], "activeProfileId": "" }))
}

fn save_profiles(data: &Value) -> AppResult<()> {
    let path = settings::settings_path()?;
    let patch = json!({ "claudeProfiles": data });
    settings::write_settings_at_with_patch(&path, patch)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn claude_profiles_get() -> AppResult<IpcResult<Value>> {
    let data = tokio::task::spawn_blocking(|| read_profiles())
        .await
        .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))?;
    Ok(IpcResult::ok(data))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn claude_profile_save(profile: Value) -> AppResult<IpcResult<Value>> {
    let saved = tokio::task::spawn_blocking(move || -> AppResult<Value> {
        let mut data = read_profiles();
        let profiles = data.get_mut("profiles")
            .and_then(|v| v.as_array_mut())
            .ok_or_else(|| AppError::new("invalid_profiles", "profiles field missing or not array"))?;

        // If profile has an id, update existing; otherwise add new
        let profile_id = profile.get("id").and_then(|v| v.as_str()).map(String::from)
            .unwrap_or_else(|| Uuid::new_v4().to_string());

        let mut profile = profile.clone();
        if let Some(obj) = profile.as_object_mut() {
            obj.insert("id".to_string(), json!(profile_id));
        }

        if let Some(existing) = profiles.iter_mut().find(|p| p.get("id").and_then(|v| v.as_str()) == Some(&profile_id)) {
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
pub async fn claude_profile_rename(profile_id: String, new_name: String) -> AppResult<IpcResult<()>> {
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
pub async fn claude_profile_switch(_terminal_id: String, _profile_id: String) -> AppResult<IpcResult<()>> {
    Ok(IpcResult { success: false, data: None, error: Some("profile_switch_not_ported".to_string()) })
}

#[tauri::command(rename_all = "camelCase")]
pub async fn claude_profile_initialize(_profile_id: String) -> AppResult<IpcResult<()>> {
    Ok(IpcResult { success: false, data: None, error: Some("profile_initialize_not_ported".to_string()) })
}

#[tauri::command(rename_all = "camelCase")]
pub async fn claude_profile_set_token(_profile_id: String, _token: String, _email: Option<String>) -> AppResult<IpcResult<()>> {
    Ok(IpcResult { success: false, data: None, error: Some("profile_set_token_not_ported".to_string()) })
}

#[tauri::command(rename_all = "camelCase")]
pub async fn claude_profile_authenticate(_profile_id: String) -> AppResult<IpcResult<Value>> {
    Ok(IpcResult { success: false, data: None, error: Some("profile_authenticate_not_ported".to_string()) })
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
                    let has_token = p.get("oauthToken").and_then(|v| v.as_str()).map(|t| !t.is_empty()).unwrap_or(false);

                    // Check configDir credentials if present
                    let config_dir_ok = p.get("configDir").and_then(|v| v.as_str())
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
    let auto_switch = s.get("claudeAutoSwitch").cloned().unwrap_or_else(|| json!({
        "enabled": false,
        "proactiveSwapEnabled": false,
        "usageCheckInterval": 30000,
        "sessionThreshold": 95,
        "weeklyThreshold": 99,
    }));
    Ok(IpcResult::ok(auto_switch))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn claude_auto_switch_update(settings: Value) -> AppResult<IpcResult<()>> {
    let path = settings::settings_path()?;
    let patch = json!({ "claudeAutoSwitch": settings });
    settings::write_settings_at_with_patch(&path, patch)?;
    Ok(IpcResult::ok(()))
}
