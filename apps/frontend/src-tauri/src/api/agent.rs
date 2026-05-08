//! Agent execution subsystem (Phase 5).
//!
//! Spawns apps/backend/run.py as a child process managed by tokio's async
//! runtime. stdout/stderr stream to the renderer via Tauri events so the UI
//! thread is never blocked. agent_start returns immediately after spawning.
//!
//! Deferred (stubs remain elsewhere):
//!   - Worktree management
//!   - Agent queue ordering / priority
//!   - IDE integration
//!   - Claude profile switching during execution
//!   - Agent log persistence
//!   - Multi-account rate limit switching

use crate::agent::manager::{AgentManager, RunningAgent};
use crate::types::IpcResult;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::SystemTime;
use tauri::{AppHandle, Emitter, State};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Mutex;
use tracing::{info, warn};

pub type SharedAgentManager = Arc<Mutex<AgentManager>>;

/// Resolves the Python interpreter to use for the given project root.
///
/// Order:
///   1. <projectPath>/apps/backend/.venv/bin/python   (project-local venv)
///   2. <projectPath>/.venv/bin/python                (workspace-root venv)
///   3. python3 on PATH
///   4. python on PATH
fn resolve_python(project_path: &Path) -> Option<PathBuf> {
    let bin_dir = if cfg!(windows) { "Scripts" } else { "bin" };
    let py_name = if cfg!(windows) { "python.exe" } else { "python" };

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

    // System fallback: probe PATH without spawning extra processes by attempting
    // a version check. Uses blocking std::process because this is a one-time
    // startup probe and not in a hot path.
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

/// Shared spawn logic for agent_start / agent_recover.
async fn do_spawn(
    task_id: String,
    project_path: String,
    spec_id: String,
    recover: bool,
    manager: &SharedAgentManager,
    app: AppHandle,
) -> IpcResult<serde_json::Value> {
    let project_path = PathBuf::from(&project_path);

    // Obtain the inner agents map without holding the manager lock across the spawn.
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

    // Guard: reject if a process for this task is already running.
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

    let python = match resolve_python(&project_path) {
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

    let run_py = project_path.join("apps").join("backend").join("run.py");

    let mut cmd = tokio::process::Command::new(&python);
    cmd.arg(&run_py)
        .arg("--spec")
        .arg(&spec_id)
        .arg("--project")
        .arg(&project_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Let tokio own lifetime; we kill explicitly via the oneshot channel.
        .kill_on_drop(false);

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

    // Move stdout/stderr out of Child before handing Child to the monitor task.
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // Stream stdout lines as agent:output events.
    if let Some(stdout) = stdout {
        let app_c = app.clone();
        let tid = task_id.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let _ = app_c.emit(
                    "agent:output",
                    json!({ "taskId": tid, "stream": "stdout", "data": line }),
                );
            }
        });
    }

    // Stream stderr lines as agent:output events.
    if let Some(stderr) = stderr {
        let app_c = app.clone();
        let tid = task_id.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let _ = app_c.emit(
                    "agent:output",
                    json!({ "taskId": tid, "stream": "stderr", "data": line }),
                );
            }
        });
    }

    let (kill_tx, kill_rx) = tokio::sync::oneshot::channel::<()>();

    // Monitor task: waits for natural exit OR a kill signal, then emits
    // state/exit events and removes the task from the running map.
    {
        let agents_arc2 = agents_arc.clone();
        let tid = task_id.clone();
        let app_c = app.clone();
        tokio::spawn(async move {
            tokio::select! {
                result = child.wait() => {
                    let code = result.ok().and_then(|s| s.code());
                    let state = if code == Some(0) { "stopped" } else { "crashed" };
                    let _ = app_c.emit("agent:state", json!({ "taskId": tid, "state": state }));
                    let _ = app_c.emit("agent:exit", json!({ "taskId": tid, "code": code }));
                }
                _ = kill_rx => {
                    let _ = child.kill().await;
                    let _ = app_c.emit("agent:state", json!({ "taskId": tid, "state": "stopped" }));
                    let _ = app_c.emit("agent:exit", json!({ "taskId": tid, "code": serde_json::Value::Null }));
                }
            }
            agents_arc2.lock().await.remove(&tid);
        });
    }

    // Register the running agent.
    {
        let mut map = agents_arc.lock().await;
        map.insert(
            task_id.clone(),
            RunningAgent {
                task_id: task_id.clone(),
                kill_tx,
                started_at: SystemTime::now(),
            },
        );
    }

    let _ = app.emit("agent:state", json!({ "taskId": task_id, "state": "running" }));
    info!("[agent] started task {} via {:?}", task_id, python);

    IpcResult::ok(json!({ "started": true }))
}

#[tauri::command]
pub async fn agent_start(
    app: AppHandle,
    manager: State<'_, SharedAgentManager>,
    task_id: String,
    project_path: String,
    spec_id: String,
) -> Result<IpcResult<serde_json::Value>, ()> {
    Ok(do_spawn(task_id, project_path, spec_id, false, &manager, app).await)
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
        // Dropping kill_tx sends the signal to the monitor task.
        let _ = running.kill_tx.send(());
        info!("[agent] stop requested for task {}", task_id);
    }
    // Unknown task → silent no-op per spec.
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
    Ok(do_spawn(task_id, project_path, spec_id, true, &manager, app).await)
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
