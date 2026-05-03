//! Diagnostics domain — reports usage-monitor + RDR internals to the
//! Diagnostics panel in the Tauri build.
//!
//! All four endpoints depend on subsystems that are not yet ported:
//!   - getUsageState reads the Claude profile usage monitor (Phase 4 terminal)
//!   - getRdrState reads RDR pause state + MCP busy flag (Phase 5 RDR port)
//!   - forceUsageFetch triggers a refetch through the usage monitor
//!   - sendTestRdr exercises the RDR send pipeline
//!
//! Stub each with a zero-state response so the diagnostics UI renders without
//! crashing. Round 4-light intentionally does not pull these subsystems
//! forward — they have their own dedicated phases.

use crate::error::AppResult;
use crate::types::IpcResult;
use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};

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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RdrPauseState {
    pub paused: bool,
    pub warning: bool,
    pub reason: String,
    pub paused_at: u64,
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

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn empty_usage_diagnostics() -> UsageDiagnostics {
    UsageDiagnostics {
        current_usage: serde_json::Value::Null,
        last_good_usage: serde_json::Value::Null,
        current_usage_profile_id: None,
        api_failure_timestamps: serde_json::json!({}),
        last_emit_timestamp: 0,
        last_rdr_notification_state: "not-tracked-yet".to_string(),
        is_checking: false,
        last_good_usage_path: String::new(),
        cli_credential_path: String::new(),
        cli_credential_exists: false,
        cli_credential_has_token: false,
        timestamp: now_secs(),
    }
}

fn empty_rdr_diagnostics() -> RdrDiagnostics {
    RdrDiagnostics {
        rdr_pause_state: RdrPauseState {
            paused: false,
            warning: false,
            reason: "rdr-subsystem-not-ported-yet".to_string(),
            paused_at: 0,
            rate_limit_reset_at: 0,
        },
        output_monitor_state: "not-running".to_string(),
        mcp_busy: false,
        is_claude_code_busy: false,
        timestamp: now_secs(),
    }
}

#[tauri::command(rename_all = "camelCase")]
pub async fn diag_get_usage_state() -> AppResult<IpcResult<UsageDiagnostics>> {
    Ok(IpcResult::ok(empty_usage_diagnostics()))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn diag_get_rdr_state() -> AppResult<IpcResult<RdrDiagnostics>> {
    Ok(IpcResult::ok(empty_rdr_diagnostics()))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn diag_force_usage_fetch() -> AppResult<IpcResult<serde_json::Value>> {
    Ok(IpcResult::ok(serde_json::json!({
        "deferred": true,
        "reason": "usage-monitor-not-ported-yet",
    })))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestRdrResult {
    pub would_send: bool,
    pub busy_check_result: bool,
    pub message: String,
}

#[tauri::command(rename_all = "camelCase")]
pub async fn diag_send_test_rdr() -> AppResult<IpcResult<TestRdrResult>> {
    Ok(IpcResult::ok(TestRdrResult {
        would_send: false,
        busy_check_result: false,
        message: "RDR subsystem not ported in Tauri build yet".to_string(),
    }))
}
