use crate::error::{AppError, AppResult};
use crate::types::IpcResult;
use serde::Serialize;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use tauri::AppHandle;
use tracing::warn;

const APP_NAME: &str = "auto-claude-ui";

/// Resolves to the same path Electron's `app.getPath('userData')` would,
/// so settings written by the Electron build remain readable here:
/// - macOS:   ~/Library/Application Support/auto-claude-ui/settings.json
/// - Windows: %APPDATA%\auto-claude-ui\settings.json
/// - Linux:   ~/.config/auto-claude-ui/settings.json
pub(crate) fn settings_path() -> AppResult<PathBuf> {
    let base = dirs::config_dir().ok_or_else(|| {
        AppError::new(
            "no_config_dir",
            "Could not resolve OS config directory",
        )
    })?;
    Ok(base.join(APP_NAME).join("settings.json"))
}

pub(crate) fn read_settings_at(path: &Path) -> Value {
    if !path.exists() {
        return Value::Object(Default::default());
    }
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "failed to read settings.json");
            return Value::Object(Default::default());
        }
    };
    serde_json::from_str(&content).unwrap_or_else(|e| {
        warn!(error = %e, "failed to parse settings.json, returning empty object");
        Value::Object(Default::default())
    })
}

pub(crate) fn write_settings_at(path: &Path, settings: &Value) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| AppError::new("create_dir_failed", e.to_string()))?;
    }
    let pretty = serde_json::to_string_pretty(settings)
        .map_err(|e| AppError::new("serialize_failed", e.to_string()))?;
    fs::write(path, pretty).map_err(|e| AppError::new("write_failed", e.to_string()))?;
    Ok(())
}

pub(crate) fn shallow_merge(base: &mut Value, patch: Value) {
    if let (Value::Object(b), Value::Object(p)) = (base, patch) {
        for (k, v) in p {
            b.insert(k, v);
        }
    }
}

#[tauri::command(rename_all = "camelCase")]
pub async fn settings_get() -> AppResult<IpcResult<Value>> {
    let path = settings_path()?;
    Ok(IpcResult::ok(read_settings_at(&path)))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn settings_save(settings: Value) -> AppResult<IpcResult<()>> {
    let path = settings_path()?;
    let mut current = read_settings_at(&path);
    shallow_merge(&mut current, settings);
    write_settings_at(&path, &current)?;
    Ok(IpcResult::ok(()))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn app_version(app_handle: AppHandle) -> AppResult<String> {
    Ok(app_handle.package_info().version.to_string())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SentryConfig {
    pub dsn: String,
    pub traces_sample_rate: f64,
    pub profiles_sample_rate: f64,
}

#[tauri::command(rename_all = "camelCase")]
pub async fn get_sentry_dsn() -> AppResult<String> {
    Ok(String::new())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn get_sentry_config() -> AppResult<SentryConfig> {
    Ok(SentryConfig {
        dsn: String::new(),
        traces_sample_rate: 0.0,
        profiles_sample_rate: 0.0,
    })
}

#[tauri::command(rename_all = "camelCase")]
pub async fn settings_get_cli_tools_info() -> AppResult<IpcResult<Value>> {
    Ok(IpcResult::ok(json!({
        "python": { "detected": false },
        "git": { "detected": false },
        "gh": { "detected": false },
        "claude": { "detected": false },
    })))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn settings_claude_code_get_onboarding_status() -> AppResult<IpcResult<Value>> {
    Ok(IpcResult::ok(json!({ "hasCompletedOnboarding": false })))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn provider_accounts_get() -> AppResult<IpcResult<Value>> {
    Ok(IpcResult::ok(json!({
        "accounts": [],
        "globalPriorityOrder": [],
        "disabledAutoSwitchAccountIds": [],
    })))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn spellcheck_set_languages(_language: String) -> AppResult<IpcResult<Value>> {
    Ok(IpcResult::ok(json!({ "success": true })))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn autobuild_source_env_get() -> AppResult<IpcResult<Value>> {
    Ok(IpcResult::ok(json!({})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn read_missing_returns_empty_object() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("missing.json");
        let v = read_settings_at(&path);
        assert_eq!(v, json!({}));
    }

    #[test]
    fn read_malformed_returns_empty_object() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("malformed.json");
        fs::write(&path, b"{ not json").unwrap();
        let v = read_settings_at(&path);
        assert_eq!(v, json!({}));
    }

    #[test]
    fn read_valid_returns_parsed() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("valid.json");
        fs::write(&path, br#"{"theme":"dark","onboardingCompleted":true}"#).unwrap();
        let v = read_settings_at(&path);
        assert_eq!(v["theme"], "dark");
        assert_eq!(v["onboardingCompleted"], true);
    }

    #[test]
    fn write_creates_parent_dir() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nested").join("dir").join("settings.json");
        write_settings_at(&path, &json!({"theme": "light"})).unwrap();
        assert!(path.exists());
        let v = read_settings_at(&path);
        assert_eq!(v["theme"], "light");
    }

    #[test]
    fn shallow_merge_overwrites_existing_keys() {
        let mut base = json!({"theme": "dark", "language": "en"});
        shallow_merge(&mut base, json!({"theme": "light"}));
        assert_eq!(base["theme"], "light");
        assert_eq!(base["language"], "en");
    }

    #[test]
    fn shallow_merge_adds_new_keys() {
        let mut base = json!({"theme": "dark"});
        shallow_merge(&mut base, json!({"newKey": 42}));
        assert_eq!(base["theme"], "dark");
        assert_eq!(base["newKey"], 42);
    }

    #[test]
    fn shallow_merge_does_not_deep_merge() {
        // The Electron handler uses spread which is shallow — preserve same semantics
        let mut base = json!({"nested": {"a": 1, "b": 2}});
        shallow_merge(&mut base, json!({"nested": {"c": 3}}));
        // Top-level "nested" was REPLACED (not merged), matching Electron behavior
        assert_eq!(base["nested"], json!({"c": 3}));
    }

    #[test]
    fn save_then_read_round_trip() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");

        let mut current = read_settings_at(&path);
        shallow_merge(&mut current, json!({"theme": "dark", "onboardingCompleted": true}));
        write_settings_at(&path, &current).unwrap();

        // Read it back
        let v = read_settings_at(&path);
        assert_eq!(v["theme"], "dark");
        assert_eq!(v["onboardingCompleted"], true);

        // Save another partial — shouldn't lose existing fields
        let mut current = read_settings_at(&path);
        shallow_merge(&mut current, json!({"language": "fr"}));
        write_settings_at(&path, &current).unwrap();

        let v = read_settings_at(&path);
        assert_eq!(v["theme"], "dark");
        assert_eq!(v["onboardingCompleted"], true);
        assert_eq!(v["language"], "fr");
    }
}
