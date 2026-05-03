use crate::api::project;
use crate::error::{AppError, AppResult};
use crate::types::IpcResult;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::warn;

const SPECS_SUBDIR: &str = ".auto-claude/specs";
const PLAN_FILENAME: &str = "implementation_plan.json";
const METADATA_FILENAME: &str = "task_metadata.json";

/// Resolves a project's filesystem path from its UUID by reading projects.json.
fn project_path_by_id(project_id: &str) -> Option<PathBuf> {
    let store_path = project::store_path().ok()?;
    let store = project::read_store_at(&store_path);
    for p in store.projects() {
        if p.get("id").and_then(|v| v.as_str()) == Some(project_id) {
            return p
                .get("path")
                .and_then(|v| v.as_str())
                .map(PathBuf::from);
        }
    }
    None
}

/// Walks every project's specs dir to find a task by its spec id (== task id).
/// Returns (projectId, projectPath, specDir). Used by handlers that receive
/// only a taskId and need to figure out which project owns it.
fn find_task_location(task_id: &str) -> Option<(String, PathBuf, PathBuf)> {
    let store_path = project::store_path().ok()?;
    let store = project::read_store_at(&store_path);
    for p in store.projects() {
        let proj_id = p.get("id").and_then(|v| v.as_str())?;
        let proj_path = p.get("path").and_then(|v| v.as_str())?;
        let spec_dir = PathBuf::from(proj_path).join(SPECS_SUBDIR).join(task_id);
        if spec_dir.is_dir() {
            return Some((proj_id.to_string(), PathBuf::from(proj_path), spec_dir));
        }
    }
    None
}

/// Maps the backend plan status (in implementation_plan.json) to the renderer's
/// TaskStatus enum. Most values pass through; only "pending"/"start_requested"/
/// "plan_review" collapse to the renderer's "backlog" column.
fn map_plan_status_to_task_status(plan_status: &str) -> &'static str {
    match plan_status {
        "pending" | "start_requested" | "plan_review" => "backlog",
        "in_progress" => "in_progress",
        "ai_review" => "ai_review",
        "human_review" => "human_review",
        "done" => "done",
        "pr_created" => "pr_created",
        "errors" | "error" => "error",
        _ => "backlog",
    }
}

fn map_task_status_to_plan_status(task_status: &str) -> &'static str {
    match task_status {
        "backlog" => "pending",
        "in_progress" => "in_progress",
        "ai_review" => "ai_review",
        "human_review" => "human_review",
        "done" => "done",
        "pr_created" => "pr_created",
        "error" => "errors",
        _ => "pending",
    }
}

fn slugify(input: &str) -> String {
    let mut s: String = input
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    let trimmed = s.trim_matches('-').to_string();
    trimmed.chars().take(50).collect()
}

/// Constructs a Task object (matching the renderer's Task interface) from a
/// spec directory. Uses sensible defaults for fields the plan doesn't provide.
/// Returns None for `.gitkeep` and other non-spec entries.
fn build_task_from_disk(project_id: &str, spec_path: &Path) -> Option<Value> {
    let spec_id = spec_path.file_name()?.to_string_lossy().to_string();
    if spec_id.starts_with('.') {
        return None;
    }

    let plan_path = spec_path.join(PLAN_FILENAME);
    let metadata_path = spec_path.join(METADATA_FILENAME);

    let (plan, json_error) = if plan_path.exists() {
        match fs::read_to_string(&plan_path) {
            Ok(content) => match serde_json::from_str::<Value>(&content) {
                Ok(v) => (Some(v), None),
                Err(e) => {
                    warn!(spec = %spec_id, "plan parse error: {e}");
                    (None, Some(format!("Plan parse error: {e}")))
                }
            },
            Err(e) => {
                warn!(spec = %spec_id, "plan read error: {e}");
                (None, Some(format!("Plan read error: {e}")))
            }
        }
    } else {
        return None;
    };

    let metadata: Value = if metadata_path.exists() {
        fs::read_to_string(&metadata_path)
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .unwrap_or(Value::Null)
    } else {
        Value::Null
    };

    let title = plan
        .as_ref()
        .and_then(|p| p.get("feature").and_then(|v| v.as_str()))
        .unwrap_or(spec_id.as_str())
        .to_string();
    let description = plan
        .as_ref()
        .and_then(|p| p.get("description").and_then(|v| v.as_str()))
        .unwrap_or_default()
        .to_string();
    let plan_status = plan
        .as_ref()
        .and_then(|p| p.get("status").and_then(|v| v.as_str()))
        .unwrap_or("pending");
    let phases = plan
        .as_ref()
        .and_then(|p| p.get("phases").cloned())
        .unwrap_or_else(|| Value::Array(vec![]));
    let created_at = plan
        .as_ref()
        .and_then(|p| p.get("created_at").and_then(|v| v.as_str()))
        .unwrap_or_default()
        .to_string();
    let updated_at = plan
        .as_ref()
        .and_then(|p| p.get("updated_at").and_then(|v| v.as_str()))
        .unwrap_or_default()
        .to_string();

    let final_status = if json_error.is_some() {
        "human_review"
    } else {
        map_plan_status_to_task_status(plan_status)
    };
    let final_description = match &json_error {
        Some(err) => format!("[JSON_PARSE_ERROR] {err}"),
        None => description,
    };

    Some(json!({
        "id": spec_id.clone(),
        "specId": spec_id,
        "projectId": project_id,
        "title": title,
        "description": final_description,
        "status": final_status,
        "subtasks": [],
        "phases": phases,
        "logs": [],
        "metadata": metadata,
        "location": "main",
        "specsPath": spec_path.to_string_lossy().to_string(),
        "createdAt": created_at,
        "updatedAt": updated_at,
    }))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn task_list(
    project_id: String,
    _options: Option<Value>,
) -> AppResult<IpcResult<Vec<Value>>> {
    let project_path = project_path_by_id(&project_id).ok_or_else(|| {
        AppError::new(
            "project_not_found",
            format!("No project with id {project_id}"),
        )
    })?;
    let specs_dir = project_path.join(SPECS_SUBDIR);
    if !specs_dir.is_dir() {
        return Ok(IpcResult::ok(vec![]));
    }

    let mut tasks = Vec::new();
    let entries = fs::read_dir(&specs_dir)
        .map_err(|e| AppError::new("read_specs_failed", e.to_string()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if let Some(task) = build_task_from_disk(&project_id, &path) {
            tasks.push(task);
        }
    }
    Ok(IpcResult::ok(tasks))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn task_create(
    project_id: String,
    title: String,
    description: String,
    metadata: Option<Value>,
) -> AppResult<IpcResult<Value>> {
    let project_path = project_path_by_id(&project_id).ok_or_else(|| {
        AppError::new(
            "project_not_found",
            format!("No project with id {project_id}"),
        )
    })?;
    let specs_dir = project_path.join(SPECS_SUBDIR);
    fs::create_dir_all(&specs_dir)
        .map_err(|e| AppError::new("create_specs_dir_failed", e.to_string()))?;

    // Find next spec number by scanning existing dirs for "NNN-*"
    let mut max_num = 0u32;
    if let Ok(entries) = fs::read_dir(&specs_dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if let Some(num_str) = name.split('-').next() {
                    if let Ok(n) = num_str.parse::<u32>() {
                        if n > max_num {
                            max_num = n;
                        }
                    }
                }
            }
        }
    }
    let spec_number = max_num + 1;

    // Title fallback: empty title → first 60 chars of description, or "Untitled"
    let final_title = if title.trim().is_empty() {
        let desc = description.trim();
        if desc.is_empty() {
            "Untitled task".to_string()
        } else {
            desc.chars().take(60).collect::<String>()
        }
    } else {
        title.clone()
    };

    let slug = slugify(&final_title);
    let spec_id = if slug.is_empty() {
        format!("{:03}-untitled", spec_number)
    } else {
        format!("{:03}-{slug}", spec_number)
    };
    let spec_dir = specs_dir.join(&spec_id);
    fs::create_dir_all(&spec_dir)
        .map_err(|e| AppError::new("create_spec_dir_failed", e.to_string()))?;

    let now = chrono::Utc::now().to_rfc3339();
    let plan = json!({
        "feature": final_title,
        "description": description,
        "status": "pending",
        "created_at": now,
        "updated_at": now,
        "phases": [],
    });
    let plan_path = spec_dir.join(PLAN_FILENAME);
    fs::write(
        &plan_path,
        serde_json::to_string_pretty(&plan)
            .map_err(|e| AppError::new("serialize_failed", e.to_string()))?,
    )
    .map_err(|e| AppError::new("write_plan_failed", e.to_string()))?;

    if let Some(meta) = &metadata {
        let metadata_path = spec_dir.join(METADATA_FILENAME);
        fs::write(
            &metadata_path,
            serde_json::to_string_pretty(meta)
                .map_err(|e| AppError::new("serialize_metadata_failed", e.to_string()))?,
        )
        .map_err(|e| AppError::new("write_metadata_failed", e.to_string()))?;
    }

    let task = build_task_from_disk(&project_id, &spec_dir).ok_or_else(|| {
        AppError::new("build_task_failed", "Built task immediately after write but read failed")
    })?;
    Ok(IpcResult::ok(task))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn task_delete(task_id: String) -> AppResult<IpcResult<()>> {
    let (_, _, spec_dir) = find_task_location(&task_id).ok_or_else(|| {
        AppError::new("task_not_found", format!("No task with id {task_id}"))
    })?;
    fs::remove_dir_all(&spec_dir)
        .map_err(|e| AppError::new("remove_failed", e.to_string()))?;
    Ok(IpcResult::ok(()))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn task_update(
    task_id: String,
    updates: Value,
) -> AppResult<IpcResult<Value>> {
    let (project_id, _, spec_dir) = find_task_location(&task_id).ok_or_else(|| {
        AppError::new("task_not_found", format!("No task with id {task_id}"))
    })?;
    let plan_path = spec_dir.join(PLAN_FILENAME);
    if !plan_path.exists() {
        return Err(AppError::new(
            "plan_not_found",
            format!("No plan at {}", plan_path.display()),
        ));
    }

    let content = fs::read_to_string(&plan_path)
        .map_err(|e| AppError::new("read_plan_failed", e.to_string()))?;
    let mut plan: Value = serde_json::from_str(&content)
        .map_err(|e| AppError::new("parse_plan_failed", e.to_string()))?;

    if let (Value::Object(plan_map), Value::Object(updates_map)) = (&mut plan, &updates) {
        for (k, v) in updates_map {
            match k.as_str() {
                "title" => {
                    plan_map.insert("feature".to_string(), v.clone());
                }
                "description" => {
                    plan_map.insert("description".to_string(), v.clone());
                }
                "status" => {
                    if let Some(s) = v.as_str() {
                        plan_map.insert(
                            "status".to_string(),
                            json!(map_task_status_to_plan_status(s)),
                        );
                    }
                }
                other => {
                    plan_map.insert(other.to_string(), v.clone());
                }
            }
        }
        plan_map.insert(
            "updated_at".to_string(),
            json!(chrono::Utc::now().to_rfc3339()),
        );
    }

    fs::write(
        &plan_path,
        serde_json::to_string_pretty(&plan)
            .map_err(|e| AppError::new("serialize_failed", e.to_string()))?,
    )
    .map_err(|e| AppError::new("write_plan_failed", e.to_string()))?;

    let task = build_task_from_disk(&project_id, &spec_dir).ok_or_else(|| {
        AppError::new("build_task_failed", "Failed to rebuild task after update")
    })?;
    Ok(IpcResult::ok(task))
}

fn write_metadata_field(
    spec_dir: &Path,
    mutate: impl FnOnce(&mut serde_json::Map<String, Value>),
) -> AppResult<()> {
    let metadata_path = spec_dir.join(METADATA_FILENAME);
    let mut metadata = if metadata_path.exists() {
        fs::read_to_string(&metadata_path)
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .unwrap_or(Value::Object(Default::default()))
    } else {
        Value::Object(Default::default())
    };
    if !metadata.is_object() {
        metadata = Value::Object(Default::default());
    }
    if let Some(map) = metadata.as_object_mut() {
        mutate(map);
    }
    fs::create_dir_all(
        metadata_path
            .parent()
            .ok_or_else(|| AppError::new("no_parent", "metadata path has no parent"))?,
    )
    .map_err(|e| AppError::new("create_dir_failed", e.to_string()))?;
    fs::write(
        &metadata_path,
        serde_json::to_string_pretty(&metadata)
            .map_err(|e| AppError::new("serialize_failed", e.to_string()))?,
    )
    .map_err(|e| AppError::new("write_metadata_failed", e.to_string()))?;
    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn task_archive(
    project_id: String,
    task_ids: Vec<String>,
    version: Option<String>,
) -> AppResult<IpcResult<bool>> {
    let project_path = project_path_by_id(&project_id).ok_or_else(|| {
        AppError::new(
            "project_not_found",
            format!("No project with id {project_id}"),
        )
    })?;
    let now = chrono::Utc::now().to_rfc3339();
    for task_id in &task_ids {
        let spec_dir = project_path.join(SPECS_SUBDIR).join(task_id);
        if !spec_dir.is_dir() {
            warn!(task_id = %task_id, "task spec dir missing during archive — skipping");
            continue;
        }
        let v = version.clone();
        write_metadata_field(&spec_dir, |map| {
            map.insert("archivedAt".to_string(), json!(now));
            if let Some(version) = v {
                map.insert("archivedInVersion".to_string(), json!(version));
            }
        })?;
    }
    Ok(IpcResult::ok(true))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn task_unarchive(
    project_id: String,
    task_ids: Vec<String>,
) -> AppResult<IpcResult<bool>> {
    let project_path = project_path_by_id(&project_id).ok_or_else(|| {
        AppError::new(
            "project_not_found",
            format!("No project with id {project_id}"),
        )
    })?;
    for task_id in &task_ids {
        let spec_dir = project_path.join(SPECS_SUBDIR).join(task_id);
        if !spec_dir.is_dir() {
            continue;
        }
        write_metadata_field(&spec_dir, |map| {
            map.remove("archivedAt");
            map.remove("archivedInVersion");
        })?;
    }
    Ok(IpcResult::ok(true))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn task_toggle_rdr(
    task_id: String,
    disabled: bool,
) -> AppResult<IpcResult<bool>> {
    let (_, _, spec_dir) = find_task_location(&task_id).ok_or_else(|| {
        AppError::new("task_not_found", format!("No task with id {task_id}"))
    })?;
    write_metadata_field(&spec_dir, |map| {
        map.insert("rdrDisabled".to_string(), json!(disabled));
    })?;
    Ok(IpcResult::ok(true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn slugify_handles_typical_titles() {
        assert_eq!(slugify("Add user login"), "add-user-login");
        assert_eq!(slugify("Fix bug #42!"), "fix-bug-42");
        assert_eq!(slugify("   spaces   "), "spaces");
        assert_eq!(slugify(""), "");
    }

    #[test]
    fn slugify_caps_at_50_chars() {
        let long = "a".repeat(100);
        let s = slugify(&long);
        assert_eq!(s.len(), 50);
    }

    #[test]
    fn map_plan_status_collapses_pending_variants_to_backlog() {
        assert_eq!(map_plan_status_to_task_status("pending"), "backlog");
        assert_eq!(map_plan_status_to_task_status("start_requested"), "backlog");
        assert_eq!(map_plan_status_to_task_status("plan_review"), "backlog");
    }

    #[test]
    fn map_plan_status_passes_through_known_states() {
        assert_eq!(map_plan_status_to_task_status("in_progress"), "in_progress");
        assert_eq!(map_plan_status_to_task_status("done"), "done");
        assert_eq!(map_plan_status_to_task_status("error"), "error");
        assert_eq!(map_plan_status_to_task_status("errors"), "error");
    }

    #[test]
    fn map_task_to_plan_status_round_trips() {
        for s in ["in_progress", "ai_review", "human_review", "done", "pr_created"] {
            assert_eq!(
                map_plan_status_to_task_status(map_task_status_to_plan_status(s)),
                s
            );
        }
        // backlog → pending → backlog
        assert_eq!(
            map_plan_status_to_task_status(map_task_status_to_plan_status("backlog")),
            "backlog"
        );
    }

    #[test]
    fn build_task_from_disk_returns_none_for_dotfile() {
        let tmp = TempDir::new().unwrap();
        let dotted = tmp.path().join(".gitkeep");
        fs::create_dir(&dotted).unwrap();
        assert!(build_task_from_disk("proj", &dotted).is_none());
    }

    #[test]
    fn build_task_from_disk_returns_none_for_dir_without_plan() {
        let tmp = TempDir::new().unwrap();
        let spec = tmp.path().join("001-no-plan");
        fs::create_dir(&spec).unwrap();
        assert!(build_task_from_disk("proj", &spec).is_none());
    }

    #[test]
    fn build_task_from_disk_constructs_expected_shape() {
        let tmp = TempDir::new().unwrap();
        let spec = tmp.path().join("042-add-feature");
        fs::create_dir(&spec).unwrap();
        let plan = json!({
            "feature": "Add feature",
            "description": "Do the thing",
            "status": "in_progress",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-02T00:00:00Z",
            "phases": [{"name": "p1"}]
        });
        fs::write(
            spec.join(PLAN_FILENAME),
            serde_json::to_string(&plan).unwrap(),
        )
        .unwrap();

        let task = build_task_from_disk("proj-1", &spec).unwrap();
        assert_eq!(task["id"], "042-add-feature");
        assert_eq!(task["specId"], "042-add-feature");
        assert_eq!(task["projectId"], "proj-1");
        assert_eq!(task["title"], "Add feature");
        assert_eq!(task["description"], "Do the thing");
        assert_eq!(task["status"], "in_progress");
        assert_eq!(task["phases"], json!([{"name": "p1"}]));
        assert_eq!(task["location"], "main");
    }

    #[test]
    fn build_task_from_disk_marks_malformed_plan_as_error() {
        let tmp = TempDir::new().unwrap();
        let spec = tmp.path().join("099-broken");
        fs::create_dir(&spec).unwrap();
        fs::write(spec.join(PLAN_FILENAME), b"{ not json").unwrap();
        let task = build_task_from_disk("proj", &spec).unwrap();
        assert_eq!(task["status"], "human_review");
        assert!(task["description"]
            .as_str()
            .unwrap()
            .starts_with("[JSON_PARSE_ERROR]"));
    }

    #[test]
    fn build_task_from_disk_collapses_pending_to_backlog() {
        let tmp = TempDir::new().unwrap();
        let spec = tmp.path().join("001-pending");
        fs::create_dir(&spec).unwrap();
        let plan = json!({
            "feature": "Pending",
            "description": "x",
            "status": "pending",
        });
        fs::write(
            spec.join(PLAN_FILENAME),
            serde_json::to_string(&plan).unwrap(),
        )
        .unwrap();
        let task = build_task_from_disk("p", &spec).unwrap();
        assert_eq!(task["status"], "backlog");
    }
}
