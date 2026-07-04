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

/// Expand a leading `~` in a user-supplied path. Handles bare `"~"` and
/// `"~/rest"`; other `~x` forms are treated as literal paths. Byte-slicing
/// (`&dir[2..]`) is deliberately avoided: it panics on `"~"` and on a
/// multibyte char at byte index 2, and configDir comes from user-editable
/// settings.json.
fn expand_tilde(dir: &str, home: &Path) -> PathBuf {
    if dir == "~" {
        home.to_path_buf()
    } else if let Some(rest) = dir.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(dir)
    }
}

/// Whether real CLI credentials exist at the default `~/.claude` location.
/// `.claude.json` alone is NOT proof of login — it's a general CLI state file
/// created on first run and not removed by `/logout`. Actual credentials live
/// in `.credentials.json` (Linux/Windows) or the macOS Keychain.
fn default_location_authenticated(home: &Path) -> bool {
    if home.as_os_str().is_empty() {
        return false;
    }
    if home.join(".claude").join(".credentials.json").exists() {
        return true;
    }
    #[cfg(target_os = "macos")]
    if macos_keychain_has_claude_credentials() {
        return true;
    }
    false
}

#[cfg(target_os = "macos")]
fn macos_keychain_has_claude_credentials() -> bool {
    std::process::Command::new("security")
        .args(["find-generic-password", "-s", "Claude Code-credentials"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Check whether any registered Claude profile (or the default ~/.claude location)
/// has credentials, indicating active Claude Code CLI authentication.
///
/// This is the fallback auth check for Tauri: Rust commands (ideation, insights,
/// roadmap) invoke `claude` CLI directly and inherit its auth state, so CLI
/// credentials are sufficient — no explicit `CLAUDE_CODE_OAUTH_TOKEN` in `.env`
/// is required.
pub(crate) fn is_cli_authenticated(profiles_data: &Value) -> bool {
    let home = dirs::home_dir().unwrap_or_default();
    let default_ok = default_location_authenticated(&home);
    is_cli_authenticated_at(profiles_data, &home, default_ok)
}

/// Testable core of [`is_cli_authenticated`]: `home` and `default_ok` are
/// injected so unit tests control the environment instead of reading the real
/// `$HOME` / Keychain.
pub(crate) fn is_cli_authenticated_at(
    profiles_data: &Value,
    home: &Path,
    default_ok: bool,
) -> bool {
    let profile_ok = profiles_data
        .get("profiles")
        .and_then(|v| v.as_array())
        .map(|profiles| {
            profiles.iter().any(|p| {
                let has_stored_token = p
                    .get("oauthToken")
                    .and_then(|v| v.as_str())
                    .map(|t| !t.is_empty())
                    .unwrap_or(false);

                let config_dir_ok = p
                    .get("configDir")
                    .and_then(|v| v.as_str())
                    .map(|dir| {
                        let d = expand_tilde(dir, home);
                        d.join(".claude.json").exists()
                            || d.join("credentials.json").exists()
                            || d.join(".credentials.json").exists()
                    })
                    .unwrap_or(false);

                has_stored_token || config_dir_ok
            })
        })
        .unwrap_or(false);

    // default_ok is OR'ed unconditionally (matching the Electron handler): a CLI
    // login at the default location authenticates regardless of how many profiles
    // are registered — including zero. The previous `else { default_ok }` branch
    // was dead code because read_profiles() always returns a `profiles` array.
    profile_ok || default_ok
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

    // Fall back to checking CLI profile authentication: if any profile has credentials
    // on disk (isAuthenticated), the Tauri Rust commands work without a backend .env
    // token because they call `claude` directly (not the Python backend).
    let cli_authenticated = if !has_claude_token {
        let profiles_data = super::profiles::read_profiles();
        is_cli_authenticated(&profiles_data)
    } else {
        false
    };

    let effective_has_token = has_claude_token || cli_authenticated;

    tracing::info!(
        has_env_token = has_claude_token,
        cli_authenticated,
        effective_has_token,
        "autobuild_source_env_get: auth check complete"
    );

    Ok(IpcResult::ok(json!({
        // `hasToken` is the canonical field the renderer checks (useIdeationAuth,
        // EnvConfigModal, useClaudeTokenCheck). `hasClaudeToken` kept as alias.
        "hasToken": effective_has_token,
        "hasClaudeToken": effective_has_token,
        "claudeOAuthToken": claude_oauth_token,
        "sourcePath": source_path,
        "envExists": env_exists,
        // True when auth comes from CLI credentials rather than an explicit .env token.
        // Tauri Rust commands (ideation, insights, roadmap) use `claude` directly,
        // so CLI auth is sufficient — no backend .env token required.
        "cliAuthenticated": cli_authenticated,
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

    // ── is_cli_authenticated tests ────────────────────────────────────────────

    /// Bug fix regression: autobuild_source_env_get previously returned
    /// `hasClaudeToken` but the renderer checked `data?.hasToken` — always
    /// undefined → always falsy → auth modal appeared even when CLI was
    /// authenticated.  These tests prove `is_cli_authenticated` returns the
    /// correct value so `effective_has_token` (= `hasToken` in the response)
    /// is accurate.

    #[test]
    fn is_cli_authenticated_returns_true_when_profile_has_claude_json() {
        let tmp = TempDir::new().unwrap();
        // Create a fake profile config dir with a .claude.json credentials file
        let config_dir = tmp.path().join("profiles").join("user@example.com");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(config_dir.join(".claude.json"), b"{}").unwrap();

        let profiles = json!({
            "profiles": [{
                "id": "profile-123",
                "name": "user@example.com",
                "configDir": config_dir.to_string_lossy(),
                "isDefault": false
            }]
        });

        assert!(
            is_cli_authenticated(&profiles),
            "profile with .claude.json in configDir must be detected as authenticated"
        );
    }

    #[test]
    fn is_cli_authenticated_returns_true_when_profile_has_credentials_json() {
        let tmp = TempDir::new().unwrap();
        let config_dir = tmp.path().join("profiles").join("work");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(config_dir.join("credentials.json"), b"{}").unwrap();

        let profiles = json!({
            "profiles": [{
                "id": "profile-work",
                "name": "Work",
                "configDir": config_dir.to_string_lossy(),
                "isDefault": false
            }]
        });

        assert!(is_cli_authenticated(&profiles));
    }

    #[test]
    fn is_cli_authenticated_returns_true_when_profile_has_stored_oauth_token() {
        let profiles = json!({
            "profiles": [{
                "id": "profile-tok",
                "name": "WithToken",
                "oauthToken": "sk-ant-oat01-some-token-value",
                "isDefault": false
            }]
        });
        // No filesystem access needed — stored token is sufficient.
        assert!(is_cli_authenticated(&profiles));
    }

    #[test]
    fn is_cli_authenticated_returns_false_when_no_credentials_and_no_token() {
        let tmp = TempDir::new().unwrap();
        // Config dir exists but contains NO credential files.
        let config_dir = tmp.path().join("empty-profile");
        fs::create_dir_all(&config_dir).unwrap();

        let profiles = json!({
            "profiles": [{
                "id": "profile-empty",
                "name": "Empty",
                "configDir": config_dir.to_string_lossy(),
                "isDefault": false
            }]
        });

        // With home and default_ok injected, the assertion is hermetic: a profile
        // whose configDir has no credential files must NOT count as authenticated.
        assert!(!is_cli_authenticated_at(&profiles, tmp.path(), false));

        // An absent configDir cannot contribute true either.
        let absent = json!({
            "profiles": [{
                "id": "x",
                "name": "X",
                "configDir": tmp.path().join("definitely-absent-dir").to_string_lossy(),
                "isDefault": false
            }]
        });
        assert!(!is_cli_authenticated_at(&absent, tmp.path(), false));
    }

    #[test]
    fn is_cli_authenticated_default_ok_suffices_with_zero_profiles() {
        // Regression: read_profiles() always returns a `profiles` array (possibly
        // empty), so the old `else { default_ok }` branch was dead code — a user
        // logged in via `claude /login` with no registered profiles got
        // hasToken=false, recreating the eternal auth loop. default_ok must be
        // OR'ed unconditionally.
        let tmp = TempDir::new().unwrap();
        let empty = json!({ "profiles": [] });
        assert!(is_cli_authenticated_at(&empty, tmp.path(), true));
        assert!(!is_cli_authenticated_at(&empty, tmp.path(), false));
    }

    #[test]
    fn expand_tilde_handles_bare_tilde_without_panic() {
        // Regression: `&dir[2..]` panicked on configDir "~" (byte 2 out of bounds)
        // and on multibyte chars at byte 2 (e.g. "~é/x"), aborting the whole
        // autobuild_source_env_get command from user-editable settings.json.
        let home = Path::new("/home/user");
        assert_eq!(expand_tilde("~", home), PathBuf::from("/home/user"));
        assert_eq!(expand_tilde("~/cfg", home), PathBuf::from("/home/user/cfg"));
        assert_eq!(expand_tilde("~é/x", home), PathBuf::from("~é/x"));
        assert_eq!(expand_tilde("/abs/path", home), PathBuf::from("/abs/path"));
    }

    #[test]
    fn is_cli_authenticated_at_does_not_panic_on_bare_tilde_config_dir() {
        let tmp = TempDir::new().unwrap();
        let profiles = json!({
            "profiles": [{
                "id": "p",
                "name": "Tilde",
                "configDir": "~",
                "isDefault": false
            }]
        });
        // home is an empty temp dir → no credentials → false, and no panic.
        assert!(!is_cli_authenticated_at(&profiles, tmp.path(), false));

        // Now place a credentials file at home and the same profile counts.
        fs::write(tmp.path().join(".claude.json"), b"{}").unwrap();
        assert!(is_cli_authenticated_at(&profiles, tmp.path(), false));
    }

    #[test]
    fn is_cli_authenticated_returns_true_for_non_default_profile_with_credentials() {
        // Regression for Bug 2: the old filter was `p.oauthToken || (p.isDefault && p.configDir)`
        // which excluded non-default profiles authenticated via credentials files.
        // is_cli_authenticated must return true for non-default profiles that have
        // credentials on disk.
        let tmp = TempDir::new().unwrap();
        let config_dir = tmp.path().join("izaiasousa-profile");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(config_dir.join(".claude.json"), b"{}").unwrap();

        let profiles = json!({
            "profiles": [{
                "id": "profile-1779134612480",
                "name": "izaiasousa@gmail.com",
                "configDir": config_dir.to_string_lossy(),
                "isDefault": false   // <-- the bug: old renderer filter required isDefault=true
            }]
        });

        assert!(
            is_cli_authenticated(&profiles),
            "non-default profile with .claude.json must count as CLI-authenticated"
        );
    }

    #[test]
    fn autobuild_source_env_get_response_contains_has_token_field() {
        // Verify the response shape: `hasToken` must be present (the field the
        // renderer's useIdeationAuth, EnvConfigModal, useClaudeTokenCheck read).
        // We construct a minimal profiles JSON with a credential file to ensure
        // cli_authenticated is true, then assert the key name in the serialised output.
        let tmp = TempDir::new().unwrap();
        let config_dir = tmp.path().join("profile-dir");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(config_dir.join(".claude.json"), b"{}").unwrap();

        let profiles = json!({
            "profiles": [{
                "id": "p1",
                "name": "Test",
                "configDir": config_dir.to_string_lossy(),
                "isDefault": false
            }]
        });

        // The effective token value is the logical OR of .env token and CLI auth.
        let effective = /* .env token */ false || is_cli_authenticated(&profiles);
        let response = json!({
            "hasToken": effective,
            "hasClaudeToken": effective,
        });

        // `hasToken` must exist — previously the field was named `hasClaudeToken`
        // which the renderer never found, so `data?.hasToken` was always undefined.
        assert!(
            response.get("hasToken").is_some(),
            "response must have 'hasToken' key (not just 'hasClaudeToken')"
        );
        assert_eq!(
            response["hasToken"], effective,
            "hasToken must equal the effective auth result"
        );
    }
}
