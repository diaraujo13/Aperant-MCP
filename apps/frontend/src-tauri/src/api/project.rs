use crate::error::{AppError, AppResult};
use crate::types::IpcResult;
use fs2::FileExt;
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use tracing::warn;

const APP_NAME: &str = "auto-claude-ui";

/// Resolves to `<userData>/store/projects.json` matching Electron's path so
/// the Electron and Tauri builds share project state during parallel ship.
pub(crate) fn store_path() -> AppResult<PathBuf> {
    let base = dirs::config_dir()
        .ok_or_else(|| AppError::new("no_config_dir", "Could not resolve OS config directory"))?;
    Ok(base.join(APP_NAME).join("store").join("projects.json"))
}

#[derive(Default, Debug)]
pub(crate) struct Store {
    raw: Value,
}

impl Store {
    fn from_value(v: Value) -> Self {
        if v.is_object() {
            Self { raw: v }
        } else {
            Self {
                raw: Value::Object(Default::default()),
            }
        }
    }

    fn ensure_object(&mut self) -> &mut serde_json::Map<String, Value> {
        if !self.raw.is_object() {
            self.raw = Value::Object(Default::default());
        }
        self.raw.as_object_mut().expect("ensured above")
    }

    pub(crate) fn projects(&self) -> Vec<Value> {
        self.raw
            .get("projects")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
    }

    fn set_projects(&mut self, projects: Vec<Value>) {
        let map = self.ensure_object();
        map.insert("projects".to_string(), Value::Array(projects));
    }

    fn tab_state(&self) -> Value {
        self.raw.get("tabState").cloned().unwrap_or_else(|| {
            json!({
                "openProjectIds": [],
                "activeProjectId": null,
                "tabOrder": [],
            })
        })
    }

    fn set_tab_state(&mut self, tab_state: Value) {
        let map = self.ensure_object();
        map.insert("tabState".to_string(), tab_state);
    }

    fn kanban_preferences_for(&self, project_id: &str) -> Value {
        self.raw
            .get("kanbanPreferences")
            .and_then(|v| v.get(project_id))
            .cloned()
            .unwrap_or(Value::Null)
    }

    fn set_kanban_preferences(&mut self, project_id: &str, prefs: Value) {
        let map = self.ensure_object();
        let kp = map
            .entry("kanbanPreferences".to_string())
            .or_insert_with(|| Value::Object(Default::default()));
        if let Some(obj) = kp.as_object_mut() {
            obj.insert(project_id.to_string(), prefs);
        }
    }
}

pub(crate) fn read_store_at(path: &Path) -> Store {
    if !path.exists() {
        return Store::default();
    }
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "failed to read projects.json");
            return Store::default();
        }
    };
    let v: Value = serde_json::from_str(&content).unwrap_or_else(|e| {
        warn!(error = %e, "failed to parse projects.json, treating as empty");
        Value::Object(Default::default())
    });
    Store::from_value(v)
}

fn write_store_at(path: &Path, store: &Store) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| AppError::new("create_dir_failed", e.to_string()))?;
    }
    let pretty = serde_json::to_string_pretty(&store.raw)
        .map_err(|e| AppError::new("serialize_failed", e.to_string()))?;
    let temp_path = path.with_extension("json.tmp");
    fs::write(&temp_path, pretty)
        .map_err(|e| AppError::new("temp_write_failed", e.to_string()))?;
    fs::rename(&temp_path, path).map_err(|e| {
        let _ = fs::remove_file(&temp_path);
        AppError::new("rename_failed", e.to_string())
    })?;
    Ok(())
}

/// Acquires an exclusive cross-process lock around a read-modify-write cycle.
/// Same pattern as settings::save_with_lock — keeps Electron and Tauri builds
/// from corrupting projects.json when both are running.
fn mutate_store<F>(mutator: F) -> AppResult<Store>
where
    F: FnOnce(&mut Store) -> AppResult<()>,
{
    let path = store_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| AppError::new("create_dir_failed", e.to_string()))?;
    }
    let lock_path = path.with_extension("json.lock");
    let lock_file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| AppError::new("lock_open_failed", e.to_string()))?;
    lock_file
        .lock_exclusive()
        .map_err(|e| AppError::new("lock_acquire_failed", e.to_string()))?;

    let mut store = read_store_at(&path);
    let result = mutator(&mut store);
    let write_result = if result.is_ok() {
        write_store_at(&path, &store)
    } else {
        Ok(())
    };

    let _ = FileExt::unlock(&lock_file);
    result?;
    write_result?;
    Ok(store)
}

/// Best-effort absolute path. Canonicalize when the path exists; otherwise
/// pass through. Matches Electron's `ensureAbsolutePath` permissiveness for
/// not-yet-existing paths used when adding a planned project folder.
fn normalize_path(input: &str) -> String {
    let p = PathBuf::from(input);
    if p.exists() {
        match dunce::canonicalize_or(&p) {
            Some(canonical) => canonical.to_string_lossy().to_string(),
            None => input.to_string(),
        }
    } else {
        input.to_string()
    }
}

mod dunce {
    use std::path::{Path, PathBuf};
    /// Wrapper around `std::fs::canonicalize` that returns the verbatim path
    /// stripped of Windows `\\?\` prefix when possible. Implementing inline to
    /// avoid pulling in the dunce crate for a one-call use.
    pub fn canonicalize_or(p: &Path) -> Option<PathBuf> {
        let canonical = std::fs::canonicalize(p).ok()?;
        let s = canonical.to_string_lossy();
        if let Some(stripped) = s.strip_prefix(r"\\?\") {
            Some(PathBuf::from(stripped))
        } else {
            Some(canonical)
        }
    }
}

#[tauri::command(rename_all = "camelCase")]
pub async fn project_list() -> AppResult<IpcResult<Vec<Value>>> {
    let path = store_path()?;
    let store = read_store_at(&path);
    Ok(IpcResult::ok(store.projects()))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn project_add(project_path: String) -> AppResult<IpcResult<Value>> {
    let absolute = normalize_path(&project_path);
    let store = mutate_store(|store| {
        let mut projects = store.projects();
        if projects
            .iter()
            .any(|p| p.get("path").and_then(|v| v.as_str()) == Some(&absolute))
        {
            return Err(AppError::new(
                "duplicate_project",
                format!("Project at {absolute} is already added"),
            ));
        }
        let now = chrono::Utc::now().to_rfc3339();
        let name = std::path::Path::new(&absolute)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "Untitled".to_string());
        let new_project = json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "name": name,
            "path": absolute,
            "autoBuildPath": "",
            "settings": {
                "model": "claude-sonnet-4-5",
                "memoryBackend": "file",
                "linearSync": false,
                "notifications": {
                    "humanReview": true,
                    "errors": true,
                    "completion": true,
                },
                "graphitiMcpEnabled": false,
            },
            "createdAt": now,
            "updatedAt": now,
        });
        projects.push(new_project);
        store.set_projects(projects);
        Ok(())
    })?;

    let projects = store.projects();
    let added = projects.last().cloned().unwrap_or(Value::Null);
    Ok(IpcResult::ok(added))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn project_remove(project_id: String) -> AppResult<IpcResult<()>> {
    mutate_store(|store| {
        let filtered: Vec<Value> = store
            .projects()
            .into_iter()
            .filter(|p| p.get("id").and_then(|v| v.as_str()) != Some(&project_id))
            .collect();
        store.set_projects(filtered);

        // Also clean up tab state references
        let mut tab_state = store.tab_state();
        if let Some(obj) = tab_state.as_object_mut() {
            for key in ["openProjectIds", "tabOrder"] {
                if let Some(arr) = obj.get_mut(key).and_then(|v| v.as_array_mut()) {
                    arr.retain(|v| v.as_str() != Some(&project_id));
                }
            }
            if obj.get("activeProjectId").and_then(|v| v.as_str()) == Some(&project_id) {
                obj.insert("activeProjectId".to_string(), Value::Null);
            }
        }
        store.set_tab_state(tab_state);
        Ok(())
    })?;
    Ok(IpcResult::ok(()))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn project_update_settings(
    project_id: String,
    settings: Value,
) -> AppResult<IpcResult<()>> {
    mutate_store(|store| {
        let now = chrono::Utc::now().to_rfc3339();
        let mut projects = store.projects();
        let mut found = false;
        for p in projects.iter_mut() {
            if p.get("id").and_then(|v| v.as_str()) == Some(&project_id) {
                if let Some(p_obj) = p.as_object_mut() {
                    let current_settings = p_obj
                        .entry("settings".to_string())
                        .or_insert_with(|| Value::Object(Default::default()));
                    if let (Some(cur), Some(patch)) =
                        (current_settings.as_object_mut(), settings.as_object())
                    {
                        for (k, v) in patch {
                            cur.insert(k.clone(), v.clone());
                        }
                    }
                    p_obj.insert("updatedAt".to_string(), json!(now));
                }
                found = true;
                break;
            }
        }
        if !found {
            return Err(AppError::new(
                "project_not_found",
                format!("No project with id {project_id}"),
            ));
        }
        store.set_projects(projects);
        Ok(())
    })?;
    Ok(IpcResult::ok(()))
}

fn toggle_project_setting(
    project_id: &str,
    key: &str,
    enabled: bool,
) -> AppResult<Value> {
    mutate_store(|store| {
        let now = chrono::Utc::now().to_rfc3339();
        let mut projects = store.projects();
        let mut found = false;
        for p in projects.iter_mut() {
            if p.get("id").and_then(|v| v.as_str()) == Some(project_id) {
                if let Some(p_obj) = p.as_object_mut() {
                    let current_settings = p_obj
                        .entry("settings".to_string())
                        .or_insert_with(|| Value::Object(Default::default()));
                    if let Some(cur) = current_settings.as_object_mut() {
                        cur.insert(key.to_string(), Value::Bool(enabled));
                    }
                    p_obj.insert("updatedAt".to_string(), json!(now));
                }
                found = true;
                break;
            }
        }
        if !found {
            return Err(AppError::new(
                "project_not_found",
                format!("No project with id {project_id}"),
            ));
        }
        store.set_projects(projects);
        Ok(())
    })?;
    Ok(json!({
        "projectId": project_id,
        "key": key,
        "enabled": enabled,
    }))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn project_set_auto_resume_after_rate_limit(
    project_id: String,
    enabled: bool,
) -> AppResult<IpcResult<Value>> {
    Ok(IpcResult::ok(toggle_project_setting(
        &project_id,
        "autoResumeAfterRateLimit",
        enabled,
    )?))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn project_set_rdr_enabled(
    project_id: String,
    enabled: bool,
) -> AppResult<IpcResult<Value>> {
    Ok(IpcResult::ok(toggle_project_setting(
        &project_id,
        "rdrEnabled",
        enabled,
    )?))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn tab_state_get() -> AppResult<IpcResult<Value>> {
    let path = store_path()?;
    let store = read_store_at(&path);
    Ok(IpcResult::ok(store.tab_state()))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn tab_state_save(tab_state: Value) -> AppResult<IpcResult<()>> {
    mutate_store(|store| {
        // Filter referenced ids against actual projects (matches Electron behavior)
        let valid_ids: std::collections::HashSet<String> = store
            .projects()
            .iter()
            .filter_map(|p| {
                p.get("id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
            .collect();

        let mut sanitized = tab_state;
        if let Some(obj) = sanitized.as_object_mut() {
            for key in ["openProjectIds", "tabOrder"] {
                if let Some(arr) = obj.get_mut(key).and_then(|v| v.as_array_mut()) {
                    arr.retain(|v| {
                        v.as_str()
                            .map(|s| valid_ids.contains(s))
                            .unwrap_or(false)
                    });
                }
            }
            let active_valid = obj
                .get("activeProjectId")
                .and_then(|v| v.as_str())
                .map(|s| valid_ids.contains(s))
                .unwrap_or(false);
            if !active_valid {
                obj.insert("activeProjectId".to_string(), Value::Null);
            }
        }
        store.set_tab_state(sanitized);
        Ok(())
    })?;
    Ok(IpcResult::ok(()))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn kanban_preferences_get(
    project_id: String,
) -> AppResult<IpcResult<Value>> {
    let path = store_path()?;
    let store = read_store_at(&path);
    Ok(IpcResult::ok(store.kanban_preferences_for(&project_id)))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn kanban_preferences_save(
    project_id: String,
    preferences: Value,
) -> AppResult<IpcResult<()>> {
    mutate_store(|store| {
        store.set_kanban_preferences(&project_id, preferences);
        Ok(())
    })?;
    Ok(IpcResult::ok(()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;
    use tempfile::TempDir;

    fn fresh_store(tmp: &TempDir) -> PathBuf {
        tmp.path().join("projects.json")
    }

    #[test]
    fn read_missing_returns_empty_store() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("missing.json");
        let store = read_store_at(&path);
        assert!(store.projects().is_empty());
        let tab = store.tab_state();
        assert_eq!(tab["activeProjectId"], Value::Null);
    }

    #[test]
    fn read_malformed_returns_empty_store() {
        let tmp = TempDir::new().unwrap();
        let path = fresh_store(&tmp);
        fs::write(&path, b"{ malformed").unwrap();
        let store = read_store_at(&path);
        assert!(store.projects().is_empty());
    }

    #[test]
    fn write_then_read_round_trip() {
        let tmp = TempDir::new().unwrap();
        let path = fresh_store(&tmp);
        let mut store = Store::default();
        store.set_projects(vec![json!({"id": "p1", "name": "test"})]);
        write_store_at(&path, &store).unwrap();
        let loaded = read_store_at(&path);
        assert_eq!(loaded.projects().len(), 1);
        assert_eq!(loaded.projects()[0]["id"], "p1");
    }

    #[test]
    fn write_does_not_leak_temp_file() {
        let tmp = TempDir::new().unwrap();
        let path = fresh_store(&tmp);
        write_store_at(&path, &Store::default()).unwrap();
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn tab_state_default_shape() {
        let store = Store::default();
        let tab = store.tab_state();
        assert_eq!(tab["openProjectIds"], json!([]));
        assert_eq!(tab["tabOrder"], json!([]));
        assert_eq!(tab["activeProjectId"], Value::Null);
    }

    #[test]
    fn kanban_prefs_returns_null_when_missing() {
        let store = Store::default();
        assert_eq!(store.kanban_preferences_for("anything"), Value::Null);
    }

    #[test]
    fn set_projects_overwrites_array() {
        let mut store = Store::default();
        store.set_projects(vec![json!({"id": "a"}), json!({"id": "b"})]);
        assert_eq!(store.projects().len(), 2);
        store.set_projects(vec![json!({"id": "c"})]);
        assert_eq!(store.projects().len(), 1);
        assert_eq!(store.projects()[0]["id"], "c");
    }

    #[test]
    fn concurrent_writes_do_not_corrupt() {
        // 6 threads each call write_store_at via the lock. All 6 unique IDs survive.
        let tmp = TempDir::new().unwrap();
        let path = Arc::new(fresh_store(&tmp));

        // Seed store with the lock dance so all writers race on the same starting state
        write_store_at(&path, &Store::default()).unwrap();

        let mut handles = Vec::new();
        for i in 0..6 {
            let path = Arc::clone(&path);
            handles.push(thread::spawn(move || {
                let lock_path = path.with_extension("json.lock");
                let lock_file = OpenOptions::new()
                    .create(true)
                    .read(true)
                    .write(true)
                    .truncate(false)
                    .open(&lock_path)
                    .unwrap();
                lock_file.lock_exclusive().unwrap();

                let mut store = read_store_at(&path);
                let mut projects = store.projects();
                projects.push(json!({"id": format!("p{i}"), "name": format!("Project {i}")}));
                store.set_projects(projects);
                write_store_at(&path, &store).unwrap();

                FileExt::unlock(&lock_file).unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let loaded = read_store_at(&path);
        assert_eq!(loaded.projects().len(), 6);
    }
}
