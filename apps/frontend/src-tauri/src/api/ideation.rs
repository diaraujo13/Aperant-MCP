//! Ideation generation and management commands.
//!
//! Spawns apps/backend/runners/ideation_runner.py as a child process.
//! Events: `ideation:progress`, `ideation:log`, `ideation:complete`,
//! `ideation:error`, `ideation:stopped`, `ideation:type_complete`,
//! `ideation:type_failed`.

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

pub type IdeationProcess = Arc<Mutex<Option<std::process::Child>>>;
pub type IdeationRunning = Arc<Mutex<bool>>;

const AUTO_CLAUDE_SUBDIR: &str = ".auto-claude";
const IDEATION_FILENAME: &str = "ideation.json";

fn ideation_path(project_path: &str) -> PathBuf {
    PathBuf::from(project_path).join(AUTO_CLAUDE_SUBDIR).join(IDEATION_FILENAME)
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

/// Read ideation.json — returns null if missing.
#[tauri::command(rename_all = "camelCase")]
pub async fn ideation_get(project_path: String) -> AppResult<IpcResult<Value>> {
    let res = tokio::task::spawn_blocking(move || -> IpcResult<Value> {
        let path = ideation_path(&project_path);
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

/// Spawn ideation_runner.py, stream events.
#[tauri::command(rename_all = "camelCase")]
pub async fn ideation_generate(
    app: AppHandle,
    process: State<'_, IdeationProcess>,
    running: State<'_, IdeationRunning>,
    project_path: String,
    params: Option<Value>,
) -> AppResult<IpcResult<Value>> {
    {
        let r = running.lock().map_err(|_| AppError::new("lock_failed", "poison"))?;
        if *r {
            return Ok(IpcResult {
                success: false,
                data: None,
                error: Some("already_running".to_string()),
            });
        }
    }

    let Some(backend) = backend_dir() else {
        return Ok(IpcResult {
            success: false,
            data: None,
            error: Some("backend_dir_not_found".to_string()),
        });
    };

    let runner = backend.join("runners").join("ideation_runner.py");
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

    let mut args: Vec<String> = vec![
        runner.to_string_lossy().to_string(),
        "--project".to_string(),
        project_path.clone(),
    ];
    if let Some(p) = &params {
        if let Some(model) = p.get("model").and_then(|v| v.as_str()) {
            args.push("--model".to_string());
            args.push(model.to_string());
        }
        if let Some(refresh) = p.get("refresh").and_then(|v| v.as_bool()) {
            if refresh {
                args.push("--refresh".to_string());
            }
        }
        if let Some(tl) = p.get("thinkingLevel").and_then(|v| v.as_str()) {
            args.push("--thinking-level".to_string());
            args.push(tl.to_string());
        }
        if let Some(types) = p.get("types").and_then(|v| v.as_str()) {
            args.push("--types".to_string());
            args.push(types.to_string());
        }
        if let Some(max) = p.get("maxIdeas").and_then(|v| v.as_u64()) {
            args.push("--max-ideas".to_string());
            args.push(max.to_string());
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

    {
        let mut r = running.lock().map_err(|_| AppError::new("lock_failed", "poison"))?;
        *r = true;
    }

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    {
        let mut p = process.lock().map_err(|_| AppError::new("lock_failed", "poison"))?;
        *p = Some(child);
    }

    let process_clone = Arc::clone(&process);
    let running_clone = Arc::clone(&running);
    let app_clone = app.clone();
    let project_path_clone = project_path.clone();

    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader};

        if let Some(out) = stdout {
            let reader = BufReader::new(out);
            for line in reader.lines().flatten() {
                // Detect type completion markers in output
                if line.contains("type_complete:") || line.contains("[IDEATION] Type complete") {
                    let _ = app_clone.emit("ideation:type_complete", json!({ "message": line }));
                } else if line.contains("type_failed:") || line.contains("[IDEATION] Type failed") {
                    let _ = app_clone.emit("ideation:type_failed", json!({ "message": line }));
                } else if line.starts_with('[') || line.contains("INFO") || line.contains("DEBUG") {
                    let _ = app_clone.emit("ideation:log", json!({ "message": line }));
                } else {
                    let _ = app_clone.emit("ideation:progress", json!({ "message": line }));
                }
            }
        }

        if let Some(err) = stderr {
            let reader = BufReader::new(err);
            for line in reader.lines().flatten() {
                let _ = app_clone.emit("ideation:log", json!({ "message": line, "stream": "stderr" }));
            }
        }

        let exit_status = {
            let mut p = process_clone.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(child) = p.as_mut() {
                child.wait().ok()
            } else {
                None
            }
        };

        {
            let mut p = process_clone.lock().unwrap_or_else(|e| e.into_inner());
            *p = None;
        }
        {
            let mut r = running_clone.lock().unwrap_or_else(|e| e.into_inner());
            *r = false;
        }

        match exit_status {
            Some(s) if s.success() => {
                let ideation_data: Value = fs::read_to_string(ideation_path(&project_path_clone))
                    .ok()
                    .and_then(|raw| serde_json::from_str(&raw).ok())
                    .unwrap_or(Value::Null);
                let _ = app_clone.emit("ideation:complete", json!({ "ideation": ideation_data }));
            }
            Some(_) => {
                let _ = app_clone.emit("ideation:error", json!({ "error": "runner_failed" }));
            }
            None => {
                let _ = app_clone.emit("ideation:stopped", json!({}));
            }
        }
    });

    Ok(IpcResult::ok(json!({ "started": true })))
}

/// Kill the ideation child process.
#[tauri::command(rename_all = "camelCase")]
pub async fn ideation_stop(
    process: State<'_, IdeationProcess>,
    running: State<'_, IdeationRunning>,
) -> AppResult<IpcResult<Value>> {
    let mut p = process.lock().map_err(|_| AppError::new("lock_failed", "poison"))?;
    if let Some(child) = p.as_mut() {
        let _ = child.kill();
        let _ = child.wait();
    }
    *p = None;
    {
        let mut r = running.lock().map_err(|_| AppError::new("lock_failed", "poison"))?;
        *r = false;
    }
    Ok(IpcResult::ok(json!({ "stopped": true })))
}

/// Mutate a single idea's status in ideation.json.
#[tauri::command(rename_all = "camelCase")]
pub async fn ideation_update_status(
    project_path: String,
    idea_id: String,
    status: String,
) -> AppResult<IpcResult<Value>> {
    mutate_ideation(project_path, move |ideas| {
        for idea in ideas.iter_mut() {
            if idea.get("id").and_then(|v| v.as_str()) == Some(&idea_id) {
                if let Some(obj) = idea.as_object_mut() {
                    obj.insert("status".to_string(), json!(status));
                    obj.insert("updated_at".to_string(), json!(Utc::now().to_rfc3339()));
                }
                return Ok(json!({ "updated": true }));
            }
        }
        Ok(json!({ "updated": false, "error": "idea_not_found" }))
    })
    .await
}

/// Convert an idea to a task spec, returning the new spec id.
#[tauri::command(rename_all = "camelCase")]
pub async fn ideation_convert_to_task(
    project_path: String,
    idea_id: String,
) -> AppResult<IpcResult<Value>> {
    let res = tokio::task::spawn_blocking(move || -> AppResult<IpcResult<Value>> {
        let path = ideation_path(&project_path);
        let (title, description) = if path.exists() {
            let raw = fs::read_to_string(&path)
                .map_err(|e| AppError::new("read_failed", e.to_string()))?;
            let data: Value = serde_json::from_str(&raw)
                .map_err(|e| AppError::new("parse_failed", e.to_string()))?;
            let ideas = data.get("ideas").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let found = ideas.iter().find(|i| i.get("id").and_then(|v| v.as_str()) == Some(&idea_id));
            match found {
                Some(idea) => (
                    idea.get("title").and_then(|v| v.as_str()).unwrap_or("idea").to_string(),
                    idea.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                ),
                None => ("idea".to_string(), "".to_string()),
            }
        } else {
            ("idea".to_string(), "".to_string())
        };

        let specs_root = PathBuf::from(&project_path).join(".auto-claude").join("specs");
        fs::create_dir_all(&specs_root)
            .map_err(|e| AppError::new("mkdir_failed", e.to_string()))?;

        let spec_id = next_spec_id(&specs_root, &title);
        let spec_dir = specs_root.join(&spec_id);
        fs::create_dir_all(&spec_dir)
            .map_err(|e| AppError::new("mkdir_failed", e.to_string()))?;

        let now = Utc::now().to_rfc3339();
        let plan = json!({
            "feature": title,
            "description": description,
            "created_at": now,
            "updated_at": now,
            "status": "pending",
            "phases": [],
            "source": "ideation",
            "ideation_idea_id": idea_id,
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

/// Set dismissed=true on an idea.
#[tauri::command(rename_all = "camelCase")]
pub async fn ideation_dismiss(
    project_path: String,
    idea_id: String,
) -> AppResult<IpcResult<Value>> {
    mutate_ideation(project_path, move |ideas| {
        for idea in ideas.iter_mut() {
            if idea.get("id").and_then(|v| v.as_str()) == Some(&idea_id) {
                if let Some(obj) = idea.as_object_mut() {
                    obj.insert("dismissed".to_string(), json!(true));
                }
                return Ok(json!({ "dismissed": true }));
            }
        }
        Ok(json!({ "dismissed": false }))
    })
    .await
}

/// Set dismissed=true on all ideas.
#[tauri::command(rename_all = "camelCase")]
pub async fn ideation_dismiss_all(project_path: String) -> AppResult<IpcResult<Value>> {
    mutate_ideation(project_path, |ideas| {
        for idea in ideas.iter_mut() {
            if let Some(obj) = idea.as_object_mut() {
                obj.insert("dismissed".to_string(), json!(true));
            }
        }
        Ok(json!({ "dismissed": ideas.len() }))
    })
    .await
}

/// Set archived=true on an idea.
#[tauri::command(rename_all = "camelCase")]
pub async fn ideation_archive(
    project_path: String,
    idea_id: String,
) -> AppResult<IpcResult<Value>> {
    mutate_ideation(project_path, move |ideas| {
        for idea in ideas.iter_mut() {
            if idea.get("id").and_then(|v| v.as_str()) == Some(&idea_id) {
                if let Some(obj) = idea.as_object_mut() {
                    obj.insert("archived".to_string(), json!(true));
                }
                return Ok(json!({ "archived": true }));
            }
        }
        Ok(json!({ "archived": false }))
    })
    .await
}

/// Remove an idea from the array.
#[tauri::command(rename_all = "camelCase")]
pub async fn ideation_delete(
    project_path: String,
    idea_id: String,
) -> AppResult<IpcResult<Value>> {
    mutate_ideation(project_path, move |ideas| {
        let before = ideas.len();
        ideas.retain(|i| i.get("id").and_then(|v| v.as_str()) != Some(&idea_id));
        Ok(json!({ "deleted": ideas.len() < before }))
    })
    .await
}

/// Remove multiple ideas by id.
#[tauri::command(rename_all = "camelCase")]
pub async fn ideation_delete_multiple(
    project_path: String,
    idea_ids: Vec<String>,
) -> AppResult<IpcResult<Value>> {
    let ids_set: std::collections::HashSet<String> = idea_ids.into_iter().collect();
    mutate_ideation(project_path, move |ideas| {
        let before = ideas.len();
        ideas.retain(|i| {
            let id = i.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            !ids_set.contains(&id)
        });
        Ok(json!({ "deleted": before - ideas.len() }))
    })
    .await
}

/// Helper: read ideation.json, run mutator on the ideas array, write back.
async fn mutate_ideation<F>(project_path: String, mutator: F) -> AppResult<IpcResult<Value>>
where
    F: FnOnce(&mut Vec<Value>) -> AppResult<Value> + Send + 'static,
{
    let res = tokio::task::spawn_blocking(move || -> AppResult<IpcResult<Value>> {
        let path = ideation_path(&project_path);
        let mut data: Value = if path.exists() {
            let raw = fs::read_to_string(&path)
                .map_err(|e| AppError::new("read_failed", e.to_string()))?;
            serde_json::from_str(&raw).map_err(|e| AppError::new("parse_failed", e.to_string()))?
        } else {
            json!({ "ideas": [] })
        };

        let mut ideas: Vec<Value> = data
            .get("ideas")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let result = mutator(&mut ideas)?;

        if let Some(obj) = data.as_object_mut() {
            obj.insert("ideas".to_string(), json!(ideas));
        }

        let dir = PathBuf::from(&project_path).join(AUTO_CLAUDE_SUBDIR);
        fs::create_dir_all(&dir).map_err(|e| AppError::new("mkdir_failed", e.to_string()))?;
        fs::write(
            &path,
            serde_json::to_string_pretty(&data)
                .map_err(|e| AppError::new("serialize_failed", e.to_string()))?,
        )
        .map_err(|e| AppError::new("write_failed", e.to_string()))?;

        Ok(IpcResult::ok(result))
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
    let slug = if slug.is_empty() { "idea".to_string() } else { slug };
    let slug: String = slug.chars().take(40).collect();
    format!("{:03}-{}", next, slug)
}
