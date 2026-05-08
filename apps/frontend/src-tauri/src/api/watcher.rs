//! Specs file watcher (Phase 6b).
//!
//! Watches each project's `.auto-claude/specs/` directory for changes to
//! `implementation_plan.json` files and emits a `specs:changed` Tauri event
//! so the renderer's Kanban board refreshes automatically — no polling needed.
//!
//! Architecture:
//!   notify::RecommendedWatcher (sync callback)
//!       ↓  tokio::sync::mpsc::channel  (blocking_send)
//!   tokio task reads events, filters noise, emits Tauri event
//!
//! One watcher per project is stored in `SharedWatchers`. Watchers are
//! started automatically at app startup for every registered project, and
//! can be managed at runtime via task_watch_project / task_unwatch_project.

use crate::api::project;
use crate::types::IpcResult;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::{mpsc, Mutex};
use tracing::{info, warn};

pub type SharedWatchers = Arc<Mutex<HashMap<String, RecommendedWatcher>>>;

const SPECS_SUBDIR: &str = ".auto-claude/specs";
const PLAN_FILENAME: &str = "implementation_plan.json";

/// Starts a watcher for a single project's specs directory.
/// Idempotent: if the project is already being watched, returns immediately.
/// If the specs directory does not exist yet, no watcher is started (the
/// renderer will call `task_watch_project` again once the project is set up).
async fn start_watching(
    project_id: &str,
    project_path: &str,
    app: &AppHandle,
    watchers: &SharedWatchers,
) -> Result<(), String> {
    // Already watching?
    {
        let map = watchers.lock().await;
        if map.contains_key(project_id) {
            return Ok(());
        }
    }

    let specs_dir = PathBuf::from(project_path).join(SPECS_SUBDIR);
    if !specs_dir.exists() {
        return Ok(()); // not an error — specs dir will be created on first task
    }

    let (tx, mut rx) = mpsc::channel::<notify::Result<notify::Event>>(64);

    let watcher = RecommendedWatcher::new(
        move |res| {
            // blocking_send is safe here because the channel has buffer=64 and
            // the tokio consumer is always running (dropped only on unwatch).
            let _ = tx.blocking_send(res);
        },
        notify::Config::default(),
    )
    .map_err(|e| e.to_string())?;

    // We need a separate variable to call watch() because we move the watcher
    // into the map after setting it up.
    let mut watcher_to_start = watcher;
    watcher_to_start
        .watch(&specs_dir, RecursiveMode::Recursive)
        .map_err(|e| e.to_string())?;

    // Tokio task: consumes raw events, filters to plan-file changes, emits events.
    let app_c = app.clone();
    let pid = project_id.to_string();
    tokio::spawn(async move {
        while let Some(event_result) = rx.recv().await {
            let event = match event_result {
                Ok(e) => e,
                Err(e) => {
                    warn!("[watcher] notify error for project {}: {}", pid, e);
                    continue;
                }
            };

            // Only care about create/modify events on implementation_plan.json.
            if !matches!(
                event.kind,
                EventKind::Create(_) | EventKind::Modify(_)
            ) {
                continue;
            }

            for path in &event.paths {
                if path
                    .file_name()
                    .map(|n| n == PLAN_FILENAME)
                    .unwrap_or(false)
                {
                    let spec_id = path
                        .parent()
                        .and_then(|p| p.file_name())
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();

                    let _ = app_c.emit(
                        "specs:changed",
                        json!({ "projectId": pid, "specId": spec_id }),
                    );
                }
            }
        }
        info!("[watcher] reader task exited for project {}", pid);
    });

    let mut map = watchers.lock().await;
    map.insert(project_id.to_string(), watcher_to_start);
    info!(
        "[watcher] watching specs for project {} at {:?}",
        project_id, specs_dir
    );
    Ok(())
}

/// Called from `main.rs` setup hook: starts watchers for every project in
/// `projects.json` so the Kanban auto-refreshes from the first load.
pub async fn watch_all_projects(app: AppHandle, watchers: SharedWatchers) {
    let store_path = match project::store_path() {
        Ok(p) => p,
        Err(_) => return,
    };
    let store = project::read_store_at(&store_path);
    for proj in store.projects() {
        let id = proj
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let path = proj
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if id.is_empty() || path.is_empty() {
            continue;
        }
        if let Err(e) = start_watching(&id, &path, &app, &watchers).await {
            warn!("[watcher] could not watch project {}: {}", id, e);
        }
    }
}

// ── Tauri commands ────────────────────────────────────────────────────────────

/// Start watching a project's specs directory (called by the renderer when a
/// project is opened/selected). Safe to call multiple times for the same project.
#[tauri::command]
pub async fn task_watch_project(
    app: AppHandle,
    watchers: State<'_, SharedWatchers>,
    project_id: String,
    project_path: String,
) -> Result<IpcResult<bool>, ()> {
    match start_watching(&project_id, &project_path, &app, &watchers).await {
        Ok(()) => Ok(IpcResult::ok(true)),
        Err(e) => {
            warn!("[watcher] task_watch_project error: {}", e);
            Ok(IpcResult {
                success: false,
                data: None,
                error: Some(e),
            })
        }
    }
}

/// Stop watching a project's specs directory (called when a project is closed
/// or removed). Dropping the watcher also terminates the reader tokio task.
#[tauri::command]
pub async fn task_unwatch_project(
    watchers: State<'_, SharedWatchers>,
    project_id: String,
) -> Result<IpcResult<bool>, ()> {
    let mut map = watchers.lock().await;
    let removed = map.remove(&project_id).is_some();
    if removed {
        info!("[watcher] unwatched project {}", project_id);
    }
    Ok(IpcResult::ok(removed))
}
