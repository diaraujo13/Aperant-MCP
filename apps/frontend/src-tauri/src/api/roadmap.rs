//! Roadmap generation and management commands.
//!
//! Spawns apps/backend/runners/roadmap_runner.py as a child process.
//! Streaming progress events: `roadmap:progress`, `roadmap:complete`,
//! `roadmap:error`, `roadmap:stopped`.

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

pub struct RoadmapProcess(Arc<Mutex<Option<std::process::Child>>>);
impl std::ops::Deref for RoadmapProcess {
    type Target = Arc<Mutex<Option<std::process::Child>>>;
    fn deref(&self) -> &Self::Target { &self.0 }
}
impl Default for RoadmapProcess {
    fn default() -> Self { Self(Arc::new(Mutex::new(None))) }
}

pub struct RoadmapRunning(Arc<Mutex<bool>>);
impl std::ops::Deref for RoadmapRunning {
    type Target = Arc<Mutex<bool>>;
    fn deref(&self) -> &Self::Target { &self.0 }
}
impl Default for RoadmapRunning {
    fn default() -> Self { Self(Arc::new(Mutex::new(false))) }
}

const AUTO_CLAUDE_SUBDIR: &str = ".auto-claude";
const ROADMAP_FILENAME: &str = "roadmap.json";

fn roadmap_path(project_path: &str) -> PathBuf {
    PathBuf::from(project_path).join(AUTO_CLAUDE_SUBDIR).join(ROADMAP_FILENAME)
}

fn backend_dir() -> Option<PathBuf> {
    // In dev (tauri dev), CWD is apps/frontend — backend is ../backend
    if let Ok(cwd) = std::env::current_dir() {
        let candidate = cwd.join("..").join("backend");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    // Production bundle: alongside the executable
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.join("..").join("Resources").join("backend")))
        .filter(|p| p.exists())
}

/// Read roadmap.json — returns null data if file is missing.
#[tauri::command(rename_all = "camelCase")]
pub async fn roadmap_get(project_path: String) -> AppResult<IpcResult<Value>> {
    let res = tokio::task::spawn_blocking(move || -> IpcResult<Value> {
        let path = roadmap_path(&project_path);
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

/// Return `{ isRunning: bool }` from AppState.
#[tauri::command(rename_all = "camelCase")]
pub async fn roadmap_get_status(
    running: State<'_, RoadmapRunning>,
) -> AppResult<IpcResult<Value>> {
    let is_running = *running.lock().map_err(|_| AppError::new("lock_failed", "poison"))?;
    Ok(IpcResult::ok(json!({ "isRunning": is_running })))
}

/// Spawn roadmap_runner.py, stream progress events.
#[tauri::command(rename_all = "camelCase")]
pub async fn roadmap_generate(
    app: AppHandle,
    process: State<'_, RoadmapProcess>,
    running: State<'_, RoadmapRunning>,
    project_path: String,
    params: Option<Value>,
) -> AppResult<IpcResult<Value>> {
    // Guard: don't start if already running
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

    let runner = backend.join("runners").join("roadmap_runner.py");
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

    // Build args from params
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
    }

    let mut child = std::process::Command::new(&python)
        .args(&args)
        .current_dir(&backend)
        .env("PYTHONPATH", &backend)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| AppError::new("spawn_failed", e.to_string()))?;

    // Set running flag
    {
        let mut r = running.lock().map_err(|_| AppError::new("lock_failed", "poison"))?;
        *r = true;
    }

    // Take stdout/stderr for streaming
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // Store child in state
    {
        let mut p = process.lock().map_err(|_| AppError::new("lock_failed", "poison"))?;
        *p = Some(child);
    }

    let process_clone = Arc::clone(&process);
    let running_clone = Arc::clone(&running);
    let app_clone = app.clone();
    let project_path_clone = project_path.clone();

    // Spawn monitoring thread
    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader};

        if let Some(out) = stdout {
            let reader = BufReader::new(out);
            for line in reader.lines().flatten() {
                let _ = app_clone.emit("roadmap:progress", json!({ "message": line }));
            }
        }

        if let Some(err) = stderr {
            let reader = BufReader::new(err);
            for line in reader.lines().flatten() {
                let _ = app_clone.emit("roadmap:progress", json!({ "message": line, "stream": "stderr" }));
            }
        }

        // Wait for process
        let exit_status = {
            let mut p = process_clone.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(child) = p.as_mut() {
                child.wait().ok()
            } else {
                None
            }
        };

        // Clear process slot
        {
            let mut p = process_clone.lock().unwrap_or_else(|e| e.into_inner());
            *p = None;
        }
        // Clear running flag
        {
            let mut r = running_clone.lock().unwrap_or_else(|e| e.into_inner());
            *r = false;
        }

        match exit_status {
            Some(s) if s.success() => {
                // Try to load the roadmap.json and emit with it
                let roadmap_data: Value = fs::read_to_string(
                    PathBuf::from(&project_path_clone)
                        .join(AUTO_CLAUDE_SUBDIR)
                        .join(ROADMAP_FILENAME),
                )
                .ok()
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .unwrap_or(Value::Null);
                let _ = app_clone.emit("roadmap:complete", json!({ "roadmap": roadmap_data }));
            }
            Some(_) => {
                let _ = app_clone.emit("roadmap:error", json!({ "error": "runner_failed" }));
            }
            None => {
                // Process was killed
                let _ = app_clone.emit("roadmap:stopped", json!({}));
            }
        }
    });

    Ok(IpcResult::ok(json!({ "started": true })))
}

/// Kill the roadmap child process.
#[tauri::command(rename_all = "camelCase")]
pub async fn roadmap_stop(
    process: State<'_, RoadmapProcess>,
    running: State<'_, RoadmapRunning>,
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

/// Write roadmap.json.
#[tauri::command(rename_all = "camelCase")]
pub async fn roadmap_save(project_path: String, data: Value) -> AppResult<IpcResult<Value>> {
    let res = tokio::task::spawn_blocking(move || -> AppResult<IpcResult<Value>> {
        let dir = PathBuf::from(&project_path).join(AUTO_CLAUDE_SUBDIR);
        fs::create_dir_all(&dir).map_err(|e| AppError::new("mkdir_failed", e.to_string()))?;
        let path = dir.join(ROADMAP_FILENAME);
        let pretty = serde_json::to_string_pretty(&data)
            .map_err(|e| AppError::new("serialize_failed", e.to_string()))?;
        fs::write(&path, pretty).map_err(|e| AppError::new("write_failed", e.to_string()))?;
        Ok(IpcResult::ok(json!({ "saved": true })))
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))??;
    Ok(res)
}

/// Update a single feature's status inside roadmap.json.
#[tauri::command(rename_all = "camelCase")]
pub async fn roadmap_update_feature_status(
    project_path: String,
    feature_id: String,
    status: String,
) -> AppResult<IpcResult<Value>> {
    let res = tokio::task::spawn_blocking(move || -> AppResult<IpcResult<Value>> {
        let path = roadmap_path(&project_path);
        if !path.exists() {
            return Ok(IpcResult {
                success: false,
                data: None,
                error: Some("roadmap_not_found".to_string()),
            });
        }
        let raw = fs::read_to_string(&path)
            .map_err(|e| AppError::new("read_failed", e.to_string()))?;
        let mut roadmap: Value = serde_json::from_str(&raw)
            .map_err(|e| AppError::new("parse_failed", e.to_string()))?;

        // Search in the features array (or phases/features nested structures)
        let mut found = false;
        if let Some(features) = roadmap.get_mut("features").and_then(|v| v.as_array_mut()) {
            for feature in features.iter_mut() {
                if feature.get("id").and_then(|v| v.as_str()) == Some(&feature_id) {
                    if let Some(obj) = feature.as_object_mut() {
                        obj.insert("status".to_string(), json!(status));
                        obj.insert("updated_at".to_string(), json!(Utc::now().to_rfc3339()));
                    }
                    found = true;
                    break;
                }
            }
        }

        // Also check phases -> features nested structure
        if !found {
            if let Some(phases) = roadmap.get_mut("phases").and_then(|v| v.as_array_mut()) {
                'outer: for phase in phases.iter_mut() {
                    if let Some(feats) = phase.get_mut("features").and_then(|v| v.as_array_mut()) {
                        for feature in feats.iter_mut() {
                            if feature.get("id").and_then(|v| v.as_str()) == Some(&feature_id) {
                                if let Some(obj) = feature.as_object_mut() {
                                    obj.insert("status".to_string(), json!(status));
                                    obj.insert("updated_at".to_string(), json!(Utc::now().to_rfc3339()));
                                }
                                found = true;
                                break 'outer;
                            }
                        }
                    }
                }
            }
        }

        if !found {
            return Ok(IpcResult {
                success: false,
                data: None,
                error: Some("feature_not_found".to_string()),
            });
        }

        let pretty = serde_json::to_string_pretty(&roadmap)
            .map_err(|e| AppError::new("serialize_failed", e.to_string()))?;
        fs::write(&path, pretty).map_err(|e| AppError::new("write_failed", e.to_string()))?;
        Ok(IpcResult::ok(json!({ "updated": true })))
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))??;
    Ok(res)
}

/// Convert a roadmap feature to a task spec. Returns the new spec id.
#[tauri::command(rename_all = "camelCase")]
pub async fn roadmap_convert_feature(
    project_path: String,
    feature_id: String,
) -> AppResult<IpcResult<Value>> {
    let res = tokio::task::spawn_blocking(move || -> AppResult<IpcResult<Value>> {
        let roadmap_p = roadmap_path(&project_path);
        if !roadmap_p.exists() {
            return Ok(IpcResult {
                success: false,
                data: None,
                error: Some("roadmap_not_found".to_string()),
            });
        }
        let raw = fs::read_to_string(&roadmap_p)
            .map_err(|e| AppError::new("read_failed", e.to_string()))?;
        let roadmap: Value = serde_json::from_str(&raw)
            .map_err(|e| AppError::new("parse_failed", e.to_string()))?;

        // Find the feature
        let feature = find_feature_in_roadmap(&roadmap, &feature_id);
        let (title, description) = match feature {
            Some(f) => (
                f.get("title")
                    .or_else(|| f.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("roadmap-feature")
                    .to_string(),
                f.get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            ),
            None => ("roadmap-feature".to_string(), "".to_string()),
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
            "source": "roadmap",
            "roadmap_feature_id": feature_id,
        });
        let plan_path = spec_dir.join("implementation_plan.json");
        fs::write(&plan_path, serde_json::to_string_pretty(&plan).unwrap_or_default())
            .map_err(|e| AppError::new("write_failed", e.to_string()))?;

        let meta_path = spec_dir.join("task_metadata.json");
        let _ = fs::write(
            &meta_path,
            serde_json::to_string_pretty(&json!({ "created_at": now })).unwrap_or_default(),
        );

        Ok(IpcResult::ok(json!({ "specId": spec_id })))
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))??;
    Ok(res)
}

fn find_feature_in_roadmap<'a>(roadmap: &'a Value, feature_id: &str) -> Option<&'a Value> {
    if let Some(features) = roadmap.get("features").and_then(|v| v.as_array()) {
        for f in features {
            if f.get("id").and_then(|v| v.as_str()) == Some(feature_id) {
                return Some(f);
            }
        }
    }
    if let Some(phases) = roadmap.get("phases").and_then(|v| v.as_array()) {
        for phase in phases {
            if let Some(features) = phase.get("features").and_then(|v| v.as_array()) {
                for f in features {
                    if f.get("id").and_then(|v| v.as_str()) == Some(feature_id) {
                        return Some(f);
                    }
                }
            }
        }
    }
    None
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
    let slug = if slug.is_empty() { "feature".to_string() } else { slug };
    let slug: String = slug.chars().take(40).collect();
    format!("{:03}-{}", next, slug)
}
