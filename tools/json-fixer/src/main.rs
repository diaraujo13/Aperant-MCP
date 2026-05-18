use anyhow::{Context, Result};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;
use tokio::process::Command;
use tokio::sync::{Mutex, Semaphore};
use tracing::{error, info, warn};
use walkdir::WalkDir;

#[derive(Parser, Debug)]
#[command(name = "json-fixer", version, about = "Repair corrupted implementation_plan.json files")]
struct Cli {
    #[arg(short, long)]
    project: PathBuf,

    #[arg(long)]
    dry_run: bool,

    #[arg(long)]
    include_worktrees: bool,

    #[arg(long, help = "Optional command to run after each fix; '{}' is replaced with the file path")]
    verify_cmd: Option<String>,

    #[arg(long, default_value_t = 4)]
    parallel: usize,
}

#[derive(Serialize, Deserialize, Debug)]
struct PlanTemplate {
    feature: String,
    description: String,
    created_at: String,
    updated_at: String,
    status: String,
    phases: Vec<serde_json::Value>,
}

impl PlanTemplate {
    fn recovery() -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            feature: "Auto-recovery task".to_string(),
            description: "Task recovered by json-fixer".to_string(),
            created_at: now.clone(),
            updated_at: now,
            status: "start_requested".to_string(),
            phases: vec![],
        }
    }
}

#[derive(Default, Debug)]
struct Stats {
    scanned: usize,
    healthy: usize,
    fixed: usize,
    failed: usize,
    skipped_dry_run: usize,
}

enum Verdict {
    Healthy,
    NeedsFix(String),
}

async fn inspect(path: &Path) -> Result<Verdict> {
    let bytes = fs::read(path)
        .await
        .with_context(|| format!("read {}", path.display()))?;

    if bytes.is_empty() {
        return Ok(Verdict::NeedsFix("empty file".into()));
    }

    match serde_json::from_slice::<serde_json::Value>(&bytes) {
        Ok(v) => {
            if v.get("feature").is_none() || v.get("status").is_none() {
                Ok(Verdict::NeedsFix("missing required fields".into()))
            } else {
                Ok(Verdict::Healthy)
            }
        }
        Err(e) => Ok(Verdict::NeedsFix(format!("parse error: {e}"))),
    }
}

async fn fix_one(path: &Path, dry_run: bool) -> Result<bool> {
    if dry_run {
        return Ok(false);
    }
    let template = PlanTemplate::recovery();
    let serialized = serde_json::to_string_pretty(&template)?;
    fs::write(path, serialized)
        .await
        .with_context(|| format!("write {}", path.display()))?;
    Ok(true)
}

async fn run_verify(cmd_template: &str, path: &Path) -> Result<bool> {
    let cmd_str = cmd_template.replace("{}", &path.display().to_string());
    let parts: Vec<&str> = cmd_str.split_whitespace().collect();
    if parts.is_empty() {
        anyhow::bail!("empty verify_cmd");
    }

    let output = Command::new(parts[0])
        .args(&parts[1..])
        .output()
        .await
        .with_context(|| format!("spawn verify_cmd: {cmd_str}"))?;

    if !output.status.success() {
        warn!(
            path = %path.display(),
            stderr = %String::from_utf8_lossy(&output.stderr).trim(),
            "verify_cmd failed"
        );
        return Ok(false);
    }
    Ok(true)
}

fn find_candidates(project: &Path, include_worktrees: bool) -> Vec<PathBuf> {
    let mut roots = vec![project.join(".auto-claude").join("specs")];

    if include_worktrees {
        let worktrees = project.join(".auto-claude").join("worktrees").join("tasks");
        if worktrees.is_dir() {
            for entry in WalkDir::new(&worktrees).max_depth(1).into_iter().flatten() {
                let p = entry.path();
                if p.is_dir() && p != worktrees {
                    roots.push(p.join(".auto-claude").join("specs"));
                }
            }
        }
    }

    let mut out = Vec::new();
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        for entry in WalkDir::new(&root).into_iter().flatten() {
            if entry.file_name() == "implementation_plan.json" {
                out.push(entry.path().to_path_buf());
            }
        }
    }
    out
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "json_fixer=info".into()),
        )
        .init();

    let cli = Cli::parse();

    if !cli.project.is_dir() {
        anyhow::bail!("project does not exist or is not a directory: {}", cli.project.display());
    }

    let candidates = find_candidates(&cli.project, cli.include_worktrees);
    info!(count = candidates.len(), "candidates found");

    let stats = Arc::new(Mutex::new(Stats::default()));
    let semaphore = Arc::new(Semaphore::new(cli.parallel));
    let verify_cmd = Arc::new(cli.verify_cmd);

    let mut handles = Vec::with_capacity(candidates.len());
    for path in candidates {
        let stats = stats.clone();
        let semaphore = semaphore.clone();
        let verify_cmd = verify_cmd.clone();
        let dry_run = cli.dry_run;

        let handle = tokio::spawn(async move {
            let _permit = semaphore.acquire_owned().await.expect("semaphore closed");

            stats.lock().await.scanned += 1;

            let verdict = match inspect(&path).await {
                Ok(v) => v,
                Err(e) => {
                    error!(path = %path.display(), err = %e, "inspect failed");
                    stats.lock().await.failed += 1;
                    return;
                }
            };

            match verdict {
                Verdict::Healthy => {
                    stats.lock().await.healthy += 1;
                }
                Verdict::NeedsFix(reason) => {
                    info!(path = %path.display(), %reason, "fixing");
                    match fix_one(&path, dry_run).await {
                        Ok(true) => {
                            stats.lock().await.fixed += 1;
                            if let Some(cmd) = verify_cmd.as_ref() {
                                let _ = run_verify(cmd, &path).await;
                            }
                        }
                        Ok(false) => {
                            stats.lock().await.skipped_dry_run += 1;
                        }
                        Err(e) => {
                            error!(path = %path.display(), err = %e, "fix failed");
                            stats.lock().await.failed += 1;
                        }
                    }
                }
            }
        });
        handles.push(handle);
    }

    for h in handles {
        let _ = h.await;
    }

    let s = stats.lock().await;
    println!();
    println!("Summary:");
    println!("  scanned:        {}", s.scanned);
    println!("  healthy:        {}", s.healthy);
    println!("  fixed:          {}", s.fixed);
    println!("  failed:         {}", s.failed);
    println!("  dry-run skips:  {}", s.skipped_dry_run);

    if s.failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(dir: &TempDir, name: &str, contents: &[u8]) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[tokio::test]
    async fn inspect_empty_is_needs_fix() {
        let tmp = TempDir::new().unwrap();
        let p = write(&tmp, "plan.json", b"");
        match inspect(&p).await.unwrap() {
            Verdict::NeedsFix(r) => assert!(r.contains("empty")),
            Verdict::Healthy => panic!("empty file should not be healthy"),
        }
    }

    #[tokio::test]
    async fn inspect_malformed_is_needs_fix() {
        let tmp = TempDir::new().unwrap();
        let p = write(&tmp, "plan.json", b"{ not valid json");
        assert!(matches!(inspect(&p).await.unwrap(), Verdict::NeedsFix(_)));
    }

    #[tokio::test]
    async fn inspect_missing_fields_is_needs_fix() {
        let tmp = TempDir::new().unwrap();
        let p = write(&tmp, "plan.json", br#"{"description": "x"}"#);
        assert!(matches!(inspect(&p).await.unwrap(), Verdict::NeedsFix(_)));
    }

    #[tokio::test]
    async fn inspect_complete_is_healthy() {
        let tmp = TempDir::new().unwrap();
        let p = write(
            &tmp,
            "plan.json",
            br#"{"feature": "x", "status": "pending"}"#,
        );
        assert!(matches!(inspect(&p).await.unwrap(), Verdict::Healthy));
    }

    #[tokio::test]
    async fn fix_one_writes_template() {
        let tmp = TempDir::new().unwrap();
        let p = write(&tmp, "plan.json", b"");
        assert!(fix_one(&p, false).await.unwrap());
        let parsed: PlanTemplate = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        assert_eq!(parsed.status, "start_requested");
    }

    #[tokio::test]
    async fn fix_one_dry_run_does_not_write() {
        let tmp = TempDir::new().unwrap();
        let p = write(&tmp, "plan.json", b"original");
        assert!(!fix_one(&p, true).await.unwrap());
        assert_eq!(std::fs::read(&p).unwrap(), b"original");
    }
}
