use crate::api::settings;
use crate::error::{AppError, AppResult};
use crate::types::IpcResult;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

const NPM_PACKAGE: &str = "@anthropic-ai/claude-code";
/// `claude --version` should return in well under a second on a healthy install.
/// 5s is generous and protects against malicious wrapper scripts that block
/// indefinitely on stdin or sleep forever.
const CLAUDE_VERSION_TIMEOUT: Duration = Duration::from_secs(5);
/// `npm view` over a slow network or behind a corporate proxy can take a few
/// seconds; 30s is the upper bound before we give up and report "unknown".
const NPM_VIEW_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDetectionResult {
    pub found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub source: String,
    pub message: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeCodeVersionInfo {
    pub installed: Option<String>,
    pub latest: String,
    pub is_outdated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub detection_result: ToolDetectionResult,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeCodeVersionList {
    pub versions: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeInstallationInfo {
    pub path: String,
    pub version: Option<String>,
    pub source: String,
    pub is_active: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeInstallationList {
    pub installations: Vec<ClaudeInstallationInfo>,
    pub active_path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct InstallCommand {
    pub command: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallVersionCommand {
    pub command: String,
    pub version: String,
}

#[derive(Debug, Serialize)]
pub struct SetActivePathResult {
    pub path: String,
}

fn candidate_paths() -> Vec<(PathBuf, &'static str)> {
    let home = dirs::home_dir().unwrap_or_default();
    let mut paths: Vec<(PathBuf, &'static str)> = Vec::new();

    if cfg!(target_os = "macos") {
        paths.extend([
            (PathBuf::from("/opt/homebrew/bin/claude"), "homebrew"),
            (PathBuf::from("/usr/local/bin/claude"), "system-path"),
            (home.join(".npm-global/bin/claude"), "system-path"),
            (
                home.join(".nvm/versions/node").join("current/bin/claude"),
                "nvm",
            ),
        ]);
    } else if cfg!(target_os = "linux") {
        paths.extend([
            (PathBuf::from("/usr/local/bin/claude"), "system-path"),
            (PathBuf::from("/usr/bin/claude"), "system-path"),
            (home.join(".npm-global/bin/claude"), "system-path"),
        ]);
    } else if cfg!(target_os = "windows") {
        if let Ok(appdata) = std::env::var("APPDATA") {
            paths.push((
                PathBuf::from(appdata).join("npm").join("claude.cmd"),
                "system-path",
            ));
        }
    }

    paths
}

/// Returns the saved `claudePath` from settings.json, or None if not configured.
fn get_active_path_from_settings() -> Option<String> {
    let path = settings::settings_path().ok()?;
    let s = settings::read_settings_at(&path);
    s.get("claudePath")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// All paths to check, in priority order: user-configured first, then candidates.
/// This is the fix for the bug Codex flagged where `set_active_path` saved a path
/// but `check_version` ignored it entirely.
fn paths_to_probe() -> Vec<(PathBuf, &'static str)> {
    let mut out: Vec<(PathBuf, &'static str)> = Vec::new();
    if let Some(active) = get_active_path_from_settings() {
        out.push((PathBuf::from(active), "user-config"));
    }
    out.extend(candidate_paths());
    out
}

/// Extracts a semver-shaped version from a free-form `--version` output line.
/// Tries each whitespace-separated token (stripping a leading `v`) and returns
/// the first one that parses as a valid semver. This avoids the bug where
/// `claude-code version 1.2.3` returned `claude-code` as the version.
fn extract_version(stdout: &str) -> Option<String> {
    stdout.split_whitespace().find_map(|token| {
        let candidate = token.trim_start_matches('v').trim_end_matches([',', ';', ')']);
        semver::Version::parse(candidate)
            .ok()
            .map(|_| candidate.to_string())
    })
}

/// Runs `<path> --version` with a hard timeout. Returns None on any failure
/// (process spawn error, non-zero exit, no parseable version, or timeout).
async fn detect_version(path: &PathBuf) -> Option<String> {
    let cmd = Command::new(path).arg("--version").output();
    let output = timeout(CLAUDE_VERSION_TIMEOUT, cmd).await.ok()?.ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    extract_version(&stdout)
}

async fn fetch_latest_npm_version() -> AppResult<String> {
    let cmd = Command::new("npm")
        .args(["view", NPM_PACKAGE, "version"])
        .output();
    let output = timeout(NPM_VIEW_TIMEOUT, cmd)
        .await
        .map_err(|_| AppError::new("npm_timeout", "npm view timed out after 30s"))?
        .map_err(|e| AppError::new("npm_spawn_failed", e.to_string()))?;

    if !output.status.success() {
        return Err(AppError::new(
            "npm_view_failed",
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Strict semver compare. Falls back to false when either side is unparseable.
fn is_outdated_semver(installed: &str, latest: &str) -> bool {
    let installed = installed.trim_start_matches('v');
    let latest = latest.trim_start_matches('v');
    match (
        semver::Version::parse(installed),
        semver::Version::parse(latest),
    ) {
        (Ok(i), Ok(l)) => i < l,
        _ => false,
    }
}

#[tauri::command(rename_all = "camelCase")]
pub async fn claude_code_check_version() -> AppResult<IpcResult<ClaudeCodeVersionInfo>> {
    let mut installed_version: Option<String> = None;
    let mut found_path: Option<PathBuf> = None;
    let mut detected_source = "fallback";

    for (path, src) in paths_to_probe() {
        if !path.exists() {
            continue;
        }
        if let Some(v) = detect_version(&path).await {
            installed_version = Some(v);
            found_path = Some(path);
            detected_source = src;
            break;
        }
    }

    let latest = fetch_latest_npm_version()
        .await
        .unwrap_or_else(|_| "unknown".to_string());

    let is_outdated = match (&installed_version, latest.as_str()) {
        (Some(i), l) if l != "unknown" => is_outdated_semver(i, l),
        _ => false,
    };

    let detection_result = ToolDetectionResult {
        found: installed_version.is_some(),
        path: found_path.as_ref().map(|p| p.to_string_lossy().to_string()),
        version: installed_version.clone(),
        source: detected_source.to_string(),
        message: if installed_version.is_some() {
            "Claude CLI detected".to_string()
        } else {
            "Claude CLI not found".to_string()
        },
    };

    Ok(IpcResult::ok(ClaudeCodeVersionInfo {
        installed: installed_version,
        latest,
        is_outdated,
        path: found_path.map(|p| p.to_string_lossy().to_string()),
        detection_result,
    }))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn claude_code_get_installations() -> AppResult<IpcResult<ClaudeInstallationList>> {
    let active_path = get_active_path_from_settings();
    let mut installations = Vec::new();

    for (path, source) in paths_to_probe() {
        if !path.exists() {
            continue;
        }
        let path_str = path.to_string_lossy().to_string();
        let version = detect_version(&path).await;
        let is_active = active_path.as_deref() == Some(&path_str);
        installations.push(ClaudeInstallationInfo {
            path: path_str,
            version,
            source: source.to_string(),
            is_active,
        });
    }

    Ok(IpcResult::ok(ClaudeInstallationList {
        installations,
        active_path,
    }))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn claude_code_get_versions() -> AppResult<IpcResult<ClaudeCodeVersionList>> {
    let cmd = Command::new("npm")
        .args(["view", NPM_PACKAGE, "versions", "--json"])
        .output();
    let output = timeout(NPM_VIEW_TIMEOUT, cmd)
        .await
        .map_err(|_| AppError::new("npm_timeout", "npm view versions timed out after 30s"))?
        .map_err(|e| AppError::new("npm_spawn_failed", e.to_string()))?;

    if !output.status.success() {
        return Err(AppError::new(
            "npm_view_versions_failed",
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut versions: Vec<String> = serde_json::from_str(&stdout)
        .map_err(|e| AppError::new("npm_parse_failed", e.to_string()))?;
    versions.reverse();

    Ok(IpcResult::ok(ClaudeCodeVersionList { versions }))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn claude_code_install() -> AppResult<IpcResult<InstallCommand>> {
    Ok(IpcResult::ok(InstallCommand {
        command: format!("npm install -g {NPM_PACKAGE}"),
    }))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn claude_code_install_version(
    version: String,
) -> AppResult<IpcResult<InstallVersionCommand>> {
    Ok(IpcResult::ok(InstallVersionCommand {
        command: format!("npm install -g {NPM_PACKAGE}@{version}"),
        version,
    }))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn claude_code_set_active_path(
    cli_path: String,
) -> AppResult<IpcResult<SetActivePathResult>> {
    let path = settings::settings_path()?;
    let mut current = settings::read_settings_at(&path);
    settings::shallow_merge(&mut current, json!({ "claudePath": &cli_path }));
    settings::write_settings_at(&path, &current)?;
    Ok(IpcResult::ok(SetActivePathResult { path: cli_path }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_compare_lower_is_outdated() {
        assert!(is_outdated_semver("1.2.3", "1.2.4"));
        assert!(is_outdated_semver("1.2.9", "1.3.0"));
        assert!(is_outdated_semver("1.9.9", "2.0.0"));
    }

    #[test]
    fn semver_compare_equal_is_not_outdated() {
        assert!(!is_outdated_semver("1.2.3", "1.2.3"));
    }

    #[test]
    fn semver_compare_higher_is_not_outdated() {
        assert!(!is_outdated_semver("2.0.0", "1.9.9"));
    }

    #[test]
    fn semver_compare_handles_garbage_input() {
        assert!(!is_outdated_semver("not-a-version", "1.0.0"));
        assert!(!is_outdated_semver("1.0.0", "not-a-version"));
        assert!(!is_outdated_semver("", ""));
    }

    #[test]
    fn semver_compare_handles_prereleases_correctly() {
        // Per semver spec: 1.0.0-alpha < 1.0.0-beta < 1.0.0
        assert!(is_outdated_semver("1.0.0-alpha", "1.0.0-beta"));
        assert!(is_outdated_semver("1.0.0-alpha", "1.0.0"));
        assert!(is_outdated_semver("1.0.0-rc.1", "1.0.0"));
        assert!(!is_outdated_semver("1.0.0", "1.0.0-rc.1"));
    }

    #[test]
    fn semver_compare_strips_v_prefix() {
        assert!(is_outdated_semver("v1.2.3", "v1.2.4"));
        assert!(is_outdated_semver("v1.2.3", "1.2.4"));
        assert!(is_outdated_semver("1.2.3", "v1.2.4"));
    }

    #[test]
    fn extract_version_from_typical_output() {
        // The bug Codex flagged: "claude-code version 1.2.3" was returning "claude-code"
        assert_eq!(
            extract_version("claude-code version 1.2.3"),
            Some("1.2.3".to_string())
        );
    }

    #[test]
    fn extract_version_from_bare_version() {
        assert_eq!(extract_version("1.2.3"), Some("1.2.3".to_string()));
        assert_eq!(extract_version("1.2.3\n"), Some("1.2.3".to_string()));
    }

    #[test]
    fn extract_version_strips_v_prefix() {
        assert_eq!(extract_version("v1.2.3"), Some("1.2.3".to_string()));
        assert_eq!(
            extract_version("claude v1.2.3 (build abc)"),
            Some("1.2.3".to_string())
        );
    }

    #[test]
    fn extract_version_handles_prerelease() {
        assert_eq!(
            extract_version("1.0.0-rc.1 (Claude Code)"),
            Some("1.0.0-rc.1".to_string())
        );
        assert_eq!(
            extract_version("v2.0.0-beta.5"),
            Some("2.0.0-beta.5".to_string())
        );
    }

    #[test]
    fn extract_version_returns_none_when_no_semver() {
        assert_eq!(extract_version("not a version"), None);
        assert_eq!(extract_version(""), None);
        assert_eq!(extract_version("v"), None);
    }

    #[test]
    fn install_command_format() {
        let cmd = format!("npm install -g {NPM_PACKAGE}");
        assert_eq!(cmd, "npm install -g @anthropic-ai/claude-code");
    }

    #[test]
    fn install_version_command_format() {
        let version = "1.2.3";
        let cmd = format!("npm install -g {NPM_PACKAGE}@{version}");
        assert_eq!(cmd, "npm install -g @anthropic-ai/claude-code@1.2.3");
    }

    #[test]
    fn candidate_paths_per_os_returns_some_entries() {
        let paths = candidate_paths();
        if cfg!(any(target_os = "macos", target_os = "linux", target_os = "windows")) {
            assert!(!paths.is_empty());
        }
    }
}
