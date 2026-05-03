use crate::api::settings;
use crate::error::{AppError, AppResult};
use crate::types::IpcResult;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;
use tokio::process::Command;

const NPM_PACKAGE: &str = "@anthropic-ai/claude-code";

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
            (home.join(".nvm/versions/node").join("current/bin/claude"), "nvm"),
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

async fn detect_version(path: &PathBuf) -> Option<String> {
    let output = Command::new(path).arg("--version").output().await.ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.split_whitespace().next().map(String::from)
}

async fn fetch_latest_npm_version() -> AppResult<String> {
    let output = Command::new("npm")
        .args(["view", NPM_PACKAGE, "version"])
        .output()
        .await
        .map_err(|e| AppError::new("npm_spawn_failed", e.to_string()))?;

    if !output.status.success() {
        return Err(AppError::new(
            "npm_view_failed",
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn is_outdated_semver(installed: &str, latest: &str) -> bool {
    let parse = |s: &str| -> Vec<u32> {
        s.split('.').filter_map(|x| x.parse().ok()).collect()
    };
    let i = parse(installed);
    let l = parse(latest);
    if i.is_empty() || l.is_empty() {
        return false;
    }
    i < l
}

fn get_active_path_from_settings() -> Option<String> {
    let path = settings::settings_path().ok()?;
    let s = settings::read_settings_at(&path);
    s.get("claudePath")
        .and_then(|v| v.as_str())
        .map(String::from)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn claude_code_check_version() -> AppResult<IpcResult<ClaudeCodeVersionInfo>> {
    let mut installed_version: Option<String> = None;
    let mut found_path: Option<PathBuf> = None;
    let mut detected_source = "fallback";

    for (path, src) in candidate_paths() {
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

    for (path, source) in candidate_paths() {
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
    let output = Command::new("npm")
        .args(["view", NPM_PACKAGE, "versions", "--json"])
        .output()
        .await
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
    fn install_command_format() {
        let cmd = format!("npm install -g {NPM_PACKAGE}");
        assert!(cmd.contains("@anthropic-ai/claude-code"));
        assert!(cmd.starts_with("npm install -g"));
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
        // Every OS we target has at least one candidate
        if cfg!(any(target_os = "macos", target_os = "linux", target_os = "windows")) {
            assert!(!paths.is_empty());
        }
    }
}
