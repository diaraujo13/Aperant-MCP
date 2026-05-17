//! Inline code review commands (Tauri side).
//!
//! Counterpart of the renderer's `useReviewCommentsStore` and `InlineReview`
//! component. Exposes:
//!   - `task_review_file_patch`       — unified-diff for a single file
//!   - `task_review_comments_{list,add,delete,update}` — JSON CRUD on
//!     `.auto-claude/specs/{specId}/review_comments.json`
//!   - `task_finalize_review_triage`  — spawn the Python triage runner
//!   - `task_finalize_review_apply`   — apply (overridden) triage decisions:
//!         redo -> QA_FIX_REQUEST.md + flip status, follow_up -> new spec,
//!         wontfix -> mark resolved.

use crate::api::project;
use crate::error::AppError;
use crate::error::AppResult;
use crate::types::IpcResult;
use chrono::Utc;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use uuid::Uuid;

const SPECS_SUBDIR: &str = ".auto-claude/specs";
const WORKTREE_SUBDIR: &str = ".auto-claude/worktrees/tasks";
const COMMENTS_FILENAME: &str = "review_comments.json";
const PLAN_FILENAME: &str = "implementation_plan.json";
const QA_FIX_REQUEST_FILENAME: &str = "QA_FIX_REQUEST.md";
const MAX_PATCH_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewComment {
    pub id: String,
    pub file: String,
    pub line: u64,
    pub side: String, // "LEFT" | "RIGHT"
    pub body: String,
    #[serde(default = "default_status")]
    pub status: String, // "open" | "resolved" | "outdated"
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
}

fn default_status() -> String {
    "open".to_string()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewCommentInput {
    pub file: String,
    pub line: u64,
    pub side: String,
    pub body: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewCommentPatch {
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TriageDecision {
    pub comment_id: String,
    pub verdict: String, // "redo" | "follow_up" | "wontfix"
    #[serde(default)]
    pub rationale: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up_description: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FinalizeReviewResult {
    pub redo_count: u32,
    pub follow_up_count: u32,
    pub wontfix_count: u32,
    pub created_spec_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ReviewFilePatch {
    pub file: String,
    pub patch: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// Walk the project store and find the spec dir owning a given task id.
fn find_spec_dir(task_id: &str) -> Option<(PathBuf, PathBuf)> {
    let store_path = project::store_path().ok()?;
    let store = project::read_store_at(&store_path);
    for p in store.projects() {
        let proj_path = p.get("path").and_then(|v| v.as_str())?;
        let spec_dir = PathBuf::from(proj_path).join(SPECS_SUBDIR).join(task_id);
        if spec_dir.is_dir() {
            return Some((PathBuf::from(proj_path), spec_dir));
        }
    }
    None
}

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

/// Read comments WITHOUT a lock — read-only callers (list).
fn read_comments(spec_dir: &Path) -> Vec<ReviewComment> {
    let path = spec_dir.join(COMMENTS_FILENAME);
    let Ok(raw) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    parse_comments(&raw)
}

fn parse_comments(raw: &str) -> Vec<ReviewComment> {
    let Ok(parsed) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    parsed
        .get("comments")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| serde_json::from_value(item.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Atomic mutate: acquires an exclusive flock on a sidecar lock file, reads
/// fresh comments, calls `mutator`, writes via temp+rename. Other windows
/// block on the lock instead of racing the JSON.
///
/// Returns whatever the mutator returns. The mutator must not perform IO that
/// could deadlock with the lock (no spawning subprocesses that re-enter).
fn with_comments_lock<R>(
    spec_dir: &Path,
    mutator: impl FnOnce(&mut Vec<ReviewComment>) -> AppResult<R>,
) -> AppResult<R> {
    fs::create_dir_all(spec_dir)
        .map_err(|e| AppError::new("mkdir_failed", e.to_string()))?;
    let lock_path = spec_dir.join(format!("{COMMENTS_FILENAME}.lock"));
    let lock_file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|e| AppError::new("lock_open_failed", e.to_string()))?;
    lock_file
        .lock_exclusive()
        .map_err(|e| AppError::new("flock_failed", e.to_string()))?;

    let json_path = spec_dir.join(COMMENTS_FILENAME);
    let mut comments: Vec<ReviewComment> = match fs::read_to_string(&json_path) {
        Ok(raw) => parse_comments(&raw),
        Err(_) => Vec::new(),
    };

    let result = mutator(&mut comments)?;

    let payload = json!({ "version": 1, "comments": comments });
    let pretty = serde_json::to_string_pretty(&payload)
        .map_err(|e| AppError::new("serialize_failed", e.to_string()))?;
    let tmp_path = spec_dir.join(format!("{COMMENTS_FILENAME}.tmp"));
    fs::write(&tmp_path, pretty)
        .map_err(|e| AppError::new("write_tmp_failed", e.to_string()))?;
    fs::rename(&tmp_path, &json_path)
        .map_err(|e| AppError::new("rename_failed", e.to_string()))?;

    // FileExt::unlock_exclusive is unstable; dropping the file releases the lock.
    drop(lock_file);
    Ok(result)
}

fn detect_base_branch(project_path: &Path) -> String {
    for branch in &["main", "master"] {
        let ok = Command::new("git")
            .args(["rev-parse", "--verify", branch])
            .current_dir(project_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return (*branch).to_string();
        }
    }
    "main".to_string()
}

#[tauri::command(rename_all = "camelCase")]
pub async fn task_review_file_patch(
    task_id: String,
    file: String,
) -> AppResult<IpcResult<ReviewFilePatch>> {
    // Defense-in-depth: reject pathspec magic / git option injection / parent
    // traversal. `git diff -- <file>` accepts `:(glob)`, `:(top)`, etc., which
    // would let a compromised renderer expand into arbitrary repo paths. A `-`
    // prefix would be parsed as an option flag.
    if file.is_empty()
        || file.contains("..")
        || file.starts_with(':')
        || file.starts_with('-')
        || file.contains('\0')
    {
        return Ok(IpcResult {
            success: false,
            data: None,
            error: Some("invalid_file_path".to_string()),
        });
    }
    let result = tokio::task::spawn_blocking(move || -> AppResult<IpcResult<ReviewFilePatch>> {
        let Some((proj_path, _)) = find_spec_dir(&task_id) else {
            return Ok(IpcResult {
                success: false,
                data: None,
                error: Some("task_not_found".to_string()),
            });
        };
        let Some(wt) = find_worktree_path(&task_id) else {
            return Ok(IpcResult {
                success: false,
                data: None,
                error: Some("worktree_not_found".to_string()),
            });
        };
        let base = detect_base_branch(&proj_path);
        let range = format!("{}...HEAD", base);
        let output = Command::new("git")
            .args(["--no-pager", "diff", "--unified=3", &range, "--", &file])
            .current_dir(&wt)
            .output()
            .map_err(|e| AppError::new("git_diff_failed", e.to_string()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Ok(IpcResult {
                success: false,
                data: None,
                error: Some(format!(
                    "git diff failed (exit {}): {}",
                    output.status.code().unwrap_or(-1),
                    stderr.chars().take(200).collect::<String>()
                )),
            });
        }
        let mut patch = String::from_utf8_lossy(&output.stdout).to_string();
        let truncated = patch.len() > MAX_PATCH_BYTES;
        if truncated {
            patch.truncate(MAX_PATCH_BYTES);
        }
        Ok(IpcResult::ok(ReviewFilePatch {
            file,
            patch,
            truncated,
        }))
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))??;
    Ok(result)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn task_review_comments_list(
    task_id: String,
) -> AppResult<IpcResult<Vec<ReviewComment>>> {
    let res = tokio::task::spawn_blocking(move || -> IpcResult<Vec<ReviewComment>> {
        let Some((_, spec_dir)) = find_spec_dir(&task_id) else {
            return IpcResult {
                success: false,
                data: None,
                error: Some("task_not_found".to_string()),
            };
        };
        IpcResult::ok(read_comments(&spec_dir))
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))?;
    Ok(res)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn task_review_comments_add(
    task_id: String,
    input: ReviewCommentInput,
) -> AppResult<IpcResult<ReviewComment>> {
    if input.body.trim().is_empty() {
        return Ok(IpcResult {
            success: false,
            data: None,
            error: Some("empty_body".to_string()),
        });
    }
    let res = tokio::task::spawn_blocking(move || -> AppResult<IpcResult<ReviewComment>> {
        let Some((_, spec_dir)) = find_spec_dir(&task_id) else {
            return Ok(IpcResult {
                success: false,
                data: None,
                error: Some("task_not_found".to_string()),
            });
        };
        let comment = ReviewComment {
            id: Uuid::new_v4().to_string(),
            file: input.file,
            line: input.line,
            side: if input.side == "LEFT" { "LEFT".into() } else { "RIGHT".into() },
            body: input.body.trim().to_string(),
            status: "open".into(),
            created_at: Utc::now().to_rfc3339(),
            updated_at: None,
            author: Some("human".into()),
        };
        let comment_clone = comment.clone();
        with_comments_lock(&spec_dir, move |comments| {
            comments.push(comment_clone);
            Ok(())
        })?;
        Ok(IpcResult::ok(comment))
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))??;
    Ok(res)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn task_review_comments_delete(
    task_id: String,
    comment_id: String,
) -> AppResult<IpcResult<bool>> {
    let res = tokio::task::spawn_blocking(move || -> AppResult<IpcResult<bool>> {
        let Some((_, spec_dir)) = find_spec_dir(&task_id) else {
            return Ok(IpcResult {
                success: false,
                data: None,
                error: Some("task_not_found".to_string()),
            });
        };
        let removed = with_comments_lock(&spec_dir, |comments| {
            let before = comments.len();
            comments.retain(|c| c.id != comment_id);
            Ok(comments.len() < before)
        })?;
        if !removed {
            return Ok(IpcResult {
                success: false,
                data: None,
                error: Some("comment_not_found".to_string()),
            });
        }
        Ok(IpcResult::ok(true))
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))??;
    Ok(res)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn task_review_comments_update(
    task_id: String,
    comment_id: String,
    patch: ReviewCommentPatch,
) -> AppResult<IpcResult<ReviewComment>> {
    let res = tokio::task::spawn_blocking(move || -> AppResult<IpcResult<ReviewComment>> {
        let Some((_, spec_dir)) = find_spec_dir(&task_id) else {
            return Ok(IpcResult {
                success: false,
                data: None,
                error: Some("task_not_found".to_string()),
            });
        };
        let result = with_comments_lock(&spec_dir, move |comments| {
            let Some(c) = comments.iter_mut().find(|c| c.id == comment_id) else {
                return Ok(None);
            };
            if let Some(b) = patch.body {
                c.body = b;
            }
            if let Some(s) = patch.status {
                c.status = s;
            }
            c.updated_at = Some(Utc::now().to_rfc3339());
            Ok(Some(c.clone()))
        })?;
        match result {
            Some(updated) => Ok(IpcResult::ok(updated)),
            None => Ok(IpcResult {
                success: false,
                data: None,
                error: Some("comment_not_found".to_string()),
            }),
        }
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))??;
    Ok(res)
}

/// Spawn the Python triage runner. We pipe the payload through stdin and read
/// the JSON report from stdout. On any error the renderer falls back to a
/// heuristic so the user is never blocked.
fn run_triage_runner(payload: &Value) -> Result<Value, String> {
    // Resolve backend dir relative to the bundled app — same logic the renderer
    // assumes. In production we look one level up from the executable; in dev
    // we accept the CWD as a fallback.
    let backend_dir = std::env::current_dir()
        .ok()
        .and_then(|cwd| {
            // CWD when running `tauri dev` is apps/frontend.
            let candidate = cwd.join("..").join("backend");
            if candidate.exists() {
                Some(candidate)
            } else {
                None
            }
        })
        .or_else(|| {
            // Production: alongside the executable.
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|p| p.join("..").join("Resources").join("backend")))
                .filter(|p| p.exists())
        })
        .ok_or_else(|| "backend_dir_not_found".to_string())?;

    let runner = backend_dir.join("runners").join("review_triage_runner.py");
    if !runner.exists() {
        return Err(format!("runner_missing: {}", runner.display()));
    }

    let python = which_python().ok_or_else(|| "python_not_found".to_string())?;
    let mut child = Command::new(&python)
        .arg(&runner)
        .current_dir(&backend_dir)
        .env("PYTHONPATH", &backend_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn_failed: {e}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.to_string().as_bytes());
    }

    // Hard cap so a stuck Claude Agent SDK call doesn't pin a blocking thread
    // forever. 120s = generous for Haiku 4.5 with low thinking budget.
    let timeout = std::time::Duration::from_secs(120);
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("runner_timeout: exceeded {}s", timeout.as_secs()));
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(e) => return Err(format!("wait_failed: {e}")),
        }
    }

    let output = child
        .wait_with_output()
        .map_err(|e| format!("wait_with_output_failed: {e}"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!("runner_nonzero: {}", err.chars().take(500).collect::<String>()));
    }
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let start = stdout.find('{').ok_or_else(|| "no_json".to_string())?;
    let end = stdout.rfind('}').ok_or_else(|| "no_json".to_string())?;
    serde_json::from_str(&stdout[start..=end]).map_err(|e| format!("bad_json: {e}"))
}

pub(crate) fn which_python() -> Option<String> {
    // Windows ships `py` (the launcher) but often no `python3`/`python` on PATH.
    for candidate in ["python3", "python", "py"] {
        let ok = Command::new(candidate)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return Some(candidate.to_string());
        }
    }
    None
}

#[tauri::command(rename_all = "camelCase")]
pub async fn task_finalize_review_triage(task_id: String) -> AppResult<IpcResult<Value>> {
    let res = tokio::task::spawn_blocking(move || -> IpcResult<Value> {
        let Some((proj_path, spec_dir)) = find_spec_dir(&task_id) else {
            return IpcResult {
                success: false,
                data: None,
                error: Some("task_not_found".to_string()),
            };
        };
        let open: Vec<&ReviewComment> = vec![]; // placeholder for type inference
        let _ = open;
        let comments = read_comments(&spec_dir);
        let open: Vec<&ReviewComment> = comments.iter().filter(|c| c.status == "open").collect();
        if open.is_empty() {
            return IpcResult::ok(json!({
                "decisions": [],
                "summary": "No open comments to triage."
            }));
        }
        let payload = json!({
            "specId": task_id,
            "projectPath": proj_path.to_string_lossy(),
            "comments": open.iter().map(|c| json!({
                "id": c.id,
                "file": c.file,
                "line": c.line,
                "side": c.side,
                "body": c.body,
            })).collect::<Vec<_>>(),
        });
        match run_triage_runner(&payload) {
            Ok(report) => IpcResult::ok(report),
            Err(err) => {
                // Heuristic fallback: classify everything as redo so the user
                // is never blocked. The modal lets them override anyway.
                let decisions: Vec<Value> = open
                    .iter()
                    .map(|c| {
                        json!({
                            "commentId": c.id,
                            "verdict": "redo",
                            "rationale": format!("Classifier unavailable ({err}) — defaulted to redo."),
                        })
                    })
                    .collect();
                IpcResult::ok(json!({
                    "decisions": decisions,
                    "summary": "Heuristic fallback (classifier unavailable)."
                }))
            }
        }
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))?;
    Ok(res)
}

fn build_fix_request_markdown(comments: &[&ReviewComment]) -> String {
    let mut out = String::from(
        "# Human Review — Inline Feedback\n\nThe reviewer left the following comments on the diff. Address each one;\nthey are scoped to the original spec (no scope expansion).\n\n",
    );
    for c in comments {
        out.push_str(&format!("## {}:{}\n\n{}\n\n", c.file, c.line, c.body));
    }
    out
}

fn flip_plan_status(spec_dir: &Path, new_status: &str) -> AppResult<()> {
    let plan_path = spec_dir.join(PLAN_FILENAME);
    if !plan_path.exists() {
        return Ok(());
    }
    let raw =
        fs::read_to_string(&plan_path).map_err(|e| AppError::new("read_plan", e.to_string()))?;
    let mut parsed: Value =
        serde_json::from_str(&raw).map_err(|e| AppError::new("parse_plan", e.to_string()))?;
    if let Some(obj) = parsed.as_object_mut() {
        obj.insert("status".to_string(), json!(new_status));
        obj.insert("updated_at".to_string(), json!(Utc::now().to_rfc3339()));
    }
    let pretty = serde_json::to_string_pretty(&parsed)
        .map_err(|e| AppError::new("serialize_plan", e.to_string()))?;
    fs::write(&plan_path, pretty).map_err(|e| AppError::new("write_plan", e.to_string()))
}

fn next_spec_id(specs_root: &Path, slug_source: &str) -> String {
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
    let slug = if slug.is_empty() { "followup".to_string() } else { slug };
    let slug: String = slug.chars().take(40).collect();
    format!("{:03}-{}", next, slug)
}

fn create_follow_up_spec(
    project_path: &Path,
    parent_spec_id: &str,
    decision: &TriageDecision,
) -> Option<String> {
    let specs_root = project_path.join(SPECS_SUBDIR);
    fs::create_dir_all(&specs_root).ok()?;
    let title = decision.follow_up_title.as_deref().unwrap_or("followup");
    let spec_id = next_spec_id(&specs_root, title);
    let spec_dir = specs_root.join(&spec_id);
    fs::create_dir_all(&spec_dir).ok()?;
    let now = Utc::now().to_rfc3339();
    let plan = json!({
        "feature": title,
        "description": decision.follow_up_description.clone().unwrap_or_else(|| format!(
            "Follow-up surfaced during human review of {parent_spec_id}.\n\nRationale: {}",
            decision.rationale
        )),
        "created_at": now,
        "updated_at": now,
        "status": "pending",
        "phases": [],
        "parent_spec_id": parent_spec_id,
    });
    let _ = fs::write(
        spec_dir.join(PLAN_FILENAME),
        serde_json::to_string_pretty(&plan).ok()?,
    );
    let _ = fs::write(
        spec_dir.join("task_metadata.json"),
        serde_json::to_string_pretty(&json!({ "created_at": now })).ok()?,
    );
    Some(spec_id)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn task_finalize_review_apply(
    task_id: String,
    decisions: Vec<TriageDecision>,
) -> AppResult<IpcResult<FinalizeReviewResult>> {
    let res = tokio::task::spawn_blocking(move || -> AppResult<IpcResult<FinalizeReviewResult>> {
        let Some((proj_path, spec_dir)) = find_spec_dir(&task_id) else {
            return Ok(IpcResult {
                success: false,
                data: None,
                error: Some("task_not_found".to_string()),
            });
        };
        // Mutate under lock — compute redo/follow_ups/wontfix counters in one pass.
        let now = Utc::now().to_rfc3339();
        let now_clone = now.clone();
        let decisions_clone = decisions.clone();
        let (redo, follow_ups, wontfix) = with_comments_lock(
            &spec_dir,
            move |comments| -> AppResult<(Vec<ReviewComment>, Vec<TriageDecision>, u32)> {
                let mut redo: Vec<ReviewComment> = Vec::new();
                let mut follow_ups: Vec<TriageDecision> = Vec::new();
                let mut wontfix = 0u32;
                for decision in &decisions_clone {
                    let Some(c) = comments.iter_mut().find(|c| c.id == decision.comment_id)
                    else {
                        continue;
                    };
                    match decision.verdict.as_str() {
                        "redo" => {
                            redo.push(c.clone());
                            c.status = "resolved".into();
                            c.updated_at = Some(now_clone.clone());
                        }
                        "follow_up" => {
                            follow_ups.push(decision.clone());
                            c.status = "resolved".into();
                            c.updated_at = Some(now_clone.clone());
                        }
                        "wontfix" => {
                            wontfix += 1;
                            c.status = "resolved".into();
                            c.updated_at = Some(now_clone.clone());
                        }
                        _ => {}
                    }
                }
                Ok((redo, follow_ups, wontfix))
            },
        )?;
        let _ = now;

        // Side-effects OUTSIDE the lock so we don't hold it during fs/git work.
        if !redo.is_empty() {
            let md = build_fix_request_markdown(&redo.iter().collect::<Vec<_>>());
            fs::write(spec_dir.join(QA_FIX_REQUEST_FILENAME), &md)
                .map_err(|e| AppError::new("write_qa_fix", e.to_string()))?;
            if let Some(wt) = find_worktree_path(&task_id) {
                let wt_spec_dir = wt.join(".auto-claude").join("specs").join(&task_id);
                if wt_spec_dir.exists() {
                    let _ = fs::write(wt_spec_dir.join(QA_FIX_REQUEST_FILENAME), &md);
                }
            }
            flip_plan_status(&spec_dir, "in_progress")?;
        }

        let mut created_spec_ids = Vec::new();
        for fu in &follow_ups {
            if let Some(id) = create_follow_up_spec(&proj_path, &task_id, fu) {
                created_spec_ids.push(id);
            }
        }

        Ok(IpcResult::ok(FinalizeReviewResult {
            redo_count: redo.len() as u32,
            follow_up_count: created_spec_ids.len() as u32,
            wontfix_count: wontfix,
            created_spec_ids,
        }))
    })
    .await
    .map_err(|e| AppError::new("spawn_blocking_failed", e.to_string()))??;
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn next_spec_id_pads_and_increments() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("003-foo")).unwrap();
        fs::create_dir_all(dir.path().join("007-bar")).unwrap();
        let id = next_spec_id(dir.path(), "Add caching layer");
        assert!(id.starts_with("008-"));
        assert!(id.contains("add-caching-layer"));
    }

    #[test]
    fn next_spec_id_handles_empty_dir() {
        let dir = tempdir().unwrap();
        let id = next_spec_id(dir.path(), "");
        assert!(id.starts_with("001-"));
    }

    #[test]
    fn read_comments_handles_missing_file() {
        let dir = tempdir().unwrap();
        assert!(read_comments(dir.path()).is_empty());
    }

    #[test]
    fn write_then_read_roundtrip() {
        let dir = tempdir().unwrap();
        let c = ReviewComment {
            id: "abc".into(),
            file: "src/x.ts".into(),
            line: 42,
            side: "RIGHT".into(),
            body: "fix this".into(),
            status: "open".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: None,
            author: Some("human".into()),
        };
        let c_clone = c.clone();
        with_comments_lock(dir.path(), move |list| {
            list.push(c_clone);
            Ok(())
        })
        .unwrap();
        let read = read_comments(dir.path());
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].id, "abc");
    }

    #[test]
    fn with_comments_lock_serializes_writes() {
        // Hammer the lock from N threads — the final count must equal N. If the
        // lock weren't real, last-write-wins would clobber concurrent appends.
        use std::sync::Arc;
        let dir = Arc::new(tempdir().unwrap());
        let mut handles = Vec::new();
        for i in 0..8 {
            let dir = Arc::clone(&dir);
            handles.push(std::thread::spawn(move || {
                let c = ReviewComment {
                    id: format!("c{}", i),
                    file: "x.ts".into(),
                    line: i as u64,
                    side: "RIGHT".into(),
                    body: format!("body {}", i),
                    status: "open".into(),
                    created_at: "x".into(),
                    updated_at: None,
                    author: None,
                };
                with_comments_lock(dir.path(), move |list| {
                    list.push(c);
                    Ok(())
                })
                .unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(read_comments(dir.path()).len(), 8);
    }

    #[test]
    fn fix_request_markdown_lists_all_comments() {
        let c1 = ReviewComment {
            id: "1".into(),
            file: "a.ts".into(),
            line: 1,
            side: "RIGHT".into(),
            body: "first".into(),
            status: "open".into(),
            created_at: "x".into(),
            updated_at: None,
            author: None,
        };
        let c2 = ReviewComment { id: "2".into(), file: "b.ts".into(), line: 9, ..c1.clone() };
        let md = build_fix_request_markdown(&[&c1, &c2]);
        assert!(md.contains("a.ts:1"));
        assert!(md.contains("b.ts:9"));
        assert!(md.contains("first"));
    }
}
