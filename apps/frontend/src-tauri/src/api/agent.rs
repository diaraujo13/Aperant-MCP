//! Agent execution subsystem (Phase 5 + Phase 6c).
//!
//! Spawns apps/backend/run.py as a child process managed by tokio's async
//! runtime. stdout/stderr stream to the renderer via Tauri events so the UI
//! thread is never blocked. agent_start returns immediately after spawning.
//!
//! Phase 6c adds:
//!   - Active profile env injection at spawn time
//!   - Rate-limit detection on output streams
//!   - Auto-switch to next profile (capped at 3 switches)
//!
//! Deferred (stubs remain elsewhere):
//!   - Worktree management
//!   - Agent queue ordering / priority
//!   - IDE integration
//!   - Agent log persistence

use crate::agent::manager::{AgentManager, RunningAgent};
use crate::agent::profile_env::{resolve_profile_env, ResolvedProfile};
use crate::agent::rate_limit::detect_rate_limit;
use crate::types::IpcResult;
use serde_json::json;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use std::time::SystemTime;
use tauri::{AppHandle, Emitter, State};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Mutex;
use tracing::{info, warn};

pub type SharedAgentManager = Arc<Mutex<AgentManager>>;

/// Max total profile attempts per task (initial + 3 respawns). Tuned so a
/// typical setup of 1 active + 1 fallback Anthropic + 1 Codex fallback
/// exhausts cleanly.
const MAX_PROFILE_ATTEMPTS: usize = 4;

/// Resolves the Python interpreter to use for the given project root.
pub(crate) fn resolve_python(project_path: &Path) -> Option<PathBuf> {
    let bin_dir = if cfg!(windows) { "Scripts" } else { "bin" };
    let py_name = if cfg!(windows) {
        "python.exe"
    } else {
        "python"
    };

    let venv_candidates = [
        project_path
            .join("apps")
            .join("backend")
            .join(".venv")
            .join(bin_dir)
            .join(py_name),
        project_path.join(".venv").join(bin_dir).join(py_name),
    ];

    for candidate in &venv_candidates {
        if candidate.exists() {
            return Some(candidate.clone());
        }
    }

    for name in ["python3", "python"] {
        let ok = std::process::Command::new(name)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return Some(PathBuf::from(name));
        }
    }

    None
}

/// Returns true if any profiles exist at all (used to decide whether to error
/// out vs spawn unauthenticated).
pub(crate) fn any_profiles_configured() -> bool {
    let api = crate::api::profiles::read_api_profiles();
    let oauth = crate::api::profiles::read_profiles();
    let api_count = api
        .get("profiles")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let oauth_count = oauth
        .get("profiles")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    api_count + oauth_count > 0
}

/// Strips the *other* vendor's env vars from the inherited environment before
/// injecting the resolved profile's vars. Mirrors apps/backend/core/auth.py
/// which removes CLAUDE_CODE_OAUTH_TOKEN when ANTHROPIC_BASE_URL is set.
/// Without this, a stale parent env (e.g. shell-exported OAuth token) leaks
/// into an API-profile run and silently overrides the active profile.
pub(crate) fn apply_profile_env(
    cmd: &mut tokio::process::Command,
    rp: &crate::agent::profile_env::ResolvedProfile,
) {
    use crate::agent::profile_env::ProfileKind;
    // Codex env vars — stripped from non-Codex arms so a stale Codex run
    // doesn't leak OPENAI_API_KEY / provider sentinel into Anthropic spawns.
    const CODEX_VARS: &[&str] = &[
        "OPENAI_API_KEY",
        "APERANT_AI_PROVIDER",
        "APERANT_CODEX_CLI_PATH",
        "AUTO_CLAUDE_PROVIDER",
        "AUTO_CLAUDE_CODEX_MODEL",
        "AUTO_CLAUDE_CODEX_BINARY",
    ];
    match rp.profile_kind {
        ProfileKind::Api => {
            cmd.env_remove("CLAUDE_CODE_OAUTH_TOKEN");
            for k in CODEX_VARS {
                cmd.env_remove(k);
            }
        }
        ProfileKind::OAuth => {
            cmd.env_remove("ANTHROPIC_BASE_URL");
            cmd.env_remove("ANTHROPIC_AUTH_TOKEN");
            cmd.env_remove("ANTHROPIC_MODEL");
            for k in CODEX_VARS {
                cmd.env_remove(k);
            }
        }
        ProfileKind::Codex => {
            cmd.env_remove("CLAUDE_CODE_OAUTH_TOKEN");
            cmd.env_remove("ANTHROPIC_BASE_URL");
            cmd.env_remove("ANTHROPIC_AUTH_TOKEN");
            cmd.env_remove("ANTHROPIC_MODEL");
        }
    }
    for (k, v) in &rp.env {
        cmd.env(k, v);
    }
}

/// Shared spawn logic. `excluded_profiles` carries profile ids that have already
/// been attempted (and rate-limited) for this task.
fn do_spawn(
    task_id: String,
    project_path: String,
    spec_id: String,
    recover: bool,
    manager: SharedAgentManager,
    app: AppHandle,
    excluded_profiles: Vec<String>,
) -> Pin<Box<dyn Future<Output = IpcResult<serde_json::Value>> + Send>> {
    Box::pin(async move {
        let project_path_buf = PathBuf::from(&project_path);

        let agents_arc = {
            let mgr = match manager.try_lock() {
                Ok(g) => g,
                Err(_) => {
                    return IpcResult {
                        success: false,
                        data: None,
                        error: Some("internal_error".to_string()),
                    }
                }
            };
            mgr.agents.clone()
        };

        {
            let map = agents_arc.lock().await;
            if map.contains_key(&task_id) {
                return IpcResult {
                    success: false,
                    data: None,
                    error: Some("already_running".to_string()),
                };
            }
        }

        let python = match resolve_python(&project_path_buf) {
            Some(p) => p,
            None => {
                warn!("[agent] python not found for task {}", task_id);
                return IpcResult {
                    success: false,
                    data: None,
                    error: Some("python_not_found".to_string()),
                };
            }
        };

        let run_py = project_path_buf.join("apps").join("backend").join("run.py");

        let resolved: Option<ResolvedProfile> = resolve_profile_env(&excluded_profiles);
        if resolved.is_none() && any_profiles_configured() {
            return IpcResult {
                success: false,
                data: None,
                error: Some("no_profiles_available".to_string()),
            };
        }

        let mut cmd = tokio::process::Command::new(&python);
        cmd.arg(&run_py)
            .arg("--spec")
            .arg(&spec_id)
            .arg("--project")
            .arg(&project_path_buf)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(false);

        if let Some(rp) = &resolved {
            apply_profile_env(&mut cmd, rp);
        }

        if recover {
            cmd.arg("--recover");
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                warn!("[agent] spawn failed for task {}: {}", task_id, e);
                return IpcResult {
                    success: false,
                    data: None,
                    error: Some(format!("spawn_failed: {e}")),
                };
            }
        };

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        // Rate-limit channel: stdout/stderr readers send () on detection.
        let (rl_tx, mut rl_rx) = tokio::sync::mpsc::channel::<()>(4);

        if let Some(stdout) = stdout {
            let app_c = app.clone();
            let tid = task_id.clone();
            let rl = rl_tx.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if detect_rate_limit(&line) {
                        let _ = rl.try_send(());
                    }
                    let _ = app_c.emit(
                        "agent:output",
                        json!({ "taskId": tid, "stream": "stdout", "data": line }),
                    );
                }
            });
        }

        if let Some(stderr) = stderr {
            let app_c = app.clone();
            let tid = task_id.clone();
            let rl = rl_tx.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if detect_rate_limit(&line) {
                        let _ = rl.try_send(());
                    }
                    let _ = app_c.emit(
                        "agent:output",
                        json!({ "taskId": tid, "stream": "stderr", "data": line }),
                    );
                }
            });
        }
        drop(rl_tx);

        let (kill_tx, kill_rx) = tokio::sync::oneshot::channel::<()>();

        // Build attempted list for this attempt.
        let mut attempted = excluded_profiles.clone();
        let current_profile_id = resolved.as_ref().map(|r| r.profile_id.clone());
        if let Some(id) = &current_profile_id {
            if !attempted.iter().any(|e| e == id) {
                attempted.push(id.clone());
            }
        }

        // Monitor task: handles natural exit, kill signal, or rate-limit switch.
        {
            let agents_arc2 = agents_arc.clone();
            let tid = task_id.clone();
            let app_c = app.clone();
            let manager_c = manager.clone();
            let project_path_c = project_path.clone();
            let spec_id_c = spec_id.clone();
            let attempted_c = attempted.clone();
            let from_profile = current_profile_id.clone();
            tokio::spawn(async move {
                tokio::select! {
                    result = child.wait() => {
                        let code = result.ok().and_then(|s| s.code());
                        let state = if code == Some(0) { "stopped" } else { "crashed" };
                        let _ = app_c.emit("agent:state", json!({ "taskId": tid, "state": state }));
                        let _ = app_c.emit("agent:exit", json!({ "taskId": tid, "code": code }));
                        agents_arc2.lock().await.remove(&tid);
                    }
                    _ = kill_rx => {
                        let _ = child.kill().await;
                        let _ = app_c.emit("agent:state", json!({ "taskId": tid, "state": "stopped" }));
                        let _ = app_c.emit("agent:exit", json!({ "taskId": tid, "code": serde_json::Value::Null }));
                        agents_arc2.lock().await.remove(&tid);
                    }
                    Some(_) = rl_rx.recv() => {
                        let _ = child.kill().await;
                        // Remove the current entry so do_spawn can re-register.
                        agents_arc2.lock().await.remove(&tid);

                        if attempted_c.len() >= MAX_PROFILE_ATTEMPTS {
                            let _ = app_c.emit("agent:state", json!({ "taskId": tid, "state": "rate_limited" }));
                            let _ = app_c.emit("agent:exit", json!({ "taskId": tid, "code": serde_json::Value::Null }));
                            return;
                        }

                        // Compute next profile preview (without committing) for the event.
                        let next = crate::agent::profile_env::resolve_profile_env(&attempted_c);
                        let to_profile = next.as_ref().map(|r| r.profile_id.clone());

                        let _ = app_c.emit(
                            "agent:profile_switched",
                            json!({
                                "taskId": tid,
                                "fromProfileId": from_profile,
                                "toProfileId": to_profile,
                            }),
                        );

                        if to_profile.is_none() {
                            let _ = app_c.emit("agent:state", json!({ "taskId": tid, "state": "rate_limited" }));
                            let _ = app_c.emit("agent:exit", json!({ "taskId": tid, "code": serde_json::Value::Null }));
                            return;
                        }

                        // Respawn with recover=true and the updated exclude list.
                        let _ = do_spawn(
                            tid,
                            project_path_c,
                            spec_id_c,
                            true,
                            manager_c,
                            app_c,
                            attempted_c,
                        ).await;
                    }
                }
            });
        }

        {
            let mut map = agents_arc.lock().await;
            map.insert(
                task_id.clone(),
                RunningAgent {
                    task_id: task_id.clone(),
                    kill_tx,
                    started_at: SystemTime::now(),
                    current_profile_id: current_profile_id.clone(),
                    attempted_profile_ids: attempted,
                },
            );
        }

        let _ = app.emit(
            "agent:state",
            json!({ "taskId": task_id, "state": "running" }),
        );
        info!(
            "[agent] started task {} via {:?} (profile: {:?})",
            task_id, python, current_profile_id
        );

        IpcResult::ok(json!({ "started": true, "profileId": current_profile_id }))
    })
}

#[tauri::command]
pub async fn agent_start(
    app: AppHandle,
    manager: State<'_, SharedAgentManager>,
    task_id: String,
    project_path: String,
    spec_id: String,
) -> Result<IpcResult<serde_json::Value>, ()> {
    let m = manager.inner().clone();
    Ok(do_spawn(task_id, project_path, spec_id, false, m, app, Vec::new()).await)
}

#[tauri::command]
pub async fn agent_stop(
    manager: State<'_, SharedAgentManager>,
    task_id: String,
) -> Result<IpcResult<()>, ()> {
    let agents_arc = {
        let mgr = match manager.try_lock() {
            Ok(g) => g,
            Err(_) => {
                return Ok(IpcResult {
                    success: false,
                    data: None,
                    error: Some("internal_error".to_string()),
                })
            }
        };
        mgr.agents.clone()
    };

    let mut map = agents_arc.lock().await;
    if let Some(running) = map.remove(&task_id) {
        let _ = running.kill_tx.send(());
        info!("[agent] stop requested for task {}", task_id);
    }
    Ok(IpcResult::ok(()))
}

#[tauri::command]
pub async fn agent_recover(
    app: AppHandle,
    manager: State<'_, SharedAgentManager>,
    task_id: String,
    project_path: String,
    spec_id: String,
) -> Result<IpcResult<serde_json::Value>, ()> {
    let m = manager.inner().clone();
    Ok(do_spawn(task_id, project_path, spec_id, true, m, app, Vec::new()).await)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn agent_check_running(
    manager: State<'_, SharedAgentManager>,
    task_id: String,
) -> Result<IpcResult<bool>, ()> {
    let agents_arc = {
        let mgr = match manager.try_lock() {
            Ok(g) => g,
            Err(_) => return Ok(IpcResult::ok(false)),
        };
        mgr.agents.clone()
    };
    let map = agents_arc.lock().await;
    Ok(IpcResult::ok(map.contains_key(&task_id)))
}

#[cfg(test)]
mod env_injection_tests {
    //! Integration tests for `apply_profile_env`. Spawn a real subprocess that
    //! dumps its environment so we can verify cross-vendor leakage is fixed.
    //! These complement the pure unit tests in `agent::profile_env`.
    use super::apply_profile_env;
    use crate::agent::profile_env::{ProfileKind, ResolvedProfile};
    use std::process::Stdio;

    /// Runs the given closure against `python -c` and returns the env dump.
    /// Returns None if no python interpreter is available (CI sanity).
    async fn run_env_dump(configure: impl FnOnce(&mut tokio::process::Command)) -> Option<String> {
        let py = if std::process::Command::new("python3")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            "python3"
        } else if std::process::Command::new("python")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            "python"
        } else {
            return None;
        };

        let mut cmd = tokio::process::Command::new(py);
        cmd.arg("-c")
            .arg("import os,json; print(json.dumps(dict(os.environ)))")
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        configure(&mut cmd);
        let out = cmd.output().await.ok()?;
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8(out.stdout).ok()?)
    }

    fn parse_env(json_str: &str) -> serde_json::Map<String, serde_json::Value> {
        serde_json::from_str::<serde_json::Map<_, _>>(json_str).expect("env dump must be JSON")
    }

    #[tokio::test]
    async fn api_profile_strips_oauth_token_from_inherited_env() {
        // Simulate a parent process that already has an OAuth token in env.
        std::env::set_var("CLAUDE_CODE_OAUTH_TOKEN", "stale-oauth-from-shell");

        let rp = ResolvedProfile {
            profile_id: "api-1".into(),
            profile_kind: ProfileKind::Api,
            env: vec![
                (
                    "ANTHROPIC_BASE_URL".into(),
                    "https://api.example.com".into(),
                ),
                ("ANTHROPIC_AUTH_TOKEN".into(), "sk-test".into()),
            ],
        };

        let dump = match run_env_dump(|c| apply_profile_env(c, &rp)).await {
            Some(d) => d,
            None => {
                eprintln!("skipping: no python available");
                return;
            }
        };
        let env = parse_env(&dump);

        std::env::remove_var("CLAUDE_CODE_OAUTH_TOKEN");

        assert_eq!(
            env.get("ANTHROPIC_BASE_URL").and_then(|v| v.as_str()),
            Some("https://api.example.com"),
            "API base url must be injected"
        );
        assert_eq!(
            env.get("ANTHROPIC_AUTH_TOKEN").and_then(|v| v.as_str()),
            Some("sk-test"),
            "API auth token must be injected"
        );
        assert!(
            !env.contains_key("CLAUDE_CODE_OAUTH_TOKEN"),
            "OAuth token must be stripped when API profile is active (got: {:?})",
            env.get("CLAUDE_CODE_OAUTH_TOKEN")
        );
    }

    #[tokio::test]
    async fn oauth_profile_strips_anthropic_vars_from_inherited_env() {
        std::env::set_var("ANTHROPIC_BASE_URL", "https://stale.example.com");
        std::env::set_var("ANTHROPIC_AUTH_TOKEN", "stale-key");
        std::env::set_var("ANTHROPIC_MODEL", "stale-model");

        let rp = ResolvedProfile {
            profile_id: "oauth-1".into(),
            profile_kind: ProfileKind::OAuth,
            env: vec![("CLAUDE_CODE_OAUTH_TOKEN".into(), "fresh-oauth".into())],
        };

        let dump = match run_env_dump(|c| apply_profile_env(c, &rp)).await {
            Some(d) => d,
            None => {
                eprintln!("skipping: no python available");
                return;
            }
        };
        let env = parse_env(&dump);

        std::env::remove_var("ANTHROPIC_BASE_URL");
        std::env::remove_var("ANTHROPIC_AUTH_TOKEN");
        std::env::remove_var("ANTHROPIC_MODEL");

        assert_eq!(
            env.get("CLAUDE_CODE_OAUTH_TOKEN").and_then(|v| v.as_str()),
            Some("fresh-oauth"),
            "OAuth token must be injected"
        );
        for k in [
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_MODEL",
        ] {
            assert!(
                !env.contains_key(k),
                "{} must be stripped when OAuth profile is active (got: {:?})",
                k,
                env.get(k)
            );
        }
    }

    #[tokio::test]
    async fn codex_profile_strips_anthropic_vars() {
        std::env::set_var("ANTHROPIC_BASE_URL", "https://stale.example.com");
        std::env::set_var("CLAUDE_CODE_OAUTH_TOKEN", "stale-oauth");

        let rp = ResolvedProfile {
            profile_id: "cx-1".into(),
            profile_kind: ProfileKind::Codex,
            env: vec![
                ("OPENAI_API_KEY".into(), "sk-openai-test".into()),
                ("AUTO_CLAUDE_PROVIDER".into(), "codex".into()),
                ("AUTO_CLAUDE_CODEX_MODEL".into(), "gpt-5".into()),
            ],
        };

        let dump = match run_env_dump(|c| apply_profile_env(c, &rp)).await {
            Some(d) => d,
            None => {
                eprintln!("skipping: no python available");
                return;
            }
        };
        let env = parse_env(&dump);

        std::env::remove_var("ANTHROPIC_BASE_URL");
        std::env::remove_var("CLAUDE_CODE_OAUTH_TOKEN");

        assert_eq!(
            env.get("OPENAI_API_KEY").and_then(|v| v.as_str()),
            Some("sk-openai-test")
        );
        assert_eq!(
            env.get("AUTO_CLAUDE_PROVIDER").and_then(|v| v.as_str()),
            Some("codex")
        );
        assert_eq!(
            env.get("AUTO_CLAUDE_CODEX_MODEL").and_then(|v| v.as_str()),
            Some("gpt-5")
        );
        for k in [
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_MODEL",
            "CLAUDE_CODE_OAUTH_TOKEN",
        ] {
            assert!(
                !env.contains_key(k),
                "{} must be stripped under Codex profile",
                k
            );
        }
    }

    #[tokio::test]
    async fn api_profile_strips_codex_vars() {
        std::env::set_var("OPENAI_API_KEY", "stale-openai");
        std::env::set_var("AUTO_CLAUDE_PROVIDER", "codex");
        std::env::set_var("AUTO_CLAUDE_CODEX_MODEL", "stale-gpt");
        std::env::set_var("AUTO_CLAUDE_CODEX_BINARY", "/stale/codex");

        let rp = ResolvedProfile {
            profile_id: "api-3".into(),
            profile_kind: ProfileKind::Api,
            env: vec![
                (
                    "ANTHROPIC_BASE_URL".into(),
                    "https://api.example.com".into(),
                ),
                ("ANTHROPIC_AUTH_TOKEN".into(), "sk-test".into()),
            ],
        };

        let dump = match run_env_dump(|c| apply_profile_env(c, &rp)).await {
            Some(d) => d,
            None => {
                eprintln!("skipping: no python available");
                return;
            }
        };
        let env = parse_env(&dump);

        std::env::remove_var("OPENAI_API_KEY");
        std::env::remove_var("AUTO_CLAUDE_PROVIDER");
        std::env::remove_var("AUTO_CLAUDE_CODEX_MODEL");
        std::env::remove_var("AUTO_CLAUDE_CODEX_BINARY");

        for k in [
            "OPENAI_API_KEY",
            "AUTO_CLAUDE_PROVIDER",
            "AUTO_CLAUDE_CODEX_MODEL",
            "AUTO_CLAUDE_CODEX_BINARY",
        ] {
            assert!(
                !env.contains_key(k),
                "{} must be stripped under API (Anthropic) profile",
                k
            );
        }
    }

    #[tokio::test]
    async fn api_profile_with_model_injects_all_three_vars() {
        let rp = ResolvedProfile {
            profile_id: "api-2".into(),
            profile_kind: ProfileKind::Api,
            env: vec![
                (
                    "ANTHROPIC_BASE_URL".into(),
                    "https://api.example.com".into(),
                ),
                ("ANTHROPIC_AUTH_TOKEN".into(), "sk-test".into()),
                ("ANTHROPIC_MODEL".into(), "claude-sonnet-4-5".into()),
            ],
        };

        let dump = match run_env_dump(|c| apply_profile_env(c, &rp)).await {
            Some(d) => d,
            None => return,
        };
        let env = parse_env(&dump);

        assert_eq!(
            env.get("ANTHROPIC_MODEL").and_then(|v| v.as_str()),
            Some("claude-sonnet-4-5")
        );
    }
}
