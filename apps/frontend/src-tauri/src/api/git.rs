use crate::error::{AppError, AppResult};
use crate::types::IpcResult;
use serde_json::{json, Value};
use std::process::Stdio;

fn run_git_output(cwd: &str, args: &[&str]) -> Option<String> {
    std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn git_get_branches(project_path: String) -> AppResult<IpcResult<Vec<String>>> {
    let branches = tokio::task::spawn_blocking(move || -> Vec<String> {
        run_git_output(&project_path, &["branch", "--format=%(refname:short)"])
            .map(|s| {
                s.lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))?;
    Ok(IpcResult::ok(branches))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn git_get_branches_with_info(project_path: String) -> AppResult<IpcResult<Value>> {
    let result = tokio::task::spawn_blocking(move || -> Value {
        let current = run_git_output(&project_path, &["rev-parse", "--abbrev-ref", "HEAD"])
            .unwrap_or_default();

        let mut all: Vec<Value> =
            run_git_output(&project_path, &["branch", "--format=%(refname:short)"])
                .map(|s| {
                    s.lines()
                        .map(|l| l.trim().to_string())
                        .filter(|l| !l.is_empty())
                        .map(|name| {
                            json!({
                                "name": name,
                                "type": "local",
                                "displayName": name,
                                "isCurrent": name == current,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();

        let remote: Vec<Value> = run_git_output(
            &project_path,
            &["branch", "-r", "--format=%(refname:short)"],
        )
        .map(|s| {
            s.lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty() && !l.ends_with("/HEAD"))
                .map(|name| {
                    json!({
                        "name": name,
                        "type": "remote",
                        "displayName": name,
                        "isCurrent": false,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

        all.extend(remote);
        json!(all)
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))?;
    Ok(IpcResult::ok(result))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn git_get_current_branch(project_path: String) -> AppResult<IpcResult<Option<String>>> {
    let branch = tokio::task::spawn_blocking(move || {
        run_git_output(&project_path, &["rev-parse", "--abbrev-ref", "HEAD"])
            .filter(|s| !s.is_empty())
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))?;
    Ok(IpcResult::ok(branch))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn git_detect_main_branch(project_path: String) -> AppResult<IpcResult<Option<String>>> {
    let branch = tokio::task::spawn_blocking(move || -> Option<String> {
        for candidate in ["main", "master", "develop", "dev"] {
            let exists = std::process::Command::new("git")
                .args(["show-ref", "--verify", &format!("refs/heads/{candidate}")])
                .current_dir(&project_path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if exists {
                return Some(candidate.to_string());
            }
        }
        run_git_output(&project_path, &["rev-parse", "--abbrev-ref", "HEAD"])
            .filter(|s| !s.is_empty())
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))?;
    Ok(IpcResult::ok(branch))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn git_check_status(project_path: String) -> AppResult<IpcResult<Value>> {
    let result = tokio::task::spawn_blocking(move || -> Value {
        let out = std::process::Command::new("git")
            .args(["status", "--porcelain", "-b"])
            .current_dir(&project_path)
            .output();

        match out {
            Ok(o) if o.status.success() => {
                let text = String::from_utf8_lossy(&o.stdout).to_string();
                let mut lines = text.lines();
                let branch_line = lines.next().unwrap_or("").to_string();
                let changes: Vec<Value> = lines
                    .filter(|l| !l.is_empty())
                    .map(|l| json!({ "status": &l[..2], "file": l[3..].to_string() }))
                    .collect();
                let count = changes.len();
                json!({
                    "isGitRepo": true,
                    "branchLine": branch_line,
                    "changes": changes,
                    "hasChanges": count > 0,
                    "changeCount": count,
                })
            }
            _ => {
                json!({ "isGitRepo": false, "changes": [], "hasChanges": false, "changeCount": 0 })
            }
        }
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))?;
    Ok(IpcResult::ok(result))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn git_initialize(project_path: String) -> AppResult<IpcResult<Value>> {
    let ok = tokio::task::spawn_blocking(move || {
        std::process::Command::new("git")
            .arg("init")
            .current_dir(&project_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))?;

    if ok {
        Ok(IpcResult::ok(json!({ "initialized": true })))
    } else {
        Ok(IpcResult {
            success: false,
            data: None,
            error: Some("git_init_failed".to_string()),
        })
    }
}
