use crate::api::project;
use crate::error::{AppError, AppResult};
use crate::types::IpcResult;
use serde::Serialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;

fn open_path_or_url(target: &str) {
    if cfg!(target_os = "macos") {
        let _ = std::process::Command::new("open").arg(target).status();
    } else if cfg!(target_os = "linux") {
        let _ = std::process::Command::new("xdg-open").arg(target).status();
    } else {
        let _ = std::process::Command::new("cmd")
            .args(["/c", "start", "", target])
            .status();
    }
}

#[tauri::command(rename_all = "camelCase")]
pub async fn shell_open_external(url: String) -> AppResult<()> {
    let u = url.clone();
    tokio::task::spawn_blocking(move || open_path_or_url(&u))
        .await
        .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn shell_select_directory() -> AppResult<Option<String>> {
    tokio::task::spawn_blocking(|| -> Option<String> {
        if cfg!(target_os = "macos") {
            let out = std::process::Command::new("osascript")
                .args(["-e", "POSIX path of (choose folder)"])
                .output()
                .ok()?;
            if out.status.success() {
                let p = String::from_utf8_lossy(&out.stdout)
                    .trim()
                    .trim_end_matches('/')
                    .to_string();
                if p.is_empty() { None } else { Some(p) }
            } else {
                None
            }
        } else if cfg!(target_os = "linux") {
            for (prog, arg) in [
                ("zenity", "--file-selection"),
                ("kdialog", "--getexistingdirectory"),
            ] {
                if let Ok(out) = std::process::Command::new(prog)
                    .args(["--directory", arg])
                    .output()
                {
                    if out.status.success() {
                        let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
                        if !p.is_empty() {
                            return Some(p);
                        }
                    }
                }
            }
            None
        } else {
            let script = r#"Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.FolderBrowserDialog; if ($d.ShowDialog() -eq 'OK') { $d.SelectedPath }"#;
            let out = std::process::Command::new("powershell")
                .args(["-NoProfile", "-Command", script])
                .output()
                .ok()?;
            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if p.is_empty() { None } else { Some(p) }
        }
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn shell_get_default_project_location() -> Option<String> {
    dirs::desktop_dir()
        .or_else(dirs::document_dir)
        .or_else(dirs::home_dir)
        .map(|p| p.to_string_lossy().to_string())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn shell_open_terminal(dir_path: String) -> AppResult<IpcResult<()>> {
    let path = dir_path.clone();
    tokio::task::spawn_blocking(move || {
        if cfg!(target_os = "macos") {
            let _ = std::process::Command::new("open")
                .args(["-a", "Terminal", &path])
                .spawn();
        } else if cfg!(target_os = "linux") {
            for term in ["warp-terminal", "gnome-terminal", "konsole", "xterm"] {
                if std::process::Command::new(term)
                    .args(["--working-directory", &path])
                    .spawn()
                    .is_ok()
                {
                    break;
                }
            }
        } else {
            let _ = std::process::Command::new("cmd")
                .args(["/c", "start", "cmd", "/k", &format!("cd /d \"{}\"", path)])
                .spawn();
        }
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))?;
    Ok(IpcResult::ok(()))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateFolderResult {
    pub path: String,
    pub git_initialized: bool,
}

#[tauri::command(rename_all = "camelCase")]
pub async fn shell_create_project_folder(
    location: String,
    name: String,
    init_git: bool,
) -> AppResult<IpcResult<CreateFolderResult>> {
    let folder = PathBuf::from(&location).join(&name);
    std::fs::create_dir_all(&folder)
        .map_err(|e| AppError::new("create_dir_failed", e.to_string()))?;

    let mut git_initialized = false;
    if init_git {
        git_initialized = std::process::Command::new("git")
            .arg("init")
            .current_dir(&folder)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
    }

    Ok(IpcResult::ok(CreateFolderResult {
        path: folder.to_string_lossy().to_string(),
        git_initialized,
    }))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn shell_search_all_projects(query: String) -> AppResult<IpcResult<Value>> {
    let store_path = project::store_path()?;
    let store = project::read_store_at(&store_path);
    let q = query.to_lowercase();

    let results: Vec<Value> = store
        .projects()
        .into_iter()
        .filter(|p| {
            let name = p
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            let path = p
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            let desc = p
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            name.contains(&q) || path.contains(&q) || desc.contains(&q)
        })
        .map(|p| {
            json!({
                "type": "project",
                "id": p.get("id"),
                "name": p.get("name"),
                "path": p.get("path"),
                "description": p.get("description"),
            })
        })
        .collect();

    Ok(IpcResult::ok(json!(results)))
}
