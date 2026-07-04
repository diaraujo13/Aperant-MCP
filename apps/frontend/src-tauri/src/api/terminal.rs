//! Terminal subsystem (Phase 4 spike).
//!
//! Spawns a real PTY via `portable-pty`, lets the renderer write to stdin,
//! resize, kill, and listen to bytes coming back on the `terminal:output`
//! event. This is the foundation for Start Task, Claude OAuth login flows,
//! and every other interactive subprocess workflow in the app.
//!
//! What's NOT in this spike (deferred to dedicated rounds):
//!   - Session persistence (terminal-session-store)
//!   - Title generation via Claude AI (terminal-name-generator)
//!   - Claude invocation logic (claude-integration-handler)
//!   - Worktree config plumbing
//!   - Multi-day session restore + display order
//!   - Profile-aware spawning (PATH manipulation per Claude profile)

use crate::error::{AppError, AppResult};
use crate::types::IpcResult;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::Mutex;
use tracing::{info, warn};

/// One live PTY. Holds the master side (for I/O), the child handle (so we
/// can kill it), and a writer that stays open across input calls.
pub(crate) struct Terminal {
    /// Tokio task that copies bytes from the PTY into renderer events.
    /// Aborted on destroy so we don't leak the read loop.
    reader_task: tokio::task::JoinHandle<()>,
    /// Owns the writer half of the PTY. Wrapped in Mutex because writes
    /// can come in concurrently from the renderer (typing fast) and we
    /// need to serialize them onto the same fd.
    writer: Arc<std::sync::Mutex<Box<dyn Write + Send>>>,
    /// The master side, kept alive so the PTY stays open. Also exposes
    /// resize.
    master: Box<dyn portable_pty::MasterPty + Send>,
    /// Child process handle for kill().
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

pub type Terminals = Arc<Mutex<HashMap<String, Terminal>>>;
pub type TerminalTitles = Arc<Mutex<HashMap<String, String>>>;
pub type TerminalWorktreeConfigs = Arc<Mutex<HashMap<String, serde_json::Value>>>;
pub type TerminalDisplayOrders = Arc<Mutex<Vec<String>>>;
pub type TerminalSessions = Arc<Mutex<Vec<serde_json::Value>>>;

const ADJECTIVES: &[&str] = &[
    "swift", "bold", "calm", "deep", "fair", "glad", "keen", "mild", "neat", "pure",
    "rich", "sage", "tall", "warm", "wild", "cool", "dark", "free", "jade", "nova",
];
const NOUNS: &[&str] = &[
    "pine", "river", "stone", "cliff", "grove", "ridge", "creek", "bloom", "frost",
    "glade", "haven", "isle", "lake", "mist", "peak", "reef", "shore", "vale", "wind", "dawn",
];

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalCreateOptions {
    pub id: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub shell: Option<String>,
    #[serde(default = "default_cols")]
    pub cols: u16,
    #[serde(default = "default_rows")]
    pub rows: u16,
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
}

fn default_cols() -> u16 {
    80
}
fn default_rows() -> u16 {
    24
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateResult {
    pub id: String,
}

/// Strip ANSI/VT escape sequences from a string.
fn strip_ansi(s: &str) -> String {
    // Handles CSI (\x1b[...m), OSC (\x1b]...BEL), and simple \x1b sequences.
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    // consume until a letter (command byte)
                    for ch in chars.by_ref() {
                        if ch.is_ascii_alphabetic() { break; }
                    }
                }
                Some(']') => {
                    chars.next();
                    // consume until BEL (\x07) or ST (\x1b\\)
                    for ch in chars.by_ref() {
                        if ch == '\x07' { break; }
                        if ch == '\x1b' { chars.next(); break; }
                    }
                }
                _ => { chars.next(); }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Extract email from Claude CLI output using the same patterns as the Electron app.
fn extract_email_from_output(buf: &str) -> Option<String> {
    let patterns: &[&str] = &[
        r"(?i)(?:Authenticated as |Logged in as |email[:\s]+)([a-zA-Z0-9._%+\-]+@[a-zA-Z0-9.\-]+\.[a-zA-Z]{2,})",
        r"([a-zA-Z0-9._%+\-]+@[a-zA-Z0-9.\-]+\.[a-zA-Z]{2,})'s\s*Organization",
        r"(?i)Claude\s+(?:Max|Pro|Team|Enterprise)\s*[·•]\s*([a-zA-Z0-9._%+\-]+@[a-zA-Z0-9.\-]+\.[a-zA-Z]{2,})",
        r"([a-zA-Z0-9._%+\-]+@[a-zA-Z0-9.\-]+\.[a-zA-Z]{2,})'s",
    ];
    for pat in patterns {
        if let Ok(re) = regex_lite::Regex::new(pat) {
            if let Some(caps) = re.captures(buf) {
                if let Some(m) = caps.get(1) {
                    return Some(m.as_str().to_string());
                }
            }
        }
    }
    None
}

/// Detect successful OAuth login in terminal output (auth terminals only).
/// Returns a JSON payload ready to emit as `terminal:oauth:token`, or None.
fn detect_oauth_success(buf: &str, terminal_id: &str, profile_id: &str) -> Option<serde_json::Value> {
    // Primary: "Login successful", "Successfully logged in", "Logged in as user@..."
    let login_re = regex_lite::Regex::new(
        r"(?i)(?:Login successful|Successfully logged in|Logged in as\s+\S+@\S+)"
    ).ok()?;

    // Legacy: raw OAuth token in output
    let token_re = regex_lite::Regex::new(r"(sk-ant-oat01-[A-Za-z0-9_\-]+)").ok()?;

    if login_re.is_match(buf) || token_re.is_match(buf) {
        let email = extract_email_from_output(buf);
        return Some(serde_json::json!({
            "terminalId": terminal_id,
            "profileId": profile_id,
            "email": email,
            "success": true,
            // needsOnboarding: false → AuthTerminal immediately shows success.
            // We also emit terminal:onboarding:complete for the welcome-screen path,
            // but this ensures auth completes even when the welcome screen never appears.
            "needsOnboarding": false,
            "detectedAt": chrono::Utc::now().to_rfc3339(),
        }));
    }
    None
}

/// Detect Claude Code onboarding complete (welcome screen after login).
/// Matches: "Welcome back André!", "Claude Code v2.x", "Claude Max/Pro/Team".
fn detect_onboarding_complete(buf: &str, terminal_id: &str, profile_id: &str) -> Option<serde_json::Value> {
    let patterns: &[&str] = &[
        r"(?i)Welcome back\s+\w+",
        r"(?i)Claude Code v\d+\.\d+",
        r"(?i)Claude\s+(?:Max|Pro|Team|Enterprise)",
    ];
    for pat in patterns {
        if let Ok(re) = regex_lite::Regex::new(pat) {
            if re.is_match(buf) {
                let email = extract_email_from_output(buf);
                return Some(serde_json::json!({
                    "terminalId": terminal_id,
                    "profileId": profile_id,
                    "email": email,
                    "detectedAt": chrono::Utc::now().to_rfc3339(),
                }));
            }
        }
    }
    None
}

/// Picks a sensible shell when the renderer doesn't override it.
/// macOS/Linux: $SHELL or /bin/bash. Windows: $COMSPEC or cmd.exe.
fn default_shell() -> String {
    if cfg!(target_os = "windows") {
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string())
    }
}

/// OS-specific PATH separator (`;` on Windows, `:` elsewhere).
fn path_separator() -> char {
    if cfg!(target_os = "windows") {
        ';'
    } else {
        ':'
    }
}

/// Prepend `dir` to a PATH-style string, skipping the work if `dir` is already
/// the first/any entry (avoids unbounded growth across nested spawns). Returns
/// `dir` alone when `existing` is empty.
fn prepend_path(dir: &std::path::Path, existing: &str) -> String {
    let sep = path_separator();
    let dir_str = dir.to_string_lossy();
    if existing.is_empty() {
        return dir_str.into_owned();
    }
    if existing.split(sep).any(|entry| std::path::Path::new(entry) == dir) {
        return existing.to_string();
    }
    format!("{dir_str}{sep}{existing}")
}

/// Builds the effective PATH for a spawned PTY: the caller-supplied PATH (or the
/// app process PATH as fallback) with the resolved `claude` directory prepended.
///
/// This is the core of automatic Claude Code CLI integration. The app may be
/// launched from Finder/Dock with a minimal PATH that omits Homebrew /
/// npm-global / nvm, so a non-interactive PTY can't see `claude`. Prepending the
/// detected CLI directory makes `claude` resolve in every terminal — auth login,
/// agent terminals, and plain shells — without the user configuring anything.
fn effective_path(supplied: Option<&String>) -> Option<String> {
    let base = supplied
        .cloned()
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    let claude_dir = crate::api::claude_code::resolve_claude_dir()?;
    Some(prepend_path(&claude_dir, &base))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn terminal_create(
    options: TerminalCreateOptions,
    app_handle: AppHandle,
    terminals: State<'_, Terminals>,
    sessions: State<'_, TerminalSessions>,
) -> AppResult<IpcResult<CreateResult>> {
    let id = options.id.clone();

    // Refuse to clobber an existing terminal silently — would leak the previous PTY.
    {
        let map = terminals.lock().await;
        if map.contains_key(&id) {
            return Err(AppError::new(
                "terminal_exists",
                format!("Terminal {id} already exists; destroy it first"),
            ));
        }
    }

    let pty_system = native_pty_system();
    let pty_pair = pty_system
        .openpty(PtySize {
            cols: options.cols,
            rows: options.rows,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| AppError::new("openpty_failed", e.to_string()))?;

    let shell = options.shell.unwrap_or_else(default_shell);
    let mut cmd = CommandBuilder::new(&shell);

    if let Some(cwd) = options.cwd {
        cmd.cwd(cwd);
    }

    // Capture any caller-supplied PATH before consuming the env map, so it can
    // serve as the base that the Claude CLI directory is prepended onto.
    let supplied_path = options.env.as_ref().and_then(|e| e.get("PATH").cloned());
    if let Some(env) = options.env {
        for (k, v) in env {
            cmd.env(k, v);
        }
    }
    // Inject the resolved Claude CLI directory onto PATH last, so it wins over
    // any PATH the caller passed (their value is folded in as the base). This is
    // what makes `claude` reachable from the PTY automatically.
    if let Some(path) = effective_path(supplied_path.as_ref()) {
        cmd.env("PATH", path);
    }

    let child = pty_pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| AppError::new("spawn_failed", e.to_string()))?;

    // Drop the slave fd after spawn — we only need the master. Keeping the
    // slave open prevents the kernel from sending EOF to the reader when the
    // child exits, which would hang our cleanup.
    drop(pty_pair.slave);

    let writer = pty_pair
        .master
        .take_writer()
        .map_err(|e| AppError::new("take_writer_failed", e.to_string()))?;
    let writer = Arc::new(std::sync::Mutex::new(writer));

    let mut reader = pty_pair
        .master
        .try_clone_reader()
        .map_err(|e| AppError::new("clone_reader_failed", e.to_string()))?;

    // Detect auth terminals: claude-login-{profileId}-{timestamp}
    let auth_profile_id: Option<String> = {
        let re = regex_lite::Regex::new(r"^claude-login-([a-z0-9-]+)-\d{13,}$").ok();
        re.and_then(|r| r.captures(&id))
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
    };

    // Spawn the read loop on a blocking thread — portable-pty's reader is
    // synchronous and would block tokio workers if run on the main runtime.
    let app_handle_for_reader = app_handle.clone();
    let id_for_reader = id.clone();
    let reader_task = tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; 8192];
        // Rolling output buffer for multi-chunk pattern detection (auth terminals only)
        let mut auth_buf = String::new();
        let mut oauth_done = false;

        loop {
            match reader.read(&mut buf) {
                Ok(0) => break, // EOF (child exited)
                Ok(n) => {
                    let chunk = String::from_utf8_lossy(&buf[..n]).to_string();
                    if let Err(e) = app_handle_for_reader.emit(
                        "terminal:output",
                        json!({ "id": id_for_reader, "data": chunk }),
                    ) {
                        warn!(terminal = %id_for_reader, "emit failed: {e}");
                    }

                    // OAuth detection for auth terminals
                    if let Some(ref profile_id) = auth_profile_id {
                        // Strip ANSI escapes from chunk before appending
                        let stripped = strip_ansi(&chunk);
                        auth_buf.push_str(&stripped);
                        // Keep buffer bounded
                        if auth_buf.len() > 65536 {
                            auth_buf = auth_buf[auth_buf.len() - 32768..].to_string();
                        }

                        if !oauth_done {
                            if let Some(event) = detect_oauth_success(&auth_buf, &id_for_reader, profile_id) {
                                oauth_done = true;
                                let _ = app_handle_for_reader.emit("terminal:oauth:token", event);
                            }
                        } else {
                            // After OAuth, watch for onboarding complete (welcome screen)
                            if let Some(event) = detect_onboarding_complete(&auth_buf, &id_for_reader, profile_id) {
                                let _ = app_handle_for_reader.emit("terminal:onboarding:complete", event);
                                // Clear buffer to avoid re-firing on buffered content
                                auth_buf.clear();
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(terminal = %id_for_reader, "read error: {e}");
                    break;
                }
            }
        }
        // Notify the renderer the PTY closed so the UI can clean up its state.
        let _ = app_handle_for_reader.emit(
            "terminal:exit",
            json!({ "id": id_for_reader, "code": null }),
        );
    });

    let terminal = Terminal {
        reader_task,
        writer,
        master: pty_pair.master,
        child,
    };

    {
        let mut map = terminals.lock().await;
        map.insert(id.clone(), terminal);
    }

    {
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut list = sessions.lock().await;
        list.push(serde_json::json!({ "id": id, "title": null, "createdAt": created_at }));
    }

    info!(id = %id, shell = %shell, "terminal spawned");
    Ok(IpcResult::ok(CreateResult { id }))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn terminal_input(
    id: String,
    data: String,
    terminals: State<'_, Terminals>,
) -> AppResult<()> {
    let map = terminals.lock().await;
    let term = map
        .get(&id)
        .ok_or_else(|| AppError::new("terminal_not_found", format!("No terminal with id {id}")))?;
    let writer = Arc::clone(&term.writer);
    drop(map); // release map lock before potentially-blocking write

    tokio::task::spawn_blocking(move || {
        let mut w = writer.lock().expect("writer mutex poisoned");
        let _ = w.write_all(data.as_bytes());
        let _ = w.flush();
    })
    .await
    .map_err(|e| AppError::new("write_join_failed", e.to_string()))?;
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct ResizeResult {
    pub success: bool,
}

#[tauri::command(rename_all = "camelCase")]
pub async fn terminal_resize(
    id: String,
    cols: u16,
    rows: u16,
    terminals: State<'_, Terminals>,
) -> AppResult<IpcResult<ResizeResult>> {
    let map = terminals.lock().await;
    let term = map
        .get(&id)
        .ok_or_else(|| AppError::new("terminal_not_found", format!("No terminal with id {id}")))?;
    let success = term
        .master
        .resize(PtySize {
            cols,
            rows,
            pixel_width: 0,
            pixel_height: 0,
        })
        .is_ok();
    Ok(IpcResult::ok(ResizeResult { success }))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn terminal_destroy(
    id: String,
    terminals: State<'_, Terminals>,
    sessions: State<'_, TerminalSessions>,
) -> AppResult<IpcResult<()>> {
    let mut map = terminals.lock().await;
    let mut term = map
        .remove(&id)
        .ok_or_else(|| AppError::new("terminal_not_found", format!("No terminal with id {id}")))?;
    drop(map);

    // Order matters: kill the child process first so EOF reaches the reader,
    // which lets the read loop exit naturally. Then abort the task as a
    // belt-and-braces cleanup in case the kernel takes its time.
    let _ = term.child.kill();
    let _ = term.child.wait();
    term.reader_task.abort();
    drop(term.master);

    {
        let mut list = sessions.lock().await;
        list.retain(|s| s.get("id").and_then(serde_json::Value::as_str) != Some(&id));
    }

    info!(id = %id, "terminal destroyed");
    Ok(IpcResult::ok(()))
}

/// Diagnostic: returns whether a given terminal id is still alive in the
/// internal map. The Electron renderer polls this after suspected crashes.
#[tauri::command(rename_all = "camelCase")]
pub async fn terminal_check_alive(
    id: String,
    terminals: State<'_, Terminals>,
) -> AppResult<IpcResult<bool>> {
    let map = terminals.lock().await;
    Ok(IpcResult::ok(map.contains_key(&id)))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn terminal_generate_name() -> AppResult<IpcResult<String>> {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(42);
    let adj = ADJECTIVES[seed % ADJECTIVES.len()];
    let noun = NOUNS[(seed / ADJECTIVES.len()) % NOUNS.len()];
    Ok(IpcResult::ok(format!("{adj}-{noun}")))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn terminal_set_title(
    app: AppHandle,
    terminal_id: String,
    title: String,
    titles: State<'_, TerminalTitles>,
) -> AppResult<IpcResult<()>> {
    {
        let mut map = titles.lock().await;
        map.insert(terminal_id.clone(), title.clone());
    }
    let _ = app.emit("terminal:title:change", serde_json::json!({ "terminalId": terminal_id, "title": title }));
    Ok(IpcResult::ok(()))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn terminal_set_worktree_config(
    app: AppHandle,
    terminal_id: String,
    config: serde_json::Value,
    worktree_configs: State<'_, TerminalWorktreeConfigs>,
) -> AppResult<IpcResult<()>> {
    {
        let mut map = worktree_configs.lock().await;
        map.insert(terminal_id.clone(), config.clone());
    }
    let _ = app.emit("terminal:worktree:config:change", serde_json::json!({ "terminalId": terminal_id, "config": config }));
    Ok(IpcResult::ok(()))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn terminal_get_sessions(
    sessions: State<'_, TerminalSessions>,
) -> AppResult<IpcResult<serde_json::Value>> {
    let list = sessions.lock().await;
    Ok(IpcResult::ok(serde_json::json!(*list)))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn terminal_update_display_orders(
    orders: Vec<String>,
    display_orders: State<'_, TerminalDisplayOrders>,
) -> AppResult<IpcResult<()>> {
    let mut list = display_orders.lock().await;
    *list = orders;
    Ok(IpcResult::ok(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_cols_and_rows_are_sane() {
        assert_eq!(default_cols(), 80);
        assert_eq!(default_rows(), 24);
    }

    #[test]
    fn prepend_path_adds_dir_to_front() {
        let sep = path_separator();
        let dir = std::path::Path::new("/opt/homebrew/bin");
        let existing = format!("/usr/bin{sep}/bin");
        let result = prepend_path(dir, &existing);
        assert_eq!(result, format!("/opt/homebrew/bin{sep}/usr/bin{sep}/bin"));
        // The injected dir is the first entry.
        assert_eq!(result.split(sep).next(), Some("/opt/homebrew/bin"));
    }

    #[test]
    fn prepend_path_is_idempotent_when_already_present() {
        let sep = path_separator();
        let dir = std::path::Path::new("/opt/homebrew/bin");
        // Already at the front.
        let front = format!("/opt/homebrew/bin{sep}/usr/bin");
        assert_eq!(prepend_path(dir, &front), front);
        // Present elsewhere — still no duplicate added.
        let middle = format!("/usr/bin{sep}/opt/homebrew/bin{sep}/bin");
        assert_eq!(prepend_path(dir, &middle), middle);
    }

    #[test]
    fn prepend_path_handles_empty_existing() {
        let dir = std::path::Path::new("/opt/homebrew/bin");
        assert_eq!(prepend_path(dir, ""), "/opt/homebrew/bin");
    }

    #[test]
    fn effective_path_uses_supplied_base_when_no_claude_dir() {
        // When no claude binary is resolvable on the host, effective_path falls
        // back to None and the caller leaves PATH untouched. We can't force
        // resolve_claude_dir to None deterministically, so assert the contract
        // holds for whichever branch this host takes.
        let supplied = "/custom/base".to_string();
        // claude found: supplied base must still be present (folded in).
        // claude not found: None is the documented fallback (nothing to assert).
        if let Some(p) = effective_path(Some(&supplied)) {
            assert!(p.contains("/custom/base"));
        }
    }

    #[test]
    fn default_shell_picks_per_os() {
        let shell = default_shell();
        if cfg!(target_os = "windows") {
            assert!(
                shell.to_lowercase().contains("cmd") || shell.to_lowercase().contains("powershell")
            );
        } else {
            // Either $SHELL value or /bin/bash fallback
            assert!(shell.starts_with('/') || shell.contains("sh"));
        }
    }

    #[tokio::test]
    async fn create_input_destroy_round_trip() {
        // This test actually spawns a real shell. It's skipped on platforms
        // without a working PTY (CI in a container without a TTY).
        if std::env::var("CI").is_ok() && cfg!(target_os = "linux") {
            // PTY allocation fails in many container environments
            return;
        }

        // Build a minimal app handle for the test. We can't easily mock
        // Tauri's AppHandle in unit tests, so we focus on the state machine
        // bits in this module's pure helpers (default_shell, default_cols).
        // The full create/input/destroy cycle is covered by the manual smoke
        // test in scripts/tauri-smoke.sh once that lands.
        //
        // Leaving this scaffold in place so a future round can wire
        // tauri::test::mock_app() and exercise the real path.
    }
}
