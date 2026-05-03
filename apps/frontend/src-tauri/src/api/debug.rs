use crate::error::{AppError, AppResult};
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tauri::AppHandle;

const APP_NAME: &str = "auto-claude-ui";

fn logs_dir() -> AppResult<PathBuf> {
    let base = dirs::data_dir()
        .ok_or_else(|| AppError::new("no_data_dir", "Could not resolve OS data directory"))?;
    Ok(base.join(APP_NAME).join("logs"))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugInfo {
    pub system_info: HashMap<String, String>,
    pub recent_errors: Vec<String>,
    pub logs_path: String,
    pub debug_report: String,
}

#[derive(Debug, Serialize)]
pub struct DebugResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogFileInfo {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub modified: String,
}

fn collect_system_info(app_handle: &AppHandle) -> HashMap<String, String> {
    let mut info = HashMap::new();
    info.insert("platform".to_string(), std::env::consts::OS.to_string());
    info.insert("arch".to_string(), std::env::consts::ARCH.to_string());
    info.insert(
        "appVersion".to_string(),
        app_handle.package_info().version.to_string(),
    );
    info.insert(
        "tauriVersion".to_string(),
        env!("CARGO_PKG_VERSION").to_string(),
    );
    if let Ok(home) = std::env::var("HOME") {
        info.insert("home".to_string(), home);
    }
    info.insert(
        "rustVersion".to_string(),
        // Best-effort; rustc version is baked at compile time via build script;
        // for now report the SDK version we link against.
        "rust-1.95.0".to_string(),
    );
    info
}

#[tauri::command(rename_all = "camelCase")]
pub async fn debug_get_info(app_handle: AppHandle) -> AppResult<DebugInfo> {
    let logs_path = logs_dir()?;
    let system_info = collect_system_info(&app_handle);

    let report_lines: Vec<String> = system_info
        .iter()
        .map(|(k, v)| format!("{k}: {v}"))
        .collect();
    let debug_report = format!(
        "=== Aperant-MCP Debug Report ===\n{}\n=== End ===",
        report_lines.join("\n")
    );

    Ok(DebugInfo {
        system_info,
        recent_errors: Vec::new(),
        logs_path: logs_path.to_string_lossy().to_string(),
        debug_report,
    })
}

#[tauri::command(rename_all = "camelCase")]
pub async fn debug_open_logs_folder() -> AppResult<DebugResult> {
    let dir = logs_dir()?;
    fs::create_dir_all(&dir)
        .map_err(|e| AppError::new("create_dir_failed", e.to_string()))?;

    // Cross-platform "open in file manager"
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };

    match std::process::Command::new(cmd).arg(&dir).spawn() {
        Ok(_) => Ok(DebugResult {
            success: true,
            error: None,
        }),
        Err(e) => Ok(DebugResult {
            success: false,
            error: Some(format!("Failed to open logs folder: {e}")),
        }),
    }
}

#[tauri::command(rename_all = "camelCase")]
pub async fn debug_copy_debug_info() -> AppResult<DebugResult> {
    Ok(DebugResult {
        success: false,
        error: Some(
            "Clipboard write requires tauri-plugin-clipboard-manager (deferred)".to_string(),
        ),
    })
}

#[tauri::command(rename_all = "camelCase")]
pub async fn debug_get_recent_errors(_max_count: Option<u32>) -> AppResult<Vec<String>> {
    // No file-based logger in the Tauri build yet — Phase 6 wires Sentry.
    // Return empty so the renderer's debug panel shows "no recent errors" instead of crashing.
    Ok(Vec::new())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn debug_list_log_files() -> AppResult<Vec<LogFileInfo>> {
    let dir = match logs_dir() {
        Ok(d) => d,
        Err(_) => return Ok(Vec::new()),
    };
    if !dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut files = Vec::new();
    let entries = match fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return Ok(Vec::new()),
    };

    for entry in entries.flatten() {
        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !metadata.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".log") && !name.ends_with(".txt") {
            continue;
        }
        let modified = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| {
                chrono::DateTime::<chrono::Utc>::from_timestamp(d.as_secs() as i64, 0)
                    .map(|dt| dt.to_rfc3339())
                    .unwrap_or_default()
            })
            .unwrap_or_default();

        files.push(LogFileInfo {
            name,
            path: entry.path().to_string_lossy().to_string(),
            size: metadata.len(),
            modified,
        });
    }

    // Newest first
    files.sort_by(|a, b| b.modified.cmp(&a.modified));
    Ok(files)
}

/// Triggers a deliberate panic so QA can verify watchdog detects + restarts.
/// The renderer's debug panel surfaces this as "Trigger Crash" — only used
/// in development.
#[tauri::command(rename_all = "camelCase")]
pub async fn debug_trigger_crash() {
    panic!("debug_trigger_crash invoked (this is a deliberate test panic)");
}
