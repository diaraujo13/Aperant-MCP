//! Changelog generation and management commands.
//!
//! No dedicated Python changelog runner exists — uses git commands for
//! structural ops (tags, commits, branches) and ai_analyzer_runner.py
//! for AI-generated content. Events: `changelog:progress`, `changelog:complete`,
//! `changelog:error`.

use crate::api::review::which_python as find_python;
use crate::error::{AppError, AppResult};
use crate::types::IpcResult;
use base64::Engine;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, State};

pub struct ChangelogProcess(Arc<Mutex<Option<std::process::Child>>>);
impl std::ops::Deref for ChangelogProcess {
    type Target = Arc<Mutex<Option<std::process::Child>>>;
    fn deref(&self) -> &Self::Target { &self.0 }
}
impl Default for ChangelogProcess {
    fn default() -> Self { Self(Arc::new(Mutex::new(None))) }
}

const SPECS_SUBDIR: &str = ".auto-claude/specs";
const PLAN_FILENAME: &str = "implementation_plan.json";

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

/// List spec dirs where implementation_plan.json has status "done".
#[tauri::command(rename_all = "camelCase")]
pub async fn changelog_get_done_tasks(project_path: String) -> AppResult<IpcResult<Value>> {
    let res = tokio::task::spawn_blocking(move || -> IpcResult<Value> {
        let specs_root = PathBuf::from(&project_path).join(SPECS_SUBDIR);
        if !specs_root.exists() {
            return IpcResult::ok(json!([]));
        }
        let mut done_tasks: Vec<Value> = Vec::new();
        if let Ok(entries) = fs::read_dir(&specs_root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let plan_path = path.join(PLAN_FILENAME);
                if !plan_path.exists() {
                    continue;
                }
                if let Ok(raw) = fs::read_to_string(&plan_path) {
                    if let Ok(plan) = serde_json::from_str::<Value>(&raw) {
                        if plan.get("status").and_then(|v| v.as_str()) == Some("done") {
                            let spec_id = path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("")
                                .to_string();
                            done_tasks.push(json!({
                                "specId": spec_id,
                                "feature": plan.get("feature").and_then(|v| v.as_str()).unwrap_or(""),
                                "description": plan.get("description").and_then(|v| v.as_str()).unwrap_or(""),
                                "updated_at": plan.get("updated_at").and_then(|v| v.as_str()).unwrap_or(""),
                            }));
                        }
                    }
                }
            }
        }
        // Sort by updated_at descending
        done_tasks.sort_by(|a, b| {
            let ta = a.get("updated_at").and_then(|v| v.as_str()).unwrap_or("");
            let tb = b.get("updated_at").and_then(|v| v.as_str()).unwrap_or("");
            tb.cmp(ta)
        });
        IpcResult::ok(json!(done_tasks))
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))?;
    Ok(res)
}

/// Read spec.md files for the given spec ids.
#[tauri::command(rename_all = "camelCase")]
pub async fn changelog_load_task_specs(
    project_path: String,
    spec_ids: Vec<String>,
) -> AppResult<IpcResult<Value>> {
    let res = tokio::task::spawn_blocking(move || -> IpcResult<Value> {
        let specs_root = PathBuf::from(&project_path).join(SPECS_SUBDIR);
        let mut specs: Vec<Value> = Vec::new();
        for spec_id in &spec_ids {
            let spec_dir = specs_root.join(spec_id);
            let spec_md = spec_dir.join("spec.md");
            let plan_path = spec_dir.join(PLAN_FILENAME);

            let content = fs::read_to_string(&spec_md).unwrap_or_default();
            let feature = fs::read_to_string(&plan_path)
                .ok()
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                .and_then(|plan| plan.get("feature").and_then(|v| v.as_str()).map(String::from))
                .unwrap_or_else(|| spec_id.clone());

            specs.push(json!({
                "specId": spec_id,
                "feature": feature,
                "content": content,
            }));
        }
        IpcResult::ok(json!(specs))
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))?;
    Ok(res)
}

/// Spawn ai_analyzer_runner.py to generate changelog content.
#[tauri::command(rename_all = "camelCase")]
pub async fn changelog_generate(
    app: AppHandle,
    process: State<'_, ChangelogProcess>,
    project_path: String,
    params: Option<Value>,
) -> AppResult<IpcResult<Value>> {
    // Kill any existing process
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

    // Use ai_analyzer_runner.py since no dedicated changelog runner exists
    let runner = backend.join("runners").join("ai_analyzer_runner.py");
    if !runner.exists() {
        // Gracefully degrade: emit an error event so the renderer can show fallback
        let _ = app.emit("changelog:error", json!({ "error": "ai_analyzer_runner_missing" }));
        return Ok(IpcResult::ok(json!({ "started": false, "error": "runner_missing" })));
    }

    let Some(python) = find_python() else {
        return Ok(IpcResult {
            success: false,
            data: None,
            error: Some("python_not_found".to_string()),
        });
    };

    let task_contents = params
        .as_ref()
        .and_then(|p| p.get("taskContents"))
        .cloned()
        .unwrap_or(json!([]));
    let version = params
        .as_ref()
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_str())
        .unwrap_or("unreleased")
        .to_string();

    // Build a prompt for the ai_analyzer
    let prompt = format!(
        "Generate a professional CHANGELOG.md entry for version {version}.\n\
         Format as Keep a Changelog (https://keepachangelog.com).\n\
         Base it on these completed tasks:\n\
         {}\n\
         Output only the CHANGELOG section, starting with ## [{version}].",
        serde_json::to_string_pretty(&task_contents).unwrap_or_default()
    );

    let mut args = vec![
        runner.to_string_lossy().to_string(),
        "--prompt".to_string(),
        prompt,
        "--project".to_string(),
        project_path.clone(),
    ];
    if let Some(p) = &params {
        if let Some(model) = p.get("model").and_then(|v| v.as_str()) {
            args.push("--model".to_string());
            args.push(model.to_string());
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

    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader};

        let mut output = String::new();

        if let Some(out) = stdout {
            let reader = BufReader::new(out);
            for line in reader.lines().flatten() {
                output.push_str(&line);
                output.push('\n');
                let _ = app_clone.emit("changelog:progress", json!({ "message": line }));
            }
        }

        if let Some(err) = stderr {
            let reader = BufReader::new(err);
            for line in reader.lines().flatten() {
                let _ = app_clone.emit("changelog:progress", json!({ "message": line, "stream": "stderr" }));
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

        match exit_status {
            Some(s) if s.success() => {
                let _ = app_clone.emit("changelog:complete", json!({ "content": output }));
            }
            Some(_) => {
                let _ = app_clone.emit("changelog:error", json!({ "error": "runner_failed" }));
            }
            None => {
                let _ = app_clone.emit("changelog:error", json!({ "error": "process_killed" }));
            }
        }
    });

    Ok(IpcResult::ok(json!({ "started": true })))
}

/// Write content to CHANGELOG.md (or custom filename).
#[tauri::command(rename_all = "camelCase")]
pub async fn changelog_save(
    project_path: String,
    content: String,
    filename: Option<String>,
) -> AppResult<IpcResult<Value>> {
    let res = tokio::task::spawn_blocking(move || -> AppResult<IpcResult<Value>> {
        let fname = filename.as_deref().unwrap_or("CHANGELOG.md");
        let path = PathBuf::from(&project_path).join(fname);
        fs::write(&path, &content).map_err(|e| AppError::new("write_failed", e.to_string()))?;
        Ok(IpcResult::ok(json!({ "saved": true, "path": path.to_string_lossy() })))
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))??;
    Ok(res)
}

/// Read CHANGELOG.md, returning null data if missing.
#[tauri::command(rename_all = "camelCase")]
pub async fn changelog_read_existing(project_path: String) -> AppResult<IpcResult<Value>> {
    let res = tokio::task::spawn_blocking(move || -> IpcResult<Value> {
        let path = PathBuf::from(&project_path).join("CHANGELOG.md");
        if !path.exists() {
            return IpcResult { success: true, data: Some(Value::Null), error: None };
        }
        match fs::read_to_string(&path) {
            Ok(content) => IpcResult::ok(json!({ "content": content })),
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

/// Run `git describe` to suggest the next version.
#[tauri::command(rename_all = "camelCase")]
pub async fn changelog_suggest_version(project_path: String) -> AppResult<IpcResult<Value>> {
    let res = tokio::task::spawn_blocking(move || -> IpcResult<Value> {
        let output = Command::new("git")
            .args(["describe", "--tags", "--abbrev=0"])
            .current_dir(&project_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output();
        let latest_tag = match output {
            Ok(out) if out.status.success() => {
                String::from_utf8_lossy(&out.stdout).trim().to_string()
            }
            _ => "v0.0.0".to_string(),
        };
        // Bump patch version
        let suggested = bump_patch_version(&latest_tag);
        IpcResult::ok(json!({ "currentTag": latest_tag, "suggested": suggested }))
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))?;
    Ok(res)
}

/// Analyze git log between two refs to suggest a version bump.
#[tauri::command(rename_all = "camelCase")]
pub async fn changelog_suggest_version_from_commits(
    project_path: String,
    from_tag: String,
    to_ref: String,
) -> AppResult<IpcResult<Value>> {
    let res = tokio::task::spawn_blocking(move || -> IpcResult<Value> {
        let range = format!("{}..{}", from_tag, to_ref);
        let output = Command::new("git")
            .args(["log", "--oneline", &range])
            .current_dir(&project_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output();

        let commits: Vec<String> = match output {
            Ok(out) if out.status.success() => {
                String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .map(|l| l.to_string())
                    .collect()
            }
            _ => vec![],
        };

        // Heuristic: any commit starting with "feat!" or "BREAKING" → major;
        // "feat:" → minor; else patch.
        let has_breaking = commits.iter().any(|c| {
            c.contains("BREAKING") || c.contains("feat!") || c.contains("fix!")
        });
        let has_feat = commits.iter().any(|c| c.contains("feat:") || c.contains("feat("));
        let bump_type = if has_breaking {
            "major"
        } else if has_feat {
            "minor"
        } else {
            "patch"
        };
        let suggested = bump_version(&from_tag, bump_type);
        IpcResult::ok(json!({
            "fromTag": from_tag,
            "commits": commits,
            "bumpType": bump_type,
            "suggested": suggested,
        }))
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))?;
    Ok(res)
}

/// List git tags sorted by creator date descending.
#[tauri::command(rename_all = "camelCase")]
pub async fn changelog_get_tags(project_path: String) -> AppResult<IpcResult<Value>> {
    let res = tokio::task::spawn_blocking(move || -> IpcResult<Value> {
        let output = Command::new("git")
            .args(["tag", "--sort=-creatordate"])
            .current_dir(&project_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output();
        match output {
            Ok(out) if out.status.success() => {
                let tags: Vec<Value> = String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .filter(|l| !l.is_empty())
                    .map(|l| json!({ "name": l.trim() }))
                    .collect();
                IpcResult::ok(json!(tags))
            }
            _ => IpcResult::ok(json!([])),
        }
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))?;
    Ok(res)
}

/// Git log --oneline between two refs.
#[tauri::command(rename_all = "camelCase")]
pub async fn changelog_get_commits_preview(
    project_path: String,
    from: String,
    to: String,
) -> AppResult<IpcResult<Value>> {
    let res = tokio::task::spawn_blocking(move || -> IpcResult<Value> {
        let range = format!("{}..{}", from, to);
        let output = Command::new("git")
            .args(["log", "--oneline", &range])
            .current_dir(&project_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output();
        match output {
            Ok(out) if out.status.success() => {
                let commits: Vec<Value> = String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .filter(|l| !l.is_empty())
                    .map(|l| {
                        let mut parts = l.splitn(2, ' ');
                        let hash = parts.next().unwrap_or("").to_string();
                        let msg = parts.next().unwrap_or("").to_string();
                        json!({ "hash": hash, "message": msg })
                    })
                    .collect();
                IpcResult::ok(json!(commits))
            }
            _ => IpcResult::ok(json!([])),
        }
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))?;
    Ok(res)
}

/// Decode a base64 data URL and write it as a file.
#[tauri::command(rename_all = "camelCase")]
pub async fn changelog_save_image(
    project_path: String,
    filename: String,
    data_url: String,
) -> AppResult<IpcResult<Value>> {
    let res = tokio::task::spawn_blocking(move || -> AppResult<IpcResult<Value>> {
        // data_url format: data:<mime>;base64,<data>
        let b64 = data_url
            .splitn(2, ',')
            .nth(1)
            .ok_or_else(|| AppError::new("invalid_data_url", "no comma separator"))?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| AppError::new("base64_decode_failed", e.to_string()))?;
        let path = PathBuf::from(&project_path).join(&filename);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| AppError::new("mkdir_failed", e.to_string()))?;
        }
        fs::write(&path, &bytes).map_err(|e| AppError::new("write_failed", e.to_string()))?;
        Ok(IpcResult::ok(json!({ "saved": true, "path": path.to_string_lossy() })))
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))??;
    Ok(res)
}

/// Read a file and return it as a base64 data URL.
#[tauri::command(rename_all = "camelCase")]
pub async fn changelog_read_local_image(path: String) -> AppResult<IpcResult<Value>> {
    let res = tokio::task::spawn_blocking(move || -> AppResult<IpcResult<Value>> {
        let p = PathBuf::from(&path);
        if !p.exists() {
            return Ok(IpcResult {
                success: false,
                data: None,
                error: Some("file_not_found".to_string()),
            });
        }
        let bytes = fs::read(&p).map_err(|e| AppError::new("read_failed", e.to_string()))?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("png").to_lowercase();
        let mime = match ext.as_str() {
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "svg" => "image/svg+xml",
            _ => "image/png",
        };
        let data_url = format!("data:{mime};base64,{b64}");
        Ok(IpcResult::ok(json!({ "dataUrl": data_url })))
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))??;
    Ok(res)
}

// --- version helpers ---

fn parse_semver(tag: &str) -> (u32, u32, u32) {
    let stripped = tag.trim_start_matches('v');
    let parts: Vec<&str> = stripped.split('.').collect();
    let major = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minor = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let patch = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    (major, minor, patch)
}

fn bump_patch_version(tag: &str) -> String {
    let (maj, min, pat) = parse_semver(tag);
    let prefix = if tag.starts_with('v') { "v" } else { "" };
    format!("{prefix}{maj}.{min}.{}", pat + 1)
}

fn bump_version(tag: &str, bump_type: &str) -> String {
    let (maj, min, pat) = parse_semver(tag);
    let prefix = if tag.starts_with('v') { "v" } else { "" };
    match bump_type {
        "major" => format!("{prefix}{}.0.0", maj + 1),
        "minor" => format!("{prefix}{maj}.{}.0", min + 1),
        _ => format!("{prefix}{maj}.{min}.{}", pat + 1),
    }
}
