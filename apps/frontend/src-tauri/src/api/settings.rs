use crate::error::{AppError, AppResult};
use crate::types::IpcResult;
use fs2::FileExt;
use serde::Serialize;
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
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
        AppError::new("no_config_dir", "Could not resolve OS config directory")
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

/// Writes `settings` to `path` atomically: write to a sibling temp file, fsync-equivalent
/// flush via Drop, then rename. POSIX rename is atomic on the same filesystem so partial
/// writes from a crash mid-write cannot corrupt the destination file.
pub(crate) fn write_settings_at(path: &Path, settings: &Value) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| AppError::new("create_dir_failed", e.to_string()))?;
    }
    let pretty = serde_json::to_string_pretty(settings)
        .map_err(|e| AppError::new("serialize_failed", e.to_string()))?;

    let temp_path = path.with_extension("json.tmp");
    fs::write(&temp_path, pretty)
        .map_err(|e| AppError::new("temp_write_failed", e.to_string()))?;
    fs::rename(&temp_path, path).map_err(|e| {
        // Best effort cleanup of orphaned temp file
        let _ = fs::remove_file(&temp_path);
        AppError::new("rename_failed", e.to_string())
    })?;
    Ok(())
}

pub(crate) fn shallow_merge(base: &mut Value, patch: Value) {
    if let (Value::Object(b), Value::Object(p)) = (base, patch) {
        for (k, v) in p {
            b.insert(k, v);
        }
    }
}

/// Reads, merges, and writes settings under an exclusive cross-process file lock.
/// The lock prevents the Electron build from racing the Tauri build during the
/// parallel ship period — both implementations use the same lockfile path so
/// they serialize on the same OS-level advisory lock.
fn save_with_lock(path: &Path, patch: Value) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| AppError::new("create_dir_failed", e.to_string()))?;
    }
    let lock_path = path.with_extension("json.lock");
    let lock_file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| AppError::new("lock_open_failed", e.to_string()))?;
    lock_file
        .lock_exclusive()
        .map_err(|e| AppError::new("lock_acquire_failed", e.to_string()))?;

    let mut current = read_settings_at(path);
    shallow_merge(&mut current, patch);
    let result = write_settings_at(path, &current);

    // Release explicitly so any error in unlock surfaces; Drop would also release.
    let _ = FileExt::unlock(&lock_file);
    result
}

#[tauri::command(rename_all = "camelCase")]
pub async fn settings_get() -> AppResult<IpcResult<Value>> {
    let path = settings_path()?;
    Ok(IpcResult::ok(read_settings_at(&path)))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn settings_save(settings: Value) -> AppResult<IpcResult<()>> {
    let path = settings_path()?;
    save_with_lock(&path, settings)?;
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

/// Returns the canonical `ToolDetectionResult` shape the renderer expects.
/// Phase 2 round 4 will replace these with real detection.
fn unknown_tool(reason: &str) -> Value {
    json!({
        "found": false,
        "source": "fallback",
        "message": reason,
    })
}

#[tauri::command(rename_all = "camelCase")]
pub async fn settings_get_cli_tools_info() -> AppResult<IpcResult<Value>> {
    Ok(IpcResult::ok(json!({
        "python": unknown_tool("Detection deferred to Phase 2 round 4"),
        "git":    unknown_tool("Detection deferred to Phase 2 round 4"),
        "gh":     unknown_tool("Detection deferred to Phase 2 round 4"),
        "claude": unknown_tool("Use checkClaudeCodeVersion for live detection"),
    })))
}

/// Reads `~/.claude.json` to determine if the user has completed Claude Code
/// onboarding (matches the Electron implementation). Returns false on any I/O
/// or parse error so a missing file behaves as "not yet onboarded".
#[tauri::command(rename_all = "camelCase")]
pub async fn settings_claude_code_get_onboarding_status() -> AppResult<IpcResult<Value>> {
    let completed = dirs::home_dir()
        .map(|h| h.join(".claude.json"))
        .filter(|p| p.exists())
        .and_then(|p| fs::read_to_string(&p).ok())
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.get("hasCompletedOnboarding").and_then(|x| x.as_bool()))
        .unwrap_or(false);

    Ok(IpcResult::ok(json!({ "hasCompletedOnboarding": completed })))
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
    use std::sync::Arc;
    use std::thread;
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
    fn write_is_atomic_no_temp_leak_on_success() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        write_settings_at(&path, &json!({"a": 1})).unwrap();
        // Temp file should not exist after successful rename
        assert!(!path.with_extension("json.tmp").exists());
        assert!(path.exists());
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
        let mut base = json!({"nested": {"a": 1, "b": 2}});
        shallow_merge(&mut base, json!({"nested": {"c": 3}}));
        assert_eq!(base["nested"], json!({"c": 3}));
    }

    #[test]
    fn save_then_read_round_trip() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");

        save_with_lock(&path, json!({"theme": "dark", "onboardingCompleted": true})).unwrap();
        let v = read_settings_at(&path);
        assert_eq!(v["theme"], "dark");
        assert_eq!(v["onboardingCompleted"], true);

        save_with_lock(&path, json!({"language": "fr"})).unwrap();
        let v = read_settings_at(&path);
        assert_eq!(v["theme"], "dark");
        assert_eq!(v["onboardingCompleted"], true);
        assert_eq!(v["language"], "fr");
    }

    #[test]
    fn concurrent_saves_do_not_corrupt() {
        // 8 parallel threads each save a unique key. With the lock, all 8
        // writes survive: the file ends up containing all 8 keys. Without
        // the lock, last-writer-wins would lose most keys.
        let tmp = TempDir::new().unwrap();
        let path = Arc::new(tmp.path().join("settings.json"));

        // Seed file so all writers race on the same starting state
        write_settings_at(&path, &json!({})).unwrap();

        let mut handles = Vec::new();
        for i in 0..8 {
            let path = Arc::clone(&path);
            handles.push(thread::spawn(move || {
                let key = format!("key_{i}");
                save_with_lock(&path, json!({ key.clone(): i })).unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let v = read_settings_at(&path);
        for i in 0..8 {
            assert_eq!(
                v[format!("key_{i}")],
                i,
                "key_{i} should survive concurrent writes"
            );
        }
    }
}
