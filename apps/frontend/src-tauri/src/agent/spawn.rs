//! Reusable Python-spawn helper with profile env injection. Used by one-shot
//! runners (PR review, triage, ideation, etc.) that need profile-aware
//! credentials but do NOT need the rate-limit auto-switch loop in
//! `api::agent::do_spawn` (which is reserved for long-running task agents).

use crate::agent::profile_env::{resolve_profile_env, ResolvedProfile};
use std::path::Path;
use std::process::Stdio;

pub struct PythonSpawn {
    pub child: tokio::process::Child,
    pub profile_id: Option<String>,
}

#[derive(Debug)]
pub enum SpawnError {
    PythonNotFound,
    NoProfilesAvailable,
    SpawnFailed(String),
}

/// Builds and spawns `<python> <script> <args...>` with the active profile's
/// env vars injected. Returns the live Child plus the id of the profile used.
///
/// Mirrors `do_spawn`'s python resolution and env-application policy but is
/// fire-and-forget — caller owns the Child and decides how to read its output
/// and react to exit. No rate-limit detection, no respawn.
///
/// `extra_env` lets callers add their own env vars (applied AFTER profile env,
/// so they win on collision). `current_dir` is optional.
pub async fn spawn_python_with_profile(
    project_path: &Path,
    script: &Path,
    args: &[String],
    extra_env: &[(String, String)],
    current_dir: Option<&Path>,
) -> Result<PythonSpawn, SpawnError> {
    let python = crate::agent::resolve_python(project_path).ok_or(SpawnError::PythonNotFound)?;

    let resolved: Option<ResolvedProfile> = resolve_profile_env(&[]);
    if resolved.is_none() && crate::agent::any_profiles_configured() {
        return Err(SpawnError::NoProfilesAvailable);
    }

    let mut cmd = tokio::process::Command::new(&python);
    cmd.arg(script)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(false);

    if let Some(dir) = current_dir {
        cmd.current_dir(dir);
    }

    if let Some(rp) = &resolved {
        crate::agent::apply_profile_env(&mut cmd, rp);
    }

    for (k, v) in extra_env {
        cmd.env(k, v);
    }

    let child = cmd
        .spawn()
        .map_err(|e| SpawnError::SpawnFailed(e.to_string()))?;
    Ok(PythonSpawn {
        child,
        profile_id: resolved.map(|r| r.profile_id),
    })
}

#[cfg(test)]
mod tests {
    //! Note: a unit test that drives `resolve_profile_env` against a mocked
    //! profile source would require refactoring that function to accept a
    //! source param. Skipped here — `spawn_python_with_profile` is a thin
    //! composition of `resolve_python`, `resolve_profile_env`, and
    //! `apply_profile_env`, all already covered by their own tests.
    use super::*;
    use std::path::PathBuf;
    use std::process::Stdio;

    fn python_available() -> Option<&'static str> {
        for name in ["python3", "python"] {
            let ok = std::process::Command::new(name)
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if ok {
                return Some(name);
            }
        }
        None
    }

    #[tokio::test]
    async fn spawns_one_shot_python_and_captures_stdout() {
        if python_available().is_none() {
            eprintln!("skipping: no python available");
            return;
        }

        // Use `-c` form via a tiny shim: the helper requires a script path,
        // so write a temp script that prints "ok".
        let tmp_dir = std::env::temp_dir();
        let script = tmp_dir.join(format!("aperant_spawn_test_{}.py", std::process::id()));
        std::fs::write(&script, "print('ok')").expect("write temp script");

        let project_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let result = spawn_python_with_profile(&project_path, &script, &[], &[], None).await;

        let spawn = match result {
            Ok(s) => s,
            Err(SpawnError::NoProfilesAvailable) => {
                // Dev machine has profiles configured but none resolvable for
                // this test context — accept as a skip rather than a failure.
                eprintln!("skipping: no profiles available in test env");
                return;
            }
            Err(e) => panic!("spawn failed: {e:?}"),
        };

        let output = spawn.child.wait_with_output().await.expect("wait");
        let _ = std::fs::remove_file(&script);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        assert!(
            output.status.success(),
            "process should exit 0, stdout={stdout:?}, stderr={stderr:?}"
        );
        assert!(
            stdout.contains("ok"),
            "stdout should contain 'ok', got: {stdout:?}"
        );
    }
}
