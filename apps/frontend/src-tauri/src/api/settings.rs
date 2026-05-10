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
    let base = dirs::config_dir()
        .ok_or_else(|| AppError::new("no_config_dir", "Could not resolve OS config directory"))?;
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
    fs::write(&temp_path, pretty).map_err(|e| AppError::new("temp_write_failed", e.to_string()))?;
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

/// Public(crate) wrapper around `save_with_lock` for use by sibling modules
/// (e.g. `api::profiles`) that need to write a patch into settings.json.
pub(crate) fn write_settings_at_with_patch(path: &Path, patch: Value) -> AppResult<()> {
    save_with_lock(path, patch)
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

/// Resolves the first line of a command's stdout or stderr (whichever is
/// non-empty) as a version string. Returns `None` if the command is not found
/// or exits with a non-zero code and no output.
fn probe_version(name: &str) -> Option<String> {
    let output = std::process::Command::new(name)
        .arg("--version")
        .output()
        .ok()?;
    // Python < 3.4 writes to stderr; everything else uses stdout.
    let bytes = if !output.stdout.is_empty() {
        &output.stdout
    } else {
        &output.stderr
    };
    let line = String::from_utf8_lossy(bytes)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if line.is_empty() {
        None
    } else {
        Some(line)
    }
}

/// Returns the full path of `name` by delegating to `which` (Unix) or
/// `where` (Windows). Returns an empty string if not found.
fn which_path(name: &str) -> String {
    let finder = if cfg!(windows) { "where" } else { "which" };
    std::process::Command::new(finder)
        .arg(name)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string()
        })
        .unwrap_or_default()
}

/// Tries each candidate in order and returns the first one found. The
/// `ToolDetectionResult` shape matches what the renderer expects.
fn detect_tool(candidates: &[&str]) -> Value {
    for &name in candidates {
        if let Some(version) = probe_version(name) {
            let path = which_path(name);
            return json!({
                "found": true,
                "source": "path",
                "version": version,
                "path": path,
            });
        }
    }
    json!({
        "found": false,
        "source": "fallback",
        "message": "not found in PATH",
    })
}

#[tauri::command(rename_all = "camelCase")]
pub async fn settings_get_cli_tools_info() -> AppResult<IpcResult<Value>> {
    // Run blocking probes in a dedicated thread so we don't stall the tokio executor.
    let result = tokio::task::spawn_blocking(|| {
        json!({
            "python": detect_tool(&["python3", "python"]),
            "git":    detect_tool(&["git"]),
            "gh":     detect_tool(&["gh"]),
            // claude detection is handled by checkClaudeCodeVersion which has
            // richer version/path logic (multi-install support).
            "claude": json!({
                "found": false,
                "source": "fallback",
                "message": "Use checkClaudeCodeVersion for live detection",
            }),
        })
    })
    .await
    .unwrap_or_else(|_| json!({}));

    Ok(IpcResult::ok(result))
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

    Ok(IpcResult::ok(
        json!({ "hasCompletedOnboarding": completed }),
    ))
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

/// Parses a `.env` file into a `HashMap<String, String>`, skipping blank
/// lines and `#` comments. Values may be single- or double-quoted.
fn parse_env_file(content: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, val)) = line.split_once('=') {
            let key = key.trim().to_string();
            let val = val.trim().trim_matches('"').trim_matches('\'').to_string();
            map.insert(key, val);
        }
    }
    map
}

/// Returns the OAuth token configuration used by the Python backend.
///
/// Lookup order (mirrors the Electron production build):
///   1. `<appData>/backend/.env` for `CLAUDE_CODE_OAUTH_TOKEN`
///   2. `<autoBuildPath>/.env` if `autoBuildPath` is set in settings
///   3. `settings.globalClaudeOAuthToken` as final fallback
#[tauri::command(rename_all = "camelCase")]
pub async fn autobuild_source_env_get() -> AppResult<IpcResult<Value>> {
    let settings_val = settings_path()
        .ok()
        .map(|p| read_settings_at(&p))
        .unwrap_or_else(|| Value::Object(Default::default()));

    // Candidate .env paths in priority order.
    let app_data_env = dirs::config_dir().map(|d| d.join(APP_NAME).join("backend").join(".env"));
    let auto_build_env = settings_val
        .get("autoBuildPath")
        .and_then(|v| v.as_str())
        .map(|p| PathBuf::from(p).join(".env"));

    let mut has_claude_token = false;
    let mut claude_oauth_token: Option<String> = None;
    let mut source_path: Option<String> = None;
    let mut env_exists = false;

    for candidate in [app_data_env.as_deref(), auto_build_env.as_deref()]
        .into_iter()
        .flatten()
    {
        if candidate.exists() {
            env_exists = true;
            source_path = Some(candidate.to_string_lossy().into_owned());
            if let Ok(content) = fs::read_to_string(candidate) {
                let vars = parse_env_file(&content);
                if let Some(token) = vars.get("CLAUDE_CODE_OAUTH_TOKEN") {
                    if !token.is_empty() {
                        claude_oauth_token = Some(token.clone());
                        has_claude_token = true;
                        break;
                    }
                }
            }
            break;
        }
    }

    // Global token fallback from settings.json.
    if !has_claude_token {
        if let Some(token) = settings_val
            .get("globalClaudeOAuthToken")
            .and_then(|v| v.as_str())
            .filter(|t| !t.is_empty())
        {
            claude_oauth_token = Some(token.to_string());
            has_claude_token = true;
        }
    }

    Ok(IpcResult::ok(json!({
        "hasClaudeToken": has_claude_token,
        "claudeOAuthToken": claude_oauth_token,
        "sourcePath": source_path,
        "envExists": env_exists,
    })))
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
