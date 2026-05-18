//! Diagnostics domain — reports usage-monitor + RDR internals to the
//! Diagnostics panel in the Tauri build.
//!
//! Phase 6a: reads the files that the Electron build writes to disk so the
//! diagnostics UI shows real data without needing to port the full usage-
//! monitor or RDR subsystem.
//!
//! Still deferred (stubs remain):
//!   - forceUsageFetch: live API refetch (usage-monitor not ported)
//!   - sendTestRdr: output-monitor + MCP busy check (not ported)
//!   - output_monitor_state / mcp_busy in RdrDiagnostics

use crate::error::AppResult;
use crate::types::IpcResult;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

// ── shared helpers ────────────────────────────────────────────────────────────

const APP_NAME: &str = "auto-claude-ui";

/// `~/Library/Application Support/auto-claude-ui/` on macOS,
/// `%APPDATA%\auto-claude-ui\` on Windows,
/// `~/.config/auto-claude-ui/` on Linux.
/// Mirrors Electron's `app.getPath('userData')`.
fn app_data_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join(APP_NAME))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Reads a JSON file and parses it into `T`. Returns `None` on any I/O or
/// parse failure so callers can fall back to defaults gracefully.
fn read_json_file<T: for<'de> Deserialize<'de>>(path: &PathBuf) -> Option<T> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

// ── data types ────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDiagnostics {
    pub current_usage: serde_json::Value,
    pub last_good_usage: serde_json::Value,
    pub current_usage_profile_id: Option<String>,
    pub api_failure_timestamps: serde_json::Value,
    pub last_emit_timestamp: u64,
    pub last_rdr_notification_state: String,
    pub is_checking: bool,
    pub last_good_usage_path: String,
    pub cli_credential_path: String,
    pub cli_credential_exists: bool,
    pub cli_credential_has_token: bool,
    pub timestamp: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RdrPauseState {
    pub paused: bool,
    pub warning: bool,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub paused_at: u64,
    #[serde(default)]
    pub rate_limit_reset_at: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RdrDiagnostics {
    pub rdr_pause_state: RdrPauseState,
    pub output_monitor_state: String,
    pub mcp_busy: bool,
    pub is_claude_code_busy: bool,
    pub timestamp: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestRdrResult {
    pub would_send: bool,
    pub busy_check_result: bool,
    pub message: String,
}

// ── commands ──────────────────────────────────────────────────────────────────

#[tauri::command(rename_all = "camelCase")]
pub async fn diag_get_usage_state() -> AppResult<IpcResult<UsageDiagnostics>> {
    // Usage snapshot written by Electron's usage-monitor.
    let snapshot_path = app_data_dir()
        .map(|d| d.join("last-usage-snapshot.json"))
        .unwrap_or_default();

    let snapshot: serde_json::Value = snapshot_path
        .exists()
        .then(|| read_json_file::<serde_json::Value>(&snapshot_path))
        .flatten()
        .unwrap_or(serde_json::Value::Null);

    // Extract profile ID from the snapshot if the field is present.
    let profile_id = snapshot
        .get("profileId")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    // CLI credentials written by claude-code to ~/.claude/.credentials.json.
    let cred_path = dirs::home_dir()
        .map(|h| h.join(".claude").join(".credentials.json"))
        .unwrap_or_default();

    let cred_exists = cred_path.exists();
    let cred_has_token = cred_exists
        && read_json_file::<serde_json::Value>(&cred_path)
            .and_then(|v| {
                v.get("claudeAiOauth")
                    .and_then(|o| o.get("accessToken"))
                    .and_then(|t| t.as_str())
                    .map(|t| !t.is_empty())
            })
            .unwrap_or(false);

    Ok(IpcResult::ok(UsageDiagnostics {
        current_usage: snapshot.clone(),
        last_good_usage: snapshot,
        current_usage_profile_id: profile_id,
        api_failure_timestamps: serde_json::json!({}),
        last_emit_timestamp: 0,
        last_rdr_notification_state: "not-tracked-yet".to_string(),
        is_checking: false,
        last_good_usage_path: snapshot_path.to_string_lossy().into_owned(),
        cli_credential_path: cred_path.to_string_lossy().into_owned(),
        cli_credential_exists: cred_exists,
        cli_credential_has_token: cred_has_token,
        timestamp: now_secs(),
    }))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn diag_get_rdr_state() -> AppResult<IpcResult<RdrDiagnostics>> {
    let pause_path = app_data_dir()
        .map(|d| d.join("rdr-pause.json"))
        .unwrap_or_default();

    let mut pause_state: RdrPauseState = pause_path
        .exists()
        .then(|| read_json_file::<RdrPauseState>(&pause_path))
        .flatten()
        .unwrap_or_default();

    // Auto-expiry: if the rate-limit reset time has already passed, clear the pause.
    if pause_state.paused
        && pause_state.rate_limit_reset_at > 0
        && now_ms() > pause_state.rate_limit_reset_at
    {
        pause_state.paused = false;
        pause_state.warning = false;
    }

    Ok(IpcResult::ok(RdrDiagnostics {
        rdr_pause_state: pause_state,
        // Output monitor and MCP monitor are not ported to Rust yet.
        output_monitor_state: "not-ported".to_string(),
        mcp_busy: false,
        is_claude_code_busy: false,
        timestamp: now_secs(),
    }))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn diag_force_usage_fetch() -> AppResult<IpcResult<serde_json::Value>> {
    // The usage-monitor that fetches live data from the Claude API is not ported
    // to Rust yet. Return the last cached snapshot so the UI at least refreshes
    // its display with whatever is on disk.
    let snapshot_path = app_data_dir()
        .map(|d| d.join("last-usage-snapshot.json"))
        .unwrap_or_default();

    let last_snapshot: serde_json::Value = snapshot_path
        .exists()
        .then(|| read_json_file::<serde_json::Value>(&snapshot_path))
        .flatten()
        .unwrap_or(serde_json::Value::Null);

    Ok(IpcResult::ok(serde_json::json!({
        "deferred": true,
        "reason": "usage-monitor-not-ported",
        "lastSnapshot": last_snapshot,
    })))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn diag_send_test_rdr() -> AppResult<IpcResult<TestRdrResult>> {
    // The output-monitor and MCP-monitor that determine Claude Code busyness are
    // not ported to Rust. Report as idle (would send) so the test button gives
    // useful feedback rather than always showing "blocked".
    Ok(IpcResult::ok(TestRdrResult {
        would_send: true,
        busy_check_result: false,
        message: "RDR would send (output monitor not ported in Tauri)".to_string(),
    }))
}
