//! GitHub domain (Phase 7).
//!
//! All GitHub REST/GraphQL calls delegate to the `gh` CLI so auth is handled
//! transparently — no token management in Rust. AI operations (PR review,
//! investigate, autofix, triage) are marked as deferred stubs; they will be
//! wired to Python subprocess runners in a follow-up phase using the same
//! pattern as `api/agent.rs`.
//!
//! Pattern:
//!   gh api <path> [--method POST] [--input -]  →  JSON
//!   gh pr list --json ...                       →  JSON array
//!   std::fs  →  local config / review state files

use crate::api::project;
use crate::types::IpcResult;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tauri::{AppHandle, Emitter};

// ── constants ─────────────────────────────────────────────────────────────────

const GITHUB_DIR: &str = ".auto-claude/github";
const GITHUB_DEVICE_URL: &str = "https://github.com/login/device";

// ── helpers ───────────────────────────────────────────────────────────────────

fn ok<T: serde::Serialize>(data: T) -> Result<IpcResult<Value>, ()> {
    Ok(IpcResult::ok(
        serde_json::to_value(data).unwrap_or(Value::Null),
    ))
}

fn err(msg: &str) -> Result<IpcResult<Value>, ()> {
    Ok(IpcResult {
        success: false,
        data: None,
        error: Some(msg.to_string()),
    })
}

fn deferred(feature: &str) -> Result<IpcResult<Value>, ()> {
    Ok(IpcResult {
        success: false,
        data: None,
        error: Some(format!("deferred:{feature}")),
    })
}

/// Runs `gh api <path>` and returns parsed JSON. Passes an optional request
/// body via stdin and supports any HTTP method.
async fn gh_api(
    path: &str,
    method: &str,
    body: Option<Value>,
    cwd: Option<&Path>,
) -> Result<Value, String> {
    let mut cmd = tokio::process::Command::new("gh");
    cmd.arg("api").arg(path);
    if method != "GET" {
        cmd.args(["--method", method]);
    }
    if body.is_some() {
        cmd.args(["--input", "-"]);
        cmd.stdin(Stdio::piped());
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }

    let mut child = cmd.spawn().map_err(|e| e.to_string())?;

    if let Some(body_val) = body {
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            let bytes = serde_json::to_vec(&body_val).map_err(|e| e.to_string())?;
            stdin.write_all(&bytes).await.map_err(|e| e.to_string())?;
        }
    }

    let output = child.wait_with_output().await.map_err(|e| e.to_string())?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    if output.stdout.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())
}

/// Runs a `gh` subcommand (not `gh api`) and returns raw stdout as a string.
async fn gh_run(args: &[&str], cwd: Option<&Path>) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new("gh");
    cmd.args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let output = cmd
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Resolves the filesystem path for a project UUID.
fn project_path(project_id: &str) -> Option<PathBuf> {
    let store = project::store_path().ok()?;
    let s = project::read_store_at(&store);
    for p in s.projects() {
        if p.get("id").and_then(|v| v.as_str()) == Some(project_id) {
            return p
                .get("path")
                .and_then(|v| v.as_str())
                .map(PathBuf::from);
        }
    }
    None
}

struct RepoInfo {
    owner: String,
    repo: String,
    path: PathBuf,
}

/// Resolves owner/repo for a project by running `gh repo view` in the project
/// directory. Falls back to `settings.githubRepo` stored in projects.json.
async fn resolve_repo(project_id: &str) -> Result<RepoInfo, String> {
    let path = project_path(project_id).ok_or_else(|| format!("project not found: {project_id}"))?;

    // Try stored repo from project settings first.
    {
        let store = project::store_path()
            .ok()
            .map(|p| project::read_store_at(&p));
        if let Some(store) = store {
            for proj in store.projects() {
                if proj.get("id").and_then(|v| v.as_str()) == Some(project_id) {
                    if let Some(repo) = proj
                        .get("settings")
                        .and_then(|s| s.get("githubRepo"))
                        .and_then(|r| r.as_str())
                    {
                        // format: "owner/repo"
                        if let Some((owner, name)) = repo.split_once('/') {
                            return Ok(RepoInfo {
                                owner: owner.to_string(),
                                repo: name.to_string(),
                                path,
                            });
                        }
                    }
                }
            }
        }
    }

    // Use gh repo view (runs in project dir, reads .git/config remote).
    let output = tokio::process::Command::new("gh")
        .args(["repo", "view", "--json", "owner,name"])
        .current_dir(&path)
        .output()
        .await
        .map_err(|e| e.to_string())?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.trim().to_string());
    }

    let v: Value = serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
    let owner = v["owner"]["login"]
        .as_str()
        .ok_or("missing owner")?
        .to_string();
    let repo = v["name"].as_str().ok_or("missing name")?.to_string();
    Ok(RepoInfo { owner, repo, path })
}

/// Reads a JSON file from `<project_path>/<GITHUB_DIR>/<filename>`.
fn read_github_file(proj_path: &Path, filename: &str) -> Value {
    let p = proj_path.join(GITHUB_DIR).join(filename);
    std::fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null)
}

/// Writes a JSON value to `<project_path>/<GITHUB_DIR>/<filename>`.
fn write_github_file(proj_path: &Path, filename: &str, data: &Value) -> Result<(), String> {
    let dir = proj_path.join(GITHUB_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(data).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(filename), text).map_err(|e| e.to_string())
}

// ── progress parsing ──────────────────────────────────────────────────────────

/// Parses `[  42%] message` from a runner output line.
/// Returns `(percent, message)` or `None`.
fn parse_progress_line(line: &str) -> Option<(u32, String)> {
    let s = line.trim();
    if !s.starts_with('[') {
        return None;
    }
    let close = s.find(']')?;
    let inner = s[1..close].trim();
    let pct_str = inner.trim_end_matches('%');
    let pct: u32 = pct_str.trim().parse().ok()?;
    let rest = s[close + 1..].trim().to_string();
    if rest.is_empty() {
        return None;
    }
    Some((pct, rest))
}

// ── device-flow helpers ───────────────────────────────────────────────────────

/// Scans accumulated text for "code: XXXX-XXXX" (or space separator).
fn find_device_code(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    for marker in &["one-time code: ", "verification code: ", "code: "] {
        if let Some(pos) = lower.find(marker) {
            let after = &text[pos + marker.len()..];
            let candidate: String = after.chars().take(9).collect();
            if candidate.len() < 9 {
                continue;
            }
            let b = candidate.as_bytes();
            let sep = b[4];
            if (sep == b'-' || sep == b' ')
                && b[..4].iter().all(|c| c.is_ascii_alphanumeric())
                && b[5..9].iter().all(|c| c.is_ascii_alphanumeric())
            {
                return Some(candidate.replace(' ', "-"));
            }
        }
    }
    None
}

/// Extracts the device-flow URL from `gh` output, defaulting to the standard URL.
fn find_device_url(text: &str) -> String {
    if let Some(pos) = text.find("https://github.com/login/device") {
        let url: String = text[pos..].chars().take_while(|c| !c.is_whitespace()).collect();
        return url;
    }
    GITHUB_DEVICE_URL.to_string()
}

/// Opens `url` in the OS default browser. Returns whether the launch succeeded.
fn open_in_browser(url: &str) -> bool {
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "linux")]
    let r = std::process::Command::new("xdg-open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let r = std::process::Command::new("cmd").args(["/c", "start", "", url]).spawn();
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let r: Result<std::process::Child, std::io::Error> =
        Err(std::io::Error::new(std::io::ErrorKind::Other, "unsupported"));
    r.is_ok()
}

// ── auth / cli ────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn github_check_cli() -> Result<IpcResult<Value>, ()> {
    match gh_run(&["--version"], None).await {
        Ok(version) => ok(json!({ "installed": true, "version": version })),
        Err(_) => ok(json!({ "installed": false })),
    }
}

#[tauri::command]
pub async fn github_check_auth() -> Result<IpcResult<Value>, ()> {
    match gh_api("user", "GET", None, None).await {
        Ok(user) => ok(json!({
            "authenticated": true,
            "username": user["login"],
        })),
        Err(_) => ok(json!({ "authenticated": false })),
    }
}

#[tauri::command]
pub async fn github_get_token() -> Result<IpcResult<Value>, ()> {
    match gh_run(&["auth", "token"], None).await {
        Ok(token) => ok(json!({ "token": token })),
        Err(e) => err(&e),
    }
}

#[tauri::command]
pub async fn github_get_user() -> Result<IpcResult<Value>, ()> {
    match gh_api("user", "GET", None, None).await {
        Ok(user) => ok(user),
        Err(e) => err(&e),
    }
}

/// Starts `gh auth login --web`, streams stdout/stderr to extract the device
/// code and auth URL, opens the OS browser, and emits `github:auth:device-code`
/// immediately. When gh exits, emits `github:auth:changed` if the account
/// changed. The command blocks (async) until gh exits — may take several minutes
/// while the user completes the browser flow.
#[tauri::command]
pub async fn github_start_auth(app: AppHandle) -> Result<IpcResult<Value>, ()> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    // Snapshot current username to detect account switches after auth.
    let username_before = gh_api("user", "GET", None, None)
        .await
        .ok()
        .and_then(|v| v["login"].as_str().map(|s| s.to_string()));

    let mut child = match tokio::process::Command::new("gh")
        .args(["auth", "login", "--web", "--scopes", "repo"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return Ok(IpcResult {
                success: false,
                data: Some(json!({
                    "success": false,
                    "browserOpened": false,
                    "fallbackUrl": GITHUB_DEVICE_URL,
                    "message": "Failed to start GitHub CLI.",
                })),
                error: Some(e.to_string()),
            });
        }
    };

    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");

    // Merge stdout + stderr into a single channel so we scan both streams.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(128);
    let tx2 = tx.clone();

    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let _ = tx.send(line).await;
        }
    });
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let _ = tx2.send(line).await;
        }
    });

    let mut device_code_sent = false;
    let mut extracted_code: Option<String> = None;
    let mut extracted_url = GITHUB_DEVICE_URL.to_string();
    let mut browser_opened = false;
    let mut accumulated = String::new();

    while let Some(line) = rx.recv().await {
        accumulated.push_str(&line);
        accumulated.push('\n');

        if !device_code_sent {
            if let Some(code) = find_device_code(&accumulated) {
                let url = find_device_url(&accumulated);
                device_code_sent = true;
                extracted_code = Some(code.clone());
                extracted_url = url.clone();
                browser_opened = open_in_browser(&url);
                let _ = app.emit(
                    "github:auth:device-code",
                    json!({
                        "deviceCode": code,
                        "authUrl": url,
                        "browserOpened": browser_opened,
                    }),
                );
            }
        }
    }

    let status = match child.wait().await {
        Ok(s) => s,
        Err(e) => {
            return Ok(IpcResult {
                success: false,
                data: None,
                error: Some(e.to_string()),
            });
        }
    };

    if status.success() {
        let username_after = gh_api("user", "GET", None, None)
            .await
            .ok()
            .and_then(|v| v["login"].as_str().map(|s| s.to_string()));

        if let Some(ref new_user) = username_after {
            if Some(new_user.as_str()) != username_before.as_deref() {
                let _ = app.emit(
                    "github:auth:changed",
                    json!({
                        "oldUsername": username_before,
                        "newUsername": new_user,
                    }),
                );
            }
        }

        Ok(IpcResult::ok(json!({
            "success": true,
            "message": if browser_opened {
                "Successfully authenticated with GitHub"
            } else {
                "Authentication successful. Browser could not be opened automatically."
            },
            "deviceCode": extracted_code,
            "authUrl": extracted_url,
            "browserOpened": browser_opened,
            "fallbackUrl": if !browser_opened { Some(&extracted_url as &str) } else { None },
        })))
    } else {
        Ok(IpcResult {
            success: false,
            data: Some(json!({
                "success": false,
                "deviceCode": extracted_code,
                "authUrl": extracted_url,
                "browserOpened": browser_opened,
                "fallbackUrl": &extracted_url,
                "message": "Authentication failed. Please visit the URL manually.",
            })),
            error: Some(format!(
                "gh exited with code {:?}",
                status.code()
            )),
        })
    }
}

#[tauri::command]
pub async fn github_detect_repo(project_path: String) -> Result<IpcResult<Value>, ()> {
    let path = PathBuf::from(&project_path);
    let output = tokio::process::Command::new("gh")
        .args(["repo", "view", "--json", "owner,name,url,description,isPrivate"])
        .current_dir(&path)
        .output()
        .await;

    match output {
        Ok(o) if o.status.success() => {
            let v: Value = serde_json::from_slice(&o.stdout).unwrap_or(Value::Null);
            ok(json!({
                "detected": true,
                "owner": v["owner"]["login"],
                "repo": v["name"],
                "url": v["url"],
                "description": v["description"],
                "isPrivate": v["isPrivate"],
                "fullName": format!("{}/{}", v["owner"]["login"].as_str().unwrap_or(""), v["name"].as_str().unwrap_or("")),
            }))
        }
        _ => ok(json!({ "detected": false })),
    }
}

#[tauri::command]
pub async fn github_get_branches(project_id: String) -> Result<IpcResult<Value>, ()> {
    match resolve_repo(&project_id).await {
        Ok(r) => {
            let path = format!("repos/{}/{}/branches?per_page=100", r.owner, r.repo);
            match gh_api(&path, "GET", None, Some(&r.path)).await {
                Ok(branches) => ok(branches),
                Err(e) => err(&e),
            }
        }
        Err(e) => err(&e),
    }
}

#[tauri::command]
pub async fn github_list_user_repos() -> Result<IpcResult<Value>, ()> {
    match gh_api("user/repos?per_page=100&sort=updated", "GET", None, None).await {
        Ok(repos) => ok(repos),
        Err(e) => err(&e),
    }
}

#[tauri::command]
pub async fn github_list_orgs() -> Result<IpcResult<Value>, ()> {
    match gh_api("user/orgs?per_page=100", "GET", None, None).await {
        Ok(orgs) => ok(orgs),
        Err(e) => err(&e),
    }
}

#[tauri::command]
pub async fn github_create_repo(
    repo_name: String,
    is_private: bool,
) -> Result<IpcResult<Value>, ()> {
    let visibility = if is_private { "--private" } else { "--public" };
    match gh_run(&["repo", "create", &repo_name, visibility, "--source=."], None).await {
        Ok(_) => ok(json!({ "created": true, "name": repo_name })),
        Err(e) => err(&e),
    }
}

#[tauri::command]
pub async fn github_add_remote(project_path: String, repo_url: String) -> Result<IpcResult<Value>, ()> {
    let output = tokio::process::Command::new("git")
        .args(["remote", "add", "origin", &repo_url])
        .current_dir(&project_path)
        .output()
        .await;
    match output {
        Ok(o) if o.status.success() => ok(json!({ "added": true })),
        Ok(o) => err(&String::from_utf8_lossy(&o.stderr)),
        Err(e) => err(&e.to_string()),
    }
}

// ── repository ────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn github_check_connection(project_id: String) -> Result<IpcResult<Value>, ()> {
    match resolve_repo(&project_id).await {
        Ok(r) => {
            let path = format!("repos/{}/{}", r.owner, r.repo);
            match gh_api(&path, "GET", None, Some(&r.path)).await {
                Ok(repo) => ok(json!({
                    "connected": true,
                    "repoName": repo["name"],
                    "fullName": repo["full_name"],
                    "private": repo["private"],
                    "url": repo["html_url"],
                })),
                Err(e) => ok(json!({ "connected": false, "error": e })),
            }
        }
        Err(_) => ok(json!({ "connected": false })),
    }
}

#[tauri::command]
pub async fn github_get_repositories() -> Result<IpcResult<Value>, ()> {
    // personal + org repos — returns combined list
    match gh_api("user/repos?per_page=100&sort=updated&affiliation=owner,collaborator,organization_member", "GET", None, None).await {
        Ok(repos) => ok(repos),
        Err(e) => err(&e),
    }
}

// ── issues ────────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn github_get_issues(
    project_id: String,
    state: Option<String>,
    page: Option<u32>,
) -> Result<IpcResult<Value>, ()> {
    let r = match resolve_repo(&project_id).await {
        Ok(r) => r,
        Err(e) => return err(&e),
    };
    let state_param = state.as_deref().unwrap_or("open");
    let page_param = page.unwrap_or(1);
    let path = format!(
        "repos/{}/{}/issues?state={}&page={}&per_page=30",
        r.owner, r.repo, state_param, page_param
    );
    match gh_api(&path, "GET", None, Some(&r.path)).await {
        Ok(issues) => ok(json!({ "issues": issues, "page": page_param })),
        Err(e) => err(&e),
    }
}

#[tauri::command]
pub async fn github_get_issue(
    project_id: String,
    issue_number: u32,
) -> Result<IpcResult<Value>, ()> {
    let r = match resolve_repo(&project_id).await {
        Ok(r) => r,
        Err(e) => return err(&e),
    };
    let path = format!("repos/{}/{}/issues/{}", r.owner, r.repo, issue_number);
    match gh_api(&path, "GET", None, Some(&r.path)).await {
        Ok(issue) => ok(issue),
        Err(e) => err(&e),
    }
}

#[tauri::command]
pub async fn github_get_issue_comments(
    project_id: String,
    issue_number: u32,
) -> Result<IpcResult<Value>, ()> {
    let r = match resolve_repo(&project_id).await {
        Ok(r) => r,
        Err(e) => return err(&e),
    };
    let path = format!(
        "repos/{}/{}/issues/{}/comments?per_page=100",
        r.owner, r.repo, issue_number
    );
    match gh_api(&path, "GET", None, Some(&r.path)).await {
        Ok(comments) => ok(comments),
        Err(e) => err(&e),
    }
}

#[tauri::command]
pub async fn github_import_issues(
    _project_id: String,
    _issue_numbers: Vec<u32>,
) -> Result<IpcResult<Value>, ()> {
    // Deferred — requires cross-domain task creation logic.
    deferred("github_import_issues:task-creation")
}

// ── pull requests — read ──────────────────────────────────────────────────────

#[tauri::command]
pub async fn github_pr_list(project_id: String) -> Result<IpcResult<Value>, ()> {
    let r = match resolve_repo(&project_id).await {
        Ok(r) => r,
        Err(e) => return err(&e),
    };
    let path = format!(
        "repos/{}/{}/pulls?state=open&per_page=30&sort=updated",
        r.owner, r.repo
    );
    match gh_api(&path, "GET", None, Some(&r.path)).await {
        Ok(prs) => ok(json!({ "pullRequests": prs, "pageInfo": { "hasNextPage": false } })),
        Err(e) => err(&e),
    }
}

#[tauri::command]
pub async fn github_pr_list_more(
    project_id: String,
    page: Option<u32>,
) -> Result<IpcResult<Value>, ()> {
    let r = match resolve_repo(&project_id).await {
        Ok(r) => r,
        Err(e) => return err(&e),
    };
    let p = page.unwrap_or(2);
    let path = format!(
        "repos/{}/{}/pulls?state=open&per_page=30&page={}",
        r.owner, r.repo, p
    );
    match gh_api(&path, "GET", None, Some(&r.path)).await {
        Ok(prs) => {
            let has_more = prs.as_array().map(|a| a.len() == 30).unwrap_or(false);
            ok(json!({ "pullRequests": prs, "pageInfo": { "hasNextPage": has_more, "endCursor": p + 1 } }))
        }
        Err(e) => err(&e),
    }
}

#[tauri::command]
pub async fn github_pr_get(
    project_id: String,
    pr_number: u32,
) -> Result<IpcResult<Value>, ()> {
    let r = match resolve_repo(&project_id).await {
        Ok(r) => r,
        Err(e) => return err(&e),
    };
    let path = format!("repos/{}/{}/pulls/{}", r.owner, r.repo, pr_number);
    match gh_api(&path, "GET", None, Some(&r.path)).await {
        Ok(pr) => ok(pr),
        Err(e) => err(&e),
    }
}

#[tauri::command]
pub async fn github_pr_get_diff(
    project_id: String,
    pr_number: u32,
) -> Result<IpcResult<Value>, ()> {
    let r = match resolve_repo(&project_id).await {
        Ok(r) => r,
        Err(e) => return err(&e),
    };
    // gh api with diff Accept header
    let mut cmd = tokio::process::Command::new("gh");
    cmd.args([
        "api",
        &format!("repos/{}/{}/pulls/{}", r.owner, r.repo, pr_number),
        "--header", "Accept: application/vnd.github.v3.diff",
    ])
    .current_dir(&r.path)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());

    let output = cmd.output().await.map_err(|e| e.to_string());
    match output {
        Ok(o) if o.status.success() => {
            let diff = String::from_utf8_lossy(&o.stdout).to_string();
            ok(json!({ "diff": diff }))
        }
        Ok(o) => err(&String::from_utf8_lossy(&o.stderr)),
        Err(e) => err(&e),
    }
}

/// Reads the AI review result file written by the Python PR review runner.
#[tauri::command]
pub async fn github_pr_get_review(
    project_id: String,
    pr_number: u32,
) -> Result<IpcResult<Value>, ()> {
    let path = match project_path(&project_id) {
        Some(p) => p,
        None => return err("project not found"),
    };
    let filename = format!("pr/review_{}.json", pr_number);
    let review = read_github_file(&path, &filename);
    if review.is_null() {
        ok(json!({ "exists": false }))
    } else {
        ok(json!({ "exists": true, "review": review }))
    }
}

#[tauri::command]
pub async fn github_pr_get_reviews_batch(
    project_id: String,
    pr_numbers: Vec<u32>,
) -> Result<IpcResult<Value>, ()> {
    let path = match project_path(&project_id) {
        Some(p) => p,
        None => return err("project not found"),
    };
    let reviews: Value = pr_numbers
        .iter()
        .map(|n| {
            let file = format!("pr/review_{}.json", n);
            let review = read_github_file(&path, &file);
            json!({ "prNumber": n, "exists": !review.is_null(), "review": review })
        })
        .collect::<Vec<_>>()
        .into();
    ok(reviews)
}

#[tauri::command]
pub async fn github_pr_check_new_commits(
    project_id: String,
    pr_number: u32,
) -> Result<IpcResult<Value>, ()> {
    let r = match resolve_repo(&project_id).await {
        Ok(r) => r,
        Err(e) => return err(&e),
    };
    let path = format!(
        "repos/{}/{}/pulls/{}/commits",
        r.owner, r.repo, pr_number
    );
    match gh_api(&path, "GET", None, Some(&r.path)).await {
        Ok(commits) => ok(json!({ "commits": commits })),
        Err(e) => err(&e),
    }
}

#[tauri::command]
pub async fn github_pr_check_merge_readiness(
    project_id: String,
    pr_number: u32,
) -> Result<IpcResult<Value>, ()> {
    let r = match resolve_repo(&project_id).await {
        Ok(r) => r,
        Err(e) => return err(&e),
    };
    let path = format!("repos/{}/{}/pulls/{}", r.owner, r.repo, pr_number);
    match gh_api(&path, "GET", None, Some(&r.path)).await {
        Ok(pr) => ok(json!({
            "mergeable": pr["mergeable"],
            "mergeableState": pr["mergeable_state"],
            "state": pr["state"],
            "draft": pr["draft"],
        })),
        Err(e) => err(&e),
    }
}

#[tauri::command]
pub async fn github_pr_get_logs(
    project_id: String,
    pr_number: u32,
) -> Result<IpcResult<Value>, ()> {
    let path = match project_path(&project_id) {
        Some(p) => p,
        None => return err("project not found"),
    };
    let log_file = format!("pr/logs_{}.json", pr_number);
    let logs = read_github_file(&path, &log_file);
    ok(json!({ "logs": logs }))
}

#[tauri::command]
pub async fn github_workflows_awaiting_approval(
    project_id: String,
    _pr_number: u32,
) -> Result<IpcResult<Value>, ()> {
    let r = match resolve_repo(&project_id).await {
        Ok(r) => r,
        Err(e) => return err(&e),
    };
    let path = format!(
        "repos/{}/{}/actions/runs?event=pull_request&status=waiting",
        r.owner, r.repo
    );
    match gh_api(&path, "GET", None, Some(&r.path)).await {
        Ok(v) => ok(v["workflow_runs"].clone()),
        Err(e) => err(&e),
    }
}

// ── pull requests — write ─────────────────────────────────────────────────────

#[tauri::command]
pub async fn github_pr_post_review(
    project_id: String,
    pr_number: u32,
    review_body: String,
    event: Option<String>,
) -> Result<IpcResult<Value>, ()> {
    let r = match resolve_repo(&project_id).await {
        Ok(r) => r,
        Err(e) => return err(&e),
    };
    let path = format!("repos/{}/{}/pulls/{}/reviews", r.owner, r.repo, pr_number);
    let body = json!({
        "body": review_body,
        "event": event.as_deref().unwrap_or("COMMENT"),
    });
    match gh_api(&path, "POST", Some(body), Some(&r.path)).await {
        Ok(v) => ok(v),
        Err(e) => err(&e),
    }
}

#[tauri::command]
pub async fn github_pr_delete_review(
    project_id: String,
    pr_number: u32,
    review_id: u64,
) -> Result<IpcResult<Value>, ()> {
    let r = match resolve_repo(&project_id).await {
        Ok(r) => r,
        Err(e) => return err(&e),
    };
    let path = format!(
        "repos/{}/{}/pulls/{}/reviews/{}",
        r.owner, r.repo, pr_number, review_id
    );
    match gh_api(&path, "DELETE", None, Some(&r.path)).await {
        Ok(_) => ok(json!({ "deleted": true })),
        Err(e) => err(&e),
    }
}

#[tauri::command]
pub async fn github_pr_merge(
    project_id: String,
    pr_number: u32,
) -> Result<IpcResult<Value>, ()> {
    let r = match resolve_repo(&project_id).await {
        Ok(r) => r,
        Err(e) => return err(&e),
    };
    match gh_run(
        &["pr", "merge", &pr_number.to_string(), "--merge", "--repo", &format!("{}/{}", r.owner, r.repo)],
        Some(&r.path),
    )
    .await
    {
        Ok(_) => ok(json!({ "merged": true })),
        Err(e) => err(&e),
    }
}

#[tauri::command]
pub async fn github_pr_assign(
    project_id: String,
    pr_number: u32,
    assignee: String,
) -> Result<IpcResult<Value>, ()> {
    let r = match resolve_repo(&project_id).await {
        Ok(r) => r,
        Err(e) => return err(&e),
    };
    let path = format!(
        "repos/{}/{}/pulls/{}/requested_reviewers",
        r.owner, r.repo, pr_number
    );
    match gh_api(&path, "POST", Some(json!({ "reviewers": [assignee] })), Some(&r.path)).await {
        Ok(v) => ok(v),
        Err(e) => err(&e),
    }
}

#[tauri::command]
pub async fn github_pr_post_comment(
    project_id: String,
    pr_number: u32,
    comment: String,
) -> Result<IpcResult<Value>, ()> {
    let r = match resolve_repo(&project_id).await {
        Ok(r) => r,
        Err(e) => return err(&e),
    };
    let path = format!(
        "repos/{}/{}/issues/{}/comments",
        r.owner, r.repo, pr_number
    );
    match gh_api(&path, "POST", Some(json!({ "body": comment })), Some(&r.path)).await {
        Ok(v) => ok(v),
        Err(e) => err(&e),
    }
}

#[tauri::command]
pub async fn github_pr_mark_review_posted(
    project_id: String,
    pr_number: u32,
) -> Result<IpcResult<Value>, ()> {
    let path = match project_path(&project_id) {
        Some(p) => p,
        None => return err("project not found"),
    };
    let filename = format!("pr/review_{}.json", pr_number);
    let mut review = read_github_file(&path, &filename);
    if let Value::Object(ref mut map) = review {
        map.insert("posted".to_string(), Value::Bool(true));
        let _ = write_github_file(&path, &filename, &review);
    }
    ok(json!({ "marked": true }))
}

#[tauri::command]
pub async fn github_pr_update_branch(
    project_id: String,
    pr_number: u32,
) -> Result<IpcResult<Value>, ()> {
    let r = match resolve_repo(&project_id).await {
        Ok(r) => r,
        Err(e) => return err(&e),
    };
    let path = format!(
        "repos/{}/{}/pulls/{}/update-branch",
        r.owner, r.repo, pr_number
    );
    match gh_api(&path, "PUT", Some(json!({})), Some(&r.path)).await {
        Ok(_) => ok(json!({ "updated": true })),
        Err(e) => err(&e),
    }
}

#[tauri::command]
pub async fn github_workflow_approve(
    project_id: String,
    run_id: u64,
) -> Result<IpcResult<Value>, ()> {
    let r = match resolve_repo(&project_id).await {
        Ok(r) => r,
        Err(e) => return err(&e),
    };
    let path = format!(
        "repos/{}/{}/actions/runs/{}/approve",
        r.owner, r.repo, run_id
    );
    match gh_api(&path, "POST", None, Some(&r.path)).await {
        Ok(_) => ok(json!({ "approved": true })),
        Err(e) => err(&e),
    }
}

// ── config / local state ──────────────────────────────────────────────────────

#[tauri::command]
pub async fn github_autofix_get_config(project_id: String) -> Result<IpcResult<Value>, ()> {
    let path = match project_path(&project_id) {
        Some(p) => p,
        None => return err("project not found"),
    };
    ok(read_github_file(&path, "config.json"))
}

#[tauri::command]
pub async fn github_autofix_save_config(
    project_id: String,
    config: Value,
) -> Result<IpcResult<Value>, ()> {
    let path = match project_path(&project_id) {
        Some(p) => p,
        None => return err("project not found"),
    };
    match write_github_file(&path, "config.json", &config) {
        Ok(()) => ok(json!({ "saved": true })),
        Err(e) => err(&e),
    }
}

#[tauri::command]
pub async fn github_triage_get_config(project_id: String) -> Result<IpcResult<Value>, ()> {
    let path = match project_path(&project_id) {
        Some(p) => p,
        None => return err("project not found"),
    };
    ok(read_github_file(&path, "triage-config.json"))
}

#[tauri::command]
pub async fn github_triage_save_config(
    project_id: String,
    config: Value,
) -> Result<IpcResult<Value>, ()> {
    let path = match project_path(&project_id) {
        Some(p) => p,
        None => return err("project not found"),
    };
    match write_github_file(&path, "triage-config.json", &config) {
        Ok(()) => ok(json!({ "saved": true })),
        Err(e) => err(&e),
    }
}

#[tauri::command]
pub async fn github_triage_get_results(project_id: String) -> Result<IpcResult<Value>, ()> {
    let path = match project_path(&project_id) {
        Some(p) => p,
        None => return err("project not found"),
    };
    ok(read_github_file(&path, "triage-results.json"))
}

#[tauri::command]
pub async fn github_create_release(
    project_id: String,
    version: String,
    release_notes: String,
    draft: Option<bool>,
) -> Result<IpcResult<Value>, ()> {
    let path = match project_path(&project_id) {
        Some(p) => p,
        None => return err("project not found"),
    };
    let mut args = vec![
        "release".to_string(),
        "create".to_string(),
        version.clone(),
        "--notes".to_string(),
        release_notes,
        "--title".to_string(),
        version.clone(),
    ];
    if draft.unwrap_or(false) {
        args.push("--draft".to_string());
    }
    let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    match gh_run(&args_ref, Some(&path)).await {
        Ok(_) => ok(json!({ "created": true, "version": version })),
        Err(e) => err(&e),
    }
}

// ── pr review (Python subprocess) ────────────────────────────────────────────

/// Resolve the Python interpreter for the given project path.
/// Checks: <project>/apps/backend/.venv, <project>/.venv, then PATH.
fn resolve_python_for_review(project_path: &Path) -> Option<std::path::PathBuf> {
    let bin = if cfg!(windows) { "Scripts" } else { "bin" };
    let exe = if cfg!(windows) { "python.exe" } else { "python" };
    for base in &[
        project_path.join("apps").join("backend").join(".venv"),
        project_path.join(".venv"),
    ] {
        let p = base.join(bin).join(exe);
        if p.exists() {
            return Some(p);
        }
    }
    for name in ["python3", "python"] {
        let ok = std::process::Command::new(name)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return Some(std::path::PathBuf::from(name));
        }
    }
    None
}

/// Locate the GitHub runner.py for a project.
/// Checks: <project>/apps/backend/runners/github/runner.py, then <project>/runners/...
fn find_runner(project_path: &Path) -> Option<std::path::PathBuf> {
    let candidates = [
        project_path.join("apps").join("backend").join("runners").join("github").join("runner.py"),
        project_path.join("runners").join("github").join("runner.py"),
    ];
    for p in &candidates {
        if p.exists() {
            return Some(p.clone());
        }
    }
    None
}

/// Spawns `python runner.py --project <path> review-pr <pr_number>`, streams
/// stdout/stderr as Tauri events, and reads the review result from disk on exit.
///
/// Events emitted (payload always includes `projectId`):
///   `github:pr:review:progress` — `{ projectId, phase, prNumber, progress, message }`
///   `github:pr:review:complete` — `{ projectId, ...PRReviewResult }`
///   `github:pr:review:error`    — `{ projectId, prNumber, error }`
#[tauri::command]
pub async fn github_pr_review(
    project_id: String,
    pr_number: u32,
    app: AppHandle,
    manager: tauri::State<'_, crate::api::agent::SharedAgentManager>,
) -> Result<IpcResult<Value>, ()> {
    use crate::agent::manager::RunningAgent;
    use tokio::io::BufReader;

    let proj_path = match project_path(&project_id) {
        Some(p) => p,
        None => return err("project not found"),
    };

    let runner = match find_runner(&proj_path) {
        Some(r) => r,
        None => return err("runner.py not found — is the backend installed?"),
    };

    let python = match resolve_python_for_review(&proj_path) {
        Some(p) => p,
        None => return err("python not found"),
    };

    let review_key = format!("pr-review:{}-{}", project_id, pr_number);

    // Obtain agents map without holding the manager lock across the spawn.
    let agents_arc = {
        let mgr = manager.lock().await;
        mgr.agents.clone()
    };
    {
        let map = agents_arc.lock().await;
        if map.contains_key(&review_key) {
            return Ok(IpcResult {
                success: false,
                data: None,
                error: Some("already_running".to_string()),
            });
        }
    }

    let mut child = match tokio::process::Command::new(&python)
        .arg(&runner)
        .args(["--project", proj_path.to_str().unwrap_or(""), "review-pr", &pr_number.to_string()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(&proj_path)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return err(&format!("spawn failed: {e}")),
    };

    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");

    let (kill_tx, kill_rx) = tokio::sync::oneshot::channel::<()>();

    {
        let mut map = agents_arc.lock().await;
        map.insert(
            review_key.clone(),
            RunningAgent {
                task_id: review_key.clone(),
                kill_tx,
                started_at: std::time::SystemTime::now(),
                current_profile_id: None,
                attempted_profile_ids: Vec::new(),
            },
        );
    }

    let pid_str = project_id.clone();
    let app_c = app.clone();
    let agents_arc_c = agents_arc.clone();
    let rk = review_key.clone();

    tokio::spawn(async move {
        use tokio::io::AsyncBufReadExt;


        let (tx, mut rx) = tokio::sync::mpsc::channel::<(String, String)>(128);
        let tx2 = tx.clone();

        let mut out_lines = BufReader::new(stdout).lines();
        let mut err_lines = BufReader::new(stderr).lines();

        tokio::spawn(async move {
            while let Ok(Some(l)) = out_lines.next_line().await {
                let _ = tx.send(("stdout".into(), l)).await;
            }
        });
        tokio::spawn(async move {
            while let Ok(Some(l)) = err_lines.next_line().await {
                let _ = tx2.send(("stderr".into(), l)).await;
            }
        });

        let mut kill_rx = kill_rx;

        loop {
            tokio::select! {
                msg = rx.recv() => {
                    match msg {
                        None => break,
                        Some((stream, line)) => {
                            // Emit raw output line
                            let _ = app_c.emit("github:pr:review:output", serde_json::json!({
                                "projectId": pid_str,
                                "prNumber": pr_number,
                                "stream": stream,
                                "data": line,
                            }));

                            // Parse progress pattern [  n%] message
                            if let Some((pct, msg)) = parse_progress_line(&line) {
                                let _ = app_c.emit("github:pr:review:progress", serde_json::json!({
                                    "projectId": pid_str,
                                    "phase": "analyzing",
                                    "prNumber": pr_number,
                                    "progress": pct,
                                    "message": msg,
                                }));
                            }
                        }
                    }
                }
                _ = &mut kill_rx => {
                    child.kill().await.ok();
                    break;
                }
            }
        }

        let exit = child.wait().await;
        agents_arc_c.lock().await.remove(&rk);

        let success = exit.map(|s| s.success()).unwrap_or(false);
        if success {
            // Read result from disk
            let review_file = proj_path
                .join(".auto-claude")
                .join("github")
                .join("pr")
                .join(format!("review_{}.json", pr_number));
            let result = std::fs::read_to_string(&review_file)
                .ok()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .unwrap_or(serde_json::Value::Null);

            let mut payload = match result {
                Value::Object(map) => map,
                _ => serde_json::Map::new(),
            };
            payload.insert("projectId".into(), serde_json::json!(pid_str));
            let _ = app_c.emit("github:pr:review:complete", Value::Object(payload));
        } else {
            let _ = app_c.emit("github:pr:review:error", serde_json::json!({
                "projectId": pid_str,
                "prNumber": pr_number,
                "error": "PR review process exited with error",
            }));
        }
    });

    Ok(IpcResult::ok(json!({ "started": true, "prNumber": pr_number })))
}

#[tauri::command]
pub async fn github_pr_review_cancel(
    project_id: String,
    pr_number: u32,
    manager: tauri::State<'_, crate::api::agent::SharedAgentManager>,
) -> Result<IpcResult<Value>, ()> {
    let review_key = format!("pr-review:{}-{}", project_id, pr_number);
    let agents_arc = {
        let mgr = manager.lock().await;
        mgr.agents.clone()
    };
    let mut agents = agents_arc.lock().await;
    if let Some(running) = agents.remove(&review_key) {
        let _ = running.kill_tx.send(());
        ok(json!({ "cancelled": true }))
    } else {
        ok(json!({ "cancelled": false, "reason": "not_running" }))
    }
}

#[tauri::command]
pub async fn github_pr_fix(
    _project_id: String,
    _pr_number: u32,
) -> Result<IpcResult<Value>, ()> {
    deferred("github_pr_fix:python-runner")
}

#[tauri::command]
pub async fn github_pr_followup_review(
    _project_id: String,
    _pr_number: u32,
) -> Result<IpcResult<Value>, ()> {
    deferred("github_pr_followup_review:python-runner")
}

#[tauri::command]
pub async fn github_investigate_issue(
    _project_id: String,
    _issue_number: u32,
) -> Result<IpcResult<Value>, ()> {
    deferred("github_investigate_issue:python-runner")
}

#[tauri::command]
pub async fn github_autofix_start(
    _project_id: String,
    _issue_number: u32,
) -> Result<IpcResult<Value>, ()> {
    deferred("github_autofix_start:python-runner")
}

#[tauri::command]
pub async fn github_autofix_stop(_project_id: String) -> Result<IpcResult<Value>, ()> {
    ok(json!({ "stopped": true }))
}

#[tauri::command]
pub async fn github_autofix_get_queue(_project_id: String) -> Result<IpcResult<Value>, ()> {
    ok(json!([]))
}

#[tauri::command]
pub async fn github_autofix_check_labels(project_id: String) -> Result<IpcResult<Value>, ()> {
    let r = match resolve_repo(&project_id).await {
        Ok(r) => r,
        Err(e) => return err(&e),
    };
    let path = format!("repos/{}/{}/labels", r.owner, r.repo);
    match gh_api(&path, "GET", None, Some(&r.path)).await {
        Ok(labels) => ok(json!({ "labels": labels })),
        Err(e) => err(&e),
    }
}

#[tauri::command]
pub async fn github_autofix_check_new(_project_id: String) -> Result<IpcResult<Value>, ()> {
    ok(json!([]))
}

#[tauri::command]
pub async fn github_autofix_batch(
    _project_id: String,
    _issue_numbers: Vec<u32>,
) -> Result<IpcResult<Value>, ()> {
    deferred("github_autofix_batch:python-runner")
}

#[tauri::command]
pub async fn github_autofix_get_batches(_project_id: String) -> Result<IpcResult<Value>, ()> {
    ok(json!([]))
}

#[tauri::command]
pub async fn github_autofix_analyze_preview(
    _project_id: String,
) -> Result<IpcResult<Value>, ()> {
    deferred("github_autofix_analyze_preview:python-runner")
}

#[tauri::command]
pub async fn github_autofix_approve_batches(
    _project_id: String,
    _batches: Value,
) -> Result<IpcResult<Value>, ()> {
    deferred("github_autofix_approve_batches:python-runner")
}

#[tauri::command]
pub async fn github_triage_run(
    _project_id: String,
    _issue_numbers: Vec<u32>,
) -> Result<IpcResult<Value>, ()> {
    deferred("github_triage_run:python-runner")
}

#[tauri::command]
pub async fn github_triage_apply_labels(
    _project_id: String,
    _results: Value,
) -> Result<IpcResult<Value>, ()> {
    deferred("github_triage_apply_labels:gh-api")
}

#[tauri::command]
pub async fn github_suggest_version(_project_id: String) -> Result<IpcResult<Value>, ()> {
    deferred("github_suggest_version:python-runner")
}

#[tauri::command]
pub async fn github_pr_status_poll_start(
    _project_id: String,
    _pr_numbers: Vec<u32>,
) -> Result<IpcResult<Value>, ()> {
    ok(json!({ "polling": false, "reason": "status-polling-not-ported" }))
}

#[tauri::command]
pub async fn github_pr_status_poll_stop(_project_id: String) -> Result<IpcResult<Value>, ()> {
    ok(json!({ "stopped": true }))
}

#[tauri::command]
pub async fn github_pr_memory_get(
    _project_id: String,
    _pr_number: u32,
) -> Result<IpcResult<Value>, ()> {
    ok(json!({ "memories": [] }))
}

#[tauri::command]
pub async fn github_pr_memory_search(
    _project_id: String,
    _query: String,
) -> Result<IpcResult<Value>, ()> {
    ok(json!({ "results": [] }))
}
