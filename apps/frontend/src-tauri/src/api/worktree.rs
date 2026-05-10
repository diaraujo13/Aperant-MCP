use crate::api::project;
use crate::error::{AppError, AppResult};
use crate::types::IpcResult;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;

const WORKTREE_SUBDIR: &str = ".auto-claude/worktrees/tasks";

fn find_worktree_path(task_id: &str) -> Option<PathBuf> {
    let store_path = project::store_path().ok()?;
    let store = project::read_store_at(&store_path);
    for p in store.projects() {
        let proj_path = p.get("path").and_then(|v| v.as_str())?;
        let wt = PathBuf::from(proj_path).join(WORKTREE_SUBDIR).join(task_id);
        if wt.is_dir() {
            return Some(wt);
        }
    }
    None
}

fn probe_binary(name: &str) -> bool {
    std::process::Command::new(if cfg!(windows) { "where" } else { "which" })
        .arg(name)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn check_app_exists(path: &str) -> bool {
    std::path::Path::new(path).exists()
}

fn detect_ides() -> Vec<Value> {
    let mut ides = Vec::new();

    let vscode_installed = if cfg!(target_os = "macos") {
        check_app_exists("/Applications/Visual Studio Code.app") || probe_binary("code")
    } else {
        probe_binary("code")
    };
    ides.push(
        json!({ "id": "vscode", "name": "VS Code", "path": "code", "installed": vscode_installed }),
    );

    let cursor_installed = if cfg!(target_os = "macos") {
        check_app_exists("/Applications/Cursor.app") || probe_binary("cursor")
    } else {
        probe_binary("cursor")
    };
    ides.push(json!({ "id": "cursor", "name": "Cursor", "path": "cursor", "installed": cursor_installed }));

    let windsurf_installed =
        probe_binary("windsurf") || check_app_exists("/Applications/Windsurf.app");
    ides.push(json!({ "id": "windsurf", "name": "Windsurf", "path": "windsurf", "installed": windsurf_installed }));

    let idea_installed = check_app_exists("/Applications/IntelliJ IDEA.app")
        || check_app_exists("/Applications/IntelliJ IDEA Ultimate.app")
        || probe_binary("idea");
    ides.push(json!({ "id": "intellij", "name": "IntelliJ IDEA", "path": "idea", "installed": idea_installed }));

    ides
}

fn detect_terminals() -> Vec<Value> {
    let mut terminals = Vec::new();

    if cfg!(target_os = "macos") {
        terminals.push(json!({ "id": "warp", "name": "Warp", "path": "warp", "installed": check_app_exists("/Applications/Warp.app") }));
        terminals.push(json!({ "id": "iterm2", "name": "iTerm2", "path": "iterm2", "installed": check_app_exists("/Applications/iTerm.app") }));
        terminals.push(
            json!({ "id": "terminal", "name": "Terminal", "path": "terminal", "installed": true }),
        );
    } else if cfg!(target_os = "linux") {
        for (id, name, bin) in [
            ("warp", "Warp", "warp-terminal"),
            ("gnome-terminal", "GNOME Terminal", "gnome-terminal"),
            ("konsole", "Konsole", "konsole"),
            ("xterm", "XTerm", "xterm"),
        ] {
            terminals.push(
                json!({ "id": id, "name": name, "path": bin, "installed": probe_binary(bin) }),
            );
        }
    } else {
        terminals.push(json!({ "id": "windows-terminal", "name": "Windows Terminal", "path": "wt.exe", "installed": probe_binary("wt") }));
        terminals.push(
            json!({ "id": "cmd", "name": "Command Prompt", "path": "cmd.exe", "installed": true }),
        );
        terminals.push(json!({ "id": "powershell", "name": "PowerShell", "path": "powershell.exe", "installed": true }));
    }

    terminals
}

#[tauri::command(rename_all = "camelCase")]
pub async fn worktree_detect_tools() -> AppResult<IpcResult<Value>> {
    let result = tokio::task::spawn_blocking(
        || json!({ "ides": detect_ides(), "terminals": detect_terminals() }),
    )
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))?;
    Ok(IpcResult::ok(result))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn worktree_get_status(task_id: String) -> AppResult<IpcResult<Value>> {
    let result = tokio::task::spawn_blocking(move || -> Value {
        let Some(wt_path) = find_worktree_path(&task_id) else {
            return json!({ "exists": false, "taskId": task_id });
        };
        let out = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&wt_path)
            .output();
        match out {
            Ok(o) if o.status.success() => {
                let text = String::from_utf8_lossy(&o.stdout).to_string();
                let changes: Vec<Value> = text.lines()
                    .filter(|l| !l.is_empty())
                    .map(|l| json!({ "status": &l[..2], "path": l[3..].to_string() }))
                    .collect();
                let count = changes.len();
                json!({
                    "exists": true,
                    "taskId": task_id,
                    "worktreePath": wt_path.to_string_lossy(),
                    "hasChanges": count > 0,
                    "changedFileCount": count,
                    "changes": changes,
                })
            }
            _ => json!({ "exists": true, "taskId": task_id, "worktreePath": wt_path.to_string_lossy(), "hasChanges": false, "changedFileCount": 0, "changes": [] }),
        }
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))?;
    Ok(IpcResult::ok(result))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn worktree_get_diff(task_id: String) -> AppResult<IpcResult<Value>> {
    let result = tokio::task::spawn_blocking(move || -> Value {
        let Some(wt_path) = find_worktree_path(&task_id) else {
            return json!({ "exists": false, "diff": "" });
        };
        let diff = std::process::Command::new("git")
            .args(["diff", "HEAD"])
            .current_dir(&wt_path)
            .output()
            .ok().filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();
        let staged = std::process::Command::new("git")
            .args(["diff", "--cached"])
            .current_dir(&wt_path)
            .output()
            .ok().filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();
        let full = if staged.is_empty() { diff } else if diff.is_empty() { staged } else { format!("{staged}\n{diff}") };
        json!({ "exists": true, "taskId": task_id, "worktreePath": wt_path.to_string_lossy(), "diff": full, "hasDiff": !full.is_empty() })
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))?;
    Ok(IpcResult::ok(result))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn worktree_check_changes(task_id: String) -> AppResult<IpcResult<Value>> {
    let result = tokio::task::spawn_blocking(move || -> Value {
        let Some(wt_path) = find_worktree_path(&task_id) else {
            return json!({ "hasChanges": false });
        };
        let out = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&wt_path)
            .output();
        let (has_changes, count) = out
            .ok().filter(|o| o.status.success())
            .map(|o| {
                let text = String::from_utf8_lossy(&o.stdout).to_string();
                let n = text.lines().filter(|l| !l.is_empty()).count();
                (n > 0, n)
            })
            .unwrap_or((false, 0));
        json!({ "hasChanges": has_changes, "worktreePath": wt_path.to_string_lossy(), "changedFileCount": count })
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))?;
    Ok(IpcResult::ok(result))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn worktree_discard(
    task_id: String,
    _skip_status_change: Option<bool>,
) -> AppResult<IpcResult<Value>> {
    let result = tokio::task::spawn_blocking(move || -> (bool, Option<String>) {
        let store_path = match project::store_path() {
            Ok(p) => p,
            Err(_) => return (false, None),
        };
        let store = project::read_store_at(&store_path);
        for p in store.projects() {
            let proj_path = match p.get("path").and_then(|v| v.as_str()) {
                Some(p) => p.to_string(),
                None => continue,
            };
            let wt = PathBuf::from(&proj_path)
                .join(WORKTREE_SUBDIR)
                .join(&task_id);
            if wt.is_dir() {
                let ok = std::process::Command::new("git")
                    .args(["worktree", "remove", "--force", &wt.to_string_lossy()])
                    .current_dir(&proj_path)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false);
                if !ok {
                    let _ = std::fs::remove_dir_all(&wt);
                }
                return (true, Some(proj_path));
            }
        }
        (false, None)
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))?;

    if result.0 {
        Ok(IpcResult::ok(json!({ "discarded": true })))
    } else {
        Ok(IpcResult {
            success: false,
            data: None,
            error: Some("worktree_not_found".to_string()),
        })
    }
}

#[tauri::command(rename_all = "camelCase")]
pub async fn worktree_discard_orphaned(
    project_id: String,
    spec_name: String,
) -> AppResult<IpcResult<Value>> {
    let result = tokio::task::spawn_blocking(move || -> bool {
        let store_path = match project::store_path() {
            Ok(p) => p,
            Err(_) => return false,
        };
        let store = project::read_store_at(&store_path);
        let proj_path = store
            .projects()
            .into_iter()
            .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(&project_id))
            .and_then(|p| p.get("path").and_then(|v| v.as_str()).map(String::from));
        let Some(proj_path) = proj_path else {
            return false;
        };
        let wt = PathBuf::from(&proj_path)
            .join(WORKTREE_SUBDIR)
            .join(&spec_name);
        if wt.is_dir() {
            let ok = std::process::Command::new("git")
                .args(["worktree", "remove", "--force", &wt.to_string_lossy()])
                .current_dir(&proj_path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                let _ = std::fs::remove_dir_all(&wt);
            }
            true
        } else {
            false
        }
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))?;
    Ok(IpcResult::ok(json!({ "discarded": result })))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn worktree_list(
    project_id: String,
    _include_stats: Option<bool>,
) -> AppResult<IpcResult<Value>> {
    let result = tokio::task::spawn_blocking(move || -> Value {
        let store_path = match project::store_path() {
            Ok(p) => p,
            Err(_) => return json!({ "worktrees": [], "totalCount": 0 }),
        };
        let store = project::read_store_at(&store_path);
        let proj_path = store
            .projects()
            .into_iter()
            .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(&project_id))
            .and_then(|p| p.get("path").and_then(|v| v.as_str()).map(String::from));
        let Some(proj_path) = proj_path else {
            return json!({ "worktrees": [], "totalCount": 0 });
        };
        let wt_dir = PathBuf::from(&proj_path).join(WORKTREE_SUBDIR);
        if !wt_dir.is_dir() {
            return json!({ "worktrees": [], "totalCount": 0 });
        }
        let worktrees: Vec<Value> = std::fs::read_dir(&wt_dir)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                json!({ "taskId": name, "path": e.path().to_string_lossy(), "exists": true })
            })
            .collect();
        let count = worktrees.len();
        json!({ "worktrees": worktrees, "totalCount": count })
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))?;
    Ok(IpcResult::ok(result))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn worktree_open_in_ide(
    worktree_path: String,
    ide: String,
    custom_path: Option<String>,
) -> AppResult<IpcResult<Value>> {
    tokio::task::spawn_blocking(move || {
        let cmd = custom_path.as_deref().unwrap_or(match ide.as_str() {
            "vscode" => "code",
            "cursor" => "cursor",
            "windsurf" => "windsurf",
            _ => &ide,
        });
        let _ = std::process::Command::new(cmd)
            .arg(&worktree_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))?;
    Ok(IpcResult::ok(json!({ "opened": true })))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn worktree_open_in_terminal(
    worktree_path: String,
    terminal: String,
    custom_path: Option<String>,
) -> AppResult<IpcResult<Value>> {
    tokio::task::spawn_blocking(move || {
        if cfg!(target_os = "macos") {
            let app = match terminal.as_str() {
                "iterm2" => "iTerm",
                "warp" => "Warp",
                _ => "Terminal",
            };
            let _ = std::process::Command::new("open")
                .args(["-a", app, &worktree_path])
                .spawn();
        } else if cfg!(target_os = "linux") {
            let term = custom_path.as_deref().unwrap_or(match terminal.as_str() {
                "gnome-terminal" => "gnome-terminal",
                "konsole" => "konsole",
                _ => "xterm",
            });
            let _ = std::process::Command::new(term)
                .args(["--working-directory", &worktree_path])
                .spawn();
        } else {
            let _ = std::process::Command::new("cmd")
                .args([
                    "/c",
                    "start",
                    "cmd",
                    "/k",
                    &format!("cd /d \"{}\"", worktree_path),
                ])
                .spawn();
        }
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))?;
    Ok(IpcResult::ok(json!({ "opened": true })))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn worktree_merge(
    _task_id: String,
    _no_commit: Option<bool>,
) -> AppResult<IpcResult<Value>> {
    Ok(IpcResult {
        success: false,
        data: None,
        error: Some("worktree_merge_requires_python_backend".to_string()),
    })
}

#[tauri::command(rename_all = "camelCase")]
pub async fn worktree_merge_preview(_task_id: String) -> AppResult<IpcResult<Value>> {
    Ok(IpcResult {
        success: false,
        data: None,
        error: Some("worktree_merge_preview_requires_python_backend".to_string()),
    })
}

#[tauri::command(rename_all = "camelCase")]
pub async fn worktree_create_pr(
    _task_id: String,
    _options: Option<Value>,
) -> AppResult<IpcResult<Value>> {
    Ok(IpcResult {
        success: false,
        data: None,
        error: Some("worktree_create_pr_requires_python_backend".to_string()),
    })
}

#[tauri::command(rename_all = "camelCase")]
pub async fn worktree_clear_staged(_task_id: String) -> AppResult<IpcResult<Value>> {
    Ok(IpcResult::ok(json!({ "cleared": false })))
}
