//! Insights AI chat commands.
//!
//! Spawns apps/backend/runners/insights_runner.py (streaming line-by-line).
//! Session storage at `<project_path>/.auto-claude/insights/<session_id>/`.
//! Events: `insights:stream:chunk`, `insights:status`, `insights:error`,
//! `insights:session:updated`.

use crate::api::review::which_python as find_python;
use crate::error::{AppError, AppResult};
use crate::types::IpcResult;
use chrono::Utc;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, State};
use uuid::Uuid;

pub type InsightsProcess = Arc<Mutex<Option<std::process::Child>>>;

const INSIGHTS_SUBDIR: &str = ".auto-claude/insights";

fn insights_root(project_path: &str) -> PathBuf {
    PathBuf::from(project_path).join(INSIGHTS_SUBDIR)
}

fn session_dir(project_path: &str, session_id: &str) -> PathBuf {
    insights_root(project_path).join(session_id)
}

fn session_json_path(project_path: &str, session_id: &str) -> PathBuf {
    session_dir(project_path, session_id).join("session.json")
}

fn backend_dir() -> Option<PathBuf> {
    if let Ok(cwd) = std::env::current_dir() {
        let candidate = cwd.join("..").join("backend");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.join("..").join("Resources").join("backend")))
        .filter(|p| p.exists())
}

/// List all sessions for a project (sorted newest first).
#[tauri::command(rename_all = "camelCase")]
pub async fn insights_list_sessions(project_path: String) -> AppResult<IpcResult<Value>> {
    let res = tokio::task::spawn_blocking(move || -> IpcResult<Value> {
        let root = insights_root(&project_path);
        if !root.exists() {
            return IpcResult::ok(json!([]));
        }
        let mut sessions: Vec<Value> = Vec::new();
        if let Ok(entries) = fs::read_dir(&root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let session_json = path.join("session.json");
                    if session_json.exists() {
                        if let Ok(raw) = fs::read_to_string(&session_json) {
                            if let Ok(session) = serde_json::from_str::<Value>(&raw) {
                                sessions.push(session);
                            }
                        }
                    }
                }
            }
        }
        // Sort by updated_at descending
        sessions.sort_by(|a, b| {
            let ta = a.get("updated_at").and_then(|v| v.as_str()).unwrap_or("");
            let tb = b.get("updated_at").and_then(|v| v.as_str()).unwrap_or("");
            tb.cmp(ta)
        });
        IpcResult::ok(json!(sessions))
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))?;
    Ok(res)
}

/// Get a session by id (or the latest if None).
#[tauri::command(rename_all = "camelCase")]
pub async fn insights_get_session(
    project_path: String,
    session_id: Option<String>,
) -> AppResult<IpcResult<Value>> {
    let res = tokio::task::spawn_blocking(move || -> IpcResult<Value> {
        let sid = match &session_id {
            Some(id) => id.clone(),
            None => {
                // Find the most recently updated session
                let root = insights_root(&project_path);
                if !root.exists() {
                    return IpcResult { success: true, data: Some(Value::Null), error: None };
                }
                let mut latest: Option<(String, String)> = None; // (updated_at, id)
                if let Ok(entries) = fs::read_dir(&root) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.is_dir() {
                            let sj = path.join("session.json");
                            if sj.exists() {
                                if let Ok(raw) = fs::read_to_string(&sj) {
                                    if let Ok(s) = serde_json::from_str::<Value>(&raw) {
                                        let upd = s
                                            .get("updated_at")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .to_string();
                                        let id = s
                                            .get("id")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .to_string();
                                        if latest.as_ref().map(|(t, _)| upd > *t).unwrap_or(true) {
                                            latest = Some((upd, id));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                match latest {
                    Some((_, id)) => id,
                    None => return IpcResult { success: true, data: Some(Value::Null), error: None },
                }
            }
        };

        let path = session_json_path(&project_path, &sid);
        if !path.exists() {
            return IpcResult { success: true, data: Some(Value::Null), error: None };
        }
        match fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str::<Value>(&raw) {
                Ok(v) => IpcResult::ok(v),
                Err(e) => IpcResult {
                    success: false,
                    data: None,
                    error: Some(format!("parse_error: {e}")),
                },
            },
            Err(e) => IpcResult {
                success: false,
                data: None,
                error: Some(format!("read_error: {e}")),
            },
        }
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))?;
    Ok(res)
}

/// Create a new insights session directory + session.json.
#[tauri::command(rename_all = "camelCase")]
pub async fn insights_new_session(project_path: String) -> AppResult<IpcResult<Value>> {
    let res = tokio::task::spawn_blocking(move || -> AppResult<IpcResult<Value>> {
        let session_id = Uuid::new_v4().to_string();
        let dir = session_dir(&project_path, &session_id);
        fs::create_dir_all(&dir).map_err(|e| AppError::new("mkdir_failed", e.to_string()))?;
        let now = Utc::now().to_rfc3339();
        let session = json!({
            "id": session_id,
            "name": "New Chat",
            "created_at": now,
            "updated_at": now,
            "messages": [],
            "model_config": null,
        });
        fs::write(
            session_json_path(&project_path, &session_id),
            serde_json::to_string_pretty(&session)
                .map_err(|e| AppError::new("serialize_failed", e.to_string()))?,
        )
        .map_err(|e| AppError::new("write_failed", e.to_string()))?;
        Ok(IpcResult::ok(session))
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))??;
    Ok(res)
}

/// Send a message — spawns insights_runner.py, streams chunks via events.
#[tauri::command(rename_all = "camelCase")]
pub async fn insights_send_message(
    app: AppHandle,
    process: State<'_, InsightsProcess>,
    project_path: String,
    session_id: String,
    message: String,
    model_config: Option<Value>,
) -> AppResult<IpcResult<Value>> {
    // Kill any in-flight process first (single-session streaming)
    {
        let mut p = process.lock().map_err(|_| AppError::new("lock_failed", "poison"))?;
        if let Some(child) = p.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        *p = None;
    }

    let Some(backend) = backend_dir() else {
        return Ok(IpcResult {
            success: false,
            data: None,
            error: Some("backend_dir_not_found".to_string()),
        });
    };

    let runner = backend.join("runners").join("insights_runner.py");
    if !runner.exists() {
        return Ok(IpcResult {
            success: false,
            data: None,
            error: Some(format!("runner_missing: {}", runner.display())),
        });
    }

    let Some(python) = find_python() else {
        return Ok(IpcResult {
            success: false,
            data: None,
            error: Some("python_not_found".to_string()),
        });
    };

    // Append user message to session.json before spawning
    let session_path = session_json_path(&project_path, &session_id);
    {
        let mut session: Value = if session_path.exists() {
            serde_json::from_str(
                &fs::read_to_string(&session_path)
                    .map_err(|e| AppError::new("read_session", e.to_string()))?,
            )
            .map_err(|e| AppError::new("parse_session", e.to_string()))?
        } else {
            json!({ "id": session_id, "messages": [] })
        };

        let msgs = session
            .get_mut("messages")
            .and_then(|v| v.as_array_mut())
            .ok_or_else(|| AppError::new("no_messages_array", "invalid session structure"))?;
        msgs.push(json!({
            "role": "user",
            "content": message,
            "timestamp": Utc::now().to_rfc3339(),
        }));
        if let Some(obj) = session.as_object_mut() {
            obj.insert("updated_at".to_string(), json!(Utc::now().to_rfc3339()));
        }
        fs::write(
            &session_path,
            serde_json::to_string_pretty(&session)
                .map_err(|e| AppError::new("serialize_session", e.to_string()))?,
        )
        .map_err(|e| AppError::new("write_session", e.to_string()))?;
    }

    let mut args = vec![
        runner.to_string_lossy().to_string(),
        "--project".to_string(),
        project_path.clone(),
        "--session".to_string(),
        session_id.clone(),
        "--message".to_string(),
        message.clone(),
    ];
    if let Some(mc) = &model_config {
        if let Some(m) = mc.get("model").and_then(|v| v.as_str()) {
            args.push("--model".to_string());
            args.push(m.to_string());
        }
    }

    let mut child = std::process::Command::new(&python)
        .args(&args)
        .current_dir(&backend)
        .env("PYTHONPATH", &backend)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| AppError::new("spawn_failed", e.to_string()))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    {
        let mut p = process.lock().map_err(|_| AppError::new("lock_failed", "poison"))?;
        *p = Some(child);
    }

    let process_clone = Arc::clone(&process);
    let app_clone = app.clone();
    let session_id_clone = session_id.clone();
    let project_path_clone = project_path.clone();

    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader};

        let mut full_response = String::new();

        if let Some(out) = stdout {
            let reader = BufReader::new(out);
            for line in reader.lines().flatten() {
                // Try to parse as JSON event from runner
                if let Ok(evt) = serde_json::from_str::<Value>(&line) {
                    let evt_type = evt.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    match evt_type {
                        "chunk" | "text" => {
                            let chunk = evt
                                .get("content")
                                .or_else(|| evt.get("text"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            full_response.push_str(&chunk);
                            let _ = app_clone.emit(
                                "insights:stream:chunk",
                                json!({ "sessionId": session_id_clone, "chunk": chunk }),
                            );
                        }
                        "status" => {
                            let _ = app_clone.emit("insights:status", evt.clone());
                        }
                        "error" => {
                            let _ = app_clone.emit("insights:error", evt.clone());
                        }
                        _ => {
                            let _ = app_clone.emit(
                                "insights:stream:chunk",
                                json!({ "sessionId": session_id_clone, "chunk": line }),
                            );
                        }
                    }
                } else {
                    // Plain text chunk
                    full_response.push_str(&line);
                    full_response.push('\n');
                    let _ = app_clone.emit(
                        "insights:stream:chunk",
                        json!({ "sessionId": session_id_clone, "chunk": line }),
                    );
                }
            }
        }

        if let Some(err) = stderr {
            let reader = BufReader::new(err);
            for line in reader.lines().flatten() {
                let _ = app_clone.emit(
                    "insights:error",
                    json!({ "sessionId": session_id_clone, "error": line }),
                );
            }
        }

        // Wait for process
        {
            let mut p = process_clone.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(child) = p.as_mut() {
                let _ = child.wait();
            }
            *p = None;
        }

        // Append assistant response to session.json
        let sess_path = session_json_path(&project_path_clone, &session_id_clone);
        if !full_response.is_empty() {
            if let Ok(raw) = fs::read_to_string(&sess_path) {
                if let Ok(mut session) = serde_json::from_str::<Value>(&raw) {
                    if let Some(msgs) = session.get_mut("messages").and_then(|v| v.as_array_mut()) {
                        msgs.push(json!({
                            "role": "assistant",
                            "content": full_response.trim(),
                            "timestamp": Utc::now().to_rfc3339(),
                        }));
                    }
                    if let Some(obj) = session.as_object_mut() {
                        obj.insert("updated_at".to_string(), json!(Utc::now().to_rfc3339()));
                    }
                    let _ = fs::write(
                        &sess_path,
                        serde_json::to_string_pretty(&session).unwrap_or_default(),
                    );
                    let _ = app_clone.emit("insights:session:updated", session);
                }
            }
        }
    });

    Ok(IpcResult::ok(json!({ "started": true })))
}

/// Clear all messages from a session.
#[tauri::command(rename_all = "camelCase")]
pub async fn insights_clear_session(
    project_path: String,
    session_id: String,
) -> AppResult<IpcResult<Value>> {
    let res = tokio::task::spawn_blocking(move || -> AppResult<IpcResult<Value>> {
        let path = session_json_path(&project_path, &session_id);
        if !path.exists() {
            return Ok(IpcResult {
                success: false,
                data: None,
                error: Some("session_not_found".to_string()),
            });
        }
        let raw = fs::read_to_string(&path)
            .map_err(|e| AppError::new("read_failed", e.to_string()))?;
        let mut session: Value = serde_json::from_str(&raw)
            .map_err(|e| AppError::new("parse_failed", e.to_string()))?;
        if let Some(obj) = session.as_object_mut() {
            obj.insert("messages".to_string(), json!([]));
            obj.insert("updated_at".to_string(), json!(Utc::now().to_rfc3339()));
        }
        fs::write(
            &path,
            serde_json::to_string_pretty(&session)
                .map_err(|e| AppError::new("serialize_failed", e.to_string()))?,
        )
        .map_err(|e| AppError::new("write_failed", e.to_string()))?;
        Ok(IpcResult::ok(json!({ "cleared": true })))
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))??;
    Ok(res)
}

/// Create a task spec from the current chat session content.
#[tauri::command(rename_all = "camelCase")]
pub async fn insights_create_task(
    project_path: String,
    session_id: String,
    description: String,
) -> AppResult<IpcResult<Value>> {
    let res = tokio::task::spawn_blocking(move || -> AppResult<IpcResult<Value>> {
        let specs_root = PathBuf::from(&project_path).join(".auto-claude").join("specs");
        fs::create_dir_all(&specs_root)
            .map_err(|e| AppError::new("mkdir_failed", e.to_string()))?;

        let spec_id = next_spec_id(&specs_root, &description);
        let spec_dir = specs_root.join(&spec_id);
        fs::create_dir_all(&spec_dir)
            .map_err(|e| AppError::new("mkdir_failed", e.to_string()))?;

        let now = Utc::now().to_rfc3339();
        let plan = json!({
            "feature": description,
            "description": description,
            "created_at": now,
            "updated_at": now,
            "status": "pending",
            "phases": [],
            "source": "insights",
            "insights_session_id": session_id,
        });
        fs::write(
            spec_dir.join("implementation_plan.json"),
            serde_json::to_string_pretty(&plan).unwrap_or_default(),
        )
        .map_err(|e| AppError::new("write_failed", e.to_string()))?;
        let _ = fs::write(
            spec_dir.join("task_metadata.json"),
            serde_json::to_string_pretty(&json!({ "created_at": now })).unwrap_or_default(),
        );
        Ok(IpcResult::ok(json!({ "specId": spec_id })))
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))??;
    Ok(res)
}

/// Switch to another session — just returns its data (no state).
#[tauri::command(rename_all = "camelCase")]
pub async fn insights_switch_session(
    project_path: String,
    session_id: String,
) -> AppResult<IpcResult<Value>> {
    // Delegate to get_session with explicit id
    insights_get_session(project_path, Some(session_id)).await
}

/// Delete a session directory.
#[tauri::command(rename_all = "camelCase")]
pub async fn insights_delete_session(
    project_path: String,
    session_id: String,
) -> AppResult<IpcResult<Value>> {
    let res = tokio::task::spawn_blocking(move || -> AppResult<IpcResult<Value>> {
        let dir = session_dir(&project_path, &session_id);
        if !dir.exists() {
            return Ok(IpcResult {
                success: false,
                data: None,
                error: Some("session_not_found".to_string()),
            });
        }
        fs::remove_dir_all(&dir).map_err(|e| AppError::new("remove_failed", e.to_string()))?;
        Ok(IpcResult::ok(json!({ "deleted": true })))
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))??;
    Ok(res)
}

/// Rename a session.
#[tauri::command(rename_all = "camelCase")]
pub async fn insights_rename_session(
    project_path: String,
    session_id: String,
    name: String,
) -> AppResult<IpcResult<Value>> {
    let res = tokio::task::spawn_blocking(move || -> AppResult<IpcResult<Value>> {
        let path = session_json_path(&project_path, &session_id);
        if !path.exists() {
            return Ok(IpcResult {
                success: false,
                data: None,
                error: Some("session_not_found".to_string()),
            });
        }
        let raw = fs::read_to_string(&path)
            .map_err(|e| AppError::new("read_failed", e.to_string()))?;
        let mut session: Value = serde_json::from_str(&raw)
            .map_err(|e| AppError::new("parse_failed", e.to_string()))?;
        if let Some(obj) = session.as_object_mut() {
            obj.insert("name".to_string(), json!(name));
            obj.insert("updated_at".to_string(), json!(Utc::now().to_rfc3339()));
        }
        fs::write(
            &path,
            serde_json::to_string_pretty(&session)
                .map_err(|e| AppError::new("serialize_failed", e.to_string()))?,
        )
        .map_err(|e| AppError::new("write_failed", e.to_string()))?;
        Ok(IpcResult::ok(session))
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))??;
    Ok(res)
}

/// Update model config in session.json.
#[tauri::command(rename_all = "camelCase")]
pub async fn insights_update_model_config(
    project_path: String,
    session_id: String,
    config: Value,
) -> AppResult<IpcResult<Value>> {
    let res = tokio::task::spawn_blocking(move || -> AppResult<IpcResult<Value>> {
        let path = session_json_path(&project_path, &session_id);
        if !path.exists() {
            return Ok(IpcResult {
                success: false,
                data: None,
                error: Some("session_not_found".to_string()),
            });
        }
        let raw = fs::read_to_string(&path)
            .map_err(|e| AppError::new("read_failed", e.to_string()))?;
        let mut session: Value = serde_json::from_str(&raw)
            .map_err(|e| AppError::new("parse_failed", e.to_string()))?;
        if let Some(obj) = session.as_object_mut() {
            obj.insert("model_config".to_string(), config);
            obj.insert("updated_at".to_string(), json!(Utc::now().to_rfc3339()));
        }
        fs::write(
            &path,
            serde_json::to_string_pretty(&session)
                .map_err(|e| AppError::new("serialize_failed", e.to_string()))?,
        )
        .map_err(|e| AppError::new("write_failed", e.to_string()))?;
        Ok(IpcResult::ok(json!({ "updated": true })))
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))??;
    Ok(res)
}

fn next_spec_id(specs_root: &std::path::Path, slug_source: &str) -> String {
    let mut max_num = 0u32;
    if let Ok(entries) = fs::read_dir(specs_root) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if let Some(prefix) = name.split('-').next() {
                    if let Ok(n) = prefix.parse::<u32>() {
                        if n > max_num {
                            max_num = n;
                        }
                    }
                }
            }
        }
    }
    let next = max_num + 1;
    let slug: String = slug_source
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let slug = if slug.is_empty() { "task".to_string() } else { slug };
    let slug: String = slug.chars().take(40).collect();
    format!("{:03}-{}", next, slug)
}
