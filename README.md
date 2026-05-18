# Aperant-MCP — Tauri Migration Fork

[![License](https://img.shields.io/badge/license-AGPL--3.0-green?style=flat-square)](./agpl-3.0.txt)
[![Discord](https://img.shields.io/badge/Discord-Join%20Community-5865F2?style=flat-square&logo=discord&logoColor=white)](https://discord.gg/KCXaPBr4Dj)
[![YouTube](https://img.shields.io/badge/YouTube-Subscribe-FF0000?style=flat-square&logo=youtube&logoColor=white)](https://www.youtube.com/@AndreMikalsen)
[![CI](https://img.shields.io/github/actions/workflow/status/AndyMik90/Auto-Claude/ci.yml?branch=main&style=flat-square&label=CI)](https://github.com/AndyMik90/Auto-Claude/actions)
[![Tauri](https://img.shields.io/badge/Tauri-2.x-FFC131?style=flat-square&logo=tauri&logoColor=white)](https://tauri.app)
[![Mentioned in Awesome Claude Code](https://awesome.re/mentioned-badge-flat.svg)](https://github.com/hesreallyhim/awesome-claude-code)

Fork of [Aperant](https://github.com/AndyMik90/Auto-Claude) — extended with a custom MCP system, automatic recovery infrastructure, and a full **Tauri 2 backend** that replaces Electron's Node.js main process with compiled Rust. This fork adds **22,000+ lines** across 114 files on top of the original, including 12,000+ lines of Rust across 23 source files.

**This is a great tool for building dynamic pipelines and further automating your agentic workflows. Run overnight batches, let the Master LLM recover stuck tasks autonomously, and ship to a binary that is ~12× smaller than the Electron build.**

> **Tauri branch:** `tauri-migration` (v0.2.0-beta.0) — `npm run dev` now defaults to Tauri. Electron is preserved as `npm run dev:electron` for production parity until full release.

> **MCP note:** The RDR message **delivery pipeline** currently targets **Windows** (PowerShell + Win32 API), **VS Code** (process-level window detection), and **Claude Code** (JSONL transcript reading). The delivery is blind "focus window, paste, enter." Each layer can be swapped independently. Contributions for macOS/Linux or other LLM CLIs are welcome. See [Watchdog Process](#watchdog-process).

---

## Why Tauri? Philosophy & Motivation

Electron is the industry standard for cross-platform desktop apps with a web UI — but it pays a heavy price: it ships an entire Chromium browser and a Node.js runtime with every install, regardless of what is already on the user's machine.

**The numbers tell the story:**

| Metric | Electron build | Tauri build (this fork) |
|--------|---------------|------------------------|
| macOS DMG size | ~100 MB | **8.3 MB** |
| Runtime bundled | Chromium + Node.js | None (system WebView) |
| Backend language | JavaScript (Node.js) | Rust (compiled native binary) |
| Memory footprint (idle) | ~200–400 MB | ~40–80 MB |
| Cold startup | ~2–4 s | ~0.5–1 s |

Tauri uses the **operating system's existing WebView** (WKWebView on macOS, WebView2 on Windows, WebKitGTK on Linux) instead of bundling Chromium. The React/TypeScript renderer is completely unchanged — only the backend main process is replaced.

### Why Rust for the backend?

The original Electron main process is written in TypeScript and runs inside Node.js. Replacing it with Rust gives:

- **Memory safety without a garbage collector.** No GC pauses means the UI never freezes during PTY I/O or file-system polling.
- **Typed, compiled IPC contracts.** Every command registered in `tauri::generate_handler![]` is a real Rust function — the compiler rejects mismatches before any binary is built. There are no magic IPC strings that fail silently at runtime.
- **Native OS integration.** PTY (terminal), file system watchers, OS credential storage, and process spawning all happen through Rust crates that compile to native code, not Node.js wrappers around C++ bindings.
- **Small, auditable attack surface.** Tauri's allow-list model means the renderer can only call commands you explicitly register. No arbitrary `eval` or `require` paths are available.

### The philosophy in one sentence

> Ship the smallest possible binary, keep the React UI untouched, and put correctness guarantees in the compiler instead of in runtime checks.

---

## Tauri Migration: What Changed

### Architecture

The Electron main process (`src/main/`) has been replaced by a Tauri Rust backend (`src-tauri/src/`). The renderer (`src/renderer/`) is identical. A transparent proxy-based shim (`src/preload/electron-shim.ts`) mounts `window.electronAPI` and routes all 262 methods to either real Tauri `invoke()` calls or Tauri event `listen()` subscriptions:

```
Renderer (React/TypeScript) — unchanged
         ↓ window.electronAPI.*
electron-shim.ts (Proxy + safeInvoke + listen)
         ↓ invoke() / listen()
Tauri Rust backend (src-tauri/src/)
         ↓ spawn / emit / FS / OS APIs
Python backend runners (apps/backend/)
```

### Rust backend (12,000+ lines across 23 source files)

| Module | Commands | Notes |
|--------|----------|-------|
| `agent.rs` | 4 | Python subprocess spawn, streaming stdout → `agent:output` events |
| `changelog.rs` | 11 | Git log, tag listing, AI changelog generation via `ai_analyzer_runner.py` |
| `claude_code.rs` | 6 | Claude Code version detection and installation |
| `debug.rs` | 6 | Log folder access, crash diagnostics |
| `desktop.rs` | 4 | Multi-monitor desktop state tracking |
| `diagnostics.rs` | 4 | Usage state, RDR status |
| `file.rs` | 2 | File explorer read/list |
| `git.rs` | 6 | Branch detection, status, init |
| `github.rs` | 50+ | Full GitHub integration — PRs, issues, autofix, triage, batch ops |
| `ideation.rs` | 10 | Ideation tab via `ideation_runner.py` with streaming events |
| `insights.rs` | 10 | Chat sessions, streaming via `insights_runner.py` |
| `profiles.rs` | 35+ | Claude + API profiles, usage monitoring, provider account CRUD |
| `project.rs` | 10 | Project CRUD, env, kanban preferences |
| `review.rs` | 6 | Inline code review with per-line comments + AI triage |
| `roadmap.rs` | 7 | Roadmap generation via `roadmap_runner.py` |
| `settings.rs` | 10 | App settings, CLI tool detection |
| `shell.rs` | 6 | Directory picker, external links, terminal launch |
| `task.rs` | 15+ | Task CRUD, archive, log streaming, `activity_record` |
| `terminal.rs` | 10 | PTY via `portable-pty` crate, session management, display ordering |
| `watcher.rs` | 2 | File-system watcher for auto-refresh |
| `worktree.rs` | 12 | Diff, merge, PR creation, IDE integration |

### New features added in this branch

**Inline code review** — per-line comment system with AI triage:
- `review_triage_runner.py` Python runner that analyzes code changes and prioritizes review comments
- `InlineReview.tsx` component — shows diff hunks with comment threads
- `unified-diff.ts` parser — converts raw git diffs to structured hunk objects
- `review-comments-store.ts` — Zustand store for comment state
- Full Rust API: `task_review_file_patch`, `task_review_comments_list/add/delete/update`, `task_finalize_review_triage/apply`

**Activity log** — `activity_record` Rust command writes a JSONL audit trail to `<userData>/activity.jsonl` for every significant user action.

**Provider account CRUD** — `provider_account_save/update/delete/set_order` with on-disk persistence in `<userData>/provider-accounts.json`.

**Usage monitoring** — `usage_request_update`, `usage_request_all`, `usage_fetch_claude`, `profile_get_best_available`, priority ordering. Emits `profile:usage:updated`, `profile:all_usage_updated`, `profile:proactive_swap` events.

**GitHub streaming** — `github_pr_review` now emits `github:pr:logs:updated` on every stdout line so the log panel refreshes in real time during a PR review.

### Running the app

```bash
# Tauri (default — fast, small binary)
cd apps/frontend && npm run dev

# Electron (legacy, preserved for production parity)
cd apps/frontend && npm run dev:electron

# Production Tauri build
cd apps/frontend && npm run build:tauri
# Output: src-tauri/target/release/bundle/  (8.3 MB DMG on macOS aarch64)
```

### Tests

```bash
# Rust unit tests
cd apps/frontend/src-tauri && cargo test

# TypeScript / shim tests (3289 tests)
cd apps/frontend && npm test -- --run

# Type check
cd apps/frontend && npx tsc --noEmit
```

### To get the Master LLM working through the MCP

Copy the folders inside the `skills` folder in `.claude` to your personal `~/.claude/skills` folder.

[Quick video demo of MCP + Tauri implementation](https://www.youtube.com/watch?v=NHAm-M8Lawc)

---

## MCP Setup (Claude Code Integration)

To manage Auto-Claude tasks from Claude Code in any project, add the MCP server globally:

**Option A: CLI command (recommended)**

```bash
claude mcp add auto-claude-manager --scope user -- npx --yes tsx --import "file:///<path-to>/Aperant-MCP/apps/frontend/src/main/mcp-server/register-loader.mjs" "<path-to>/Aperant-MCP/apps/frontend/src/main/mcp-server/index.ts"
```

**Option B: Manual config** — add to `~/.claude.json` under `"mcpServers"`:

```json
{
  "auto-claude-manager": {
    "command": "npx",
    "args": [
      "--yes", "tsx", "--import",
      "file:///<path-to>/Aperant-MCP/apps/frontend/src/main/mcp-server/register-loader.mjs",
      "<path-to>/Aperant-MCP/apps/frontend/src/main/mcp-server/index.ts"
    ]
  }
}
```

Replace `<path-to>` with your absolute install path (e.g., `C:/Users/you/repos`). Restart Claude Code after adding.

> **Windows users:** The `--import` path **must** use `file:///C:/...` — bare `C:/` paths fail because Node.js ESM interprets `C:` as a URL scheme.

## What This Fork Adds

### MCP Server (Claude Code Integration)

A full MCP (Model Context Protocol) server that lets Claude Code interact with Auto-Claude directly. Create, manage, monitor, and recover tasks programmatically instead of through the UI.


**15 MCP Tools:**

| Tool                               | Purpose                                                        |
| ---------------------------------- | -------------------------------------------------------------- |
| `create_task`                    | Create a single task with full configuration                   |
| `list_tasks`                     | List all tasks, filterable by status                           |
| `get_task_status`                | Detailed status including phase/subtask progress               |
| `start_task`                     | Start task execution                                           |
| `start_batch`                    | Create and start multiple tasks at once                        |
| `wait_for_human_review`          | Monitor tasks, execute callback (e.g., shutdown) when complete |
| `get_tasks_needing_intervention` | Get all tasks needing recovery                                 |
| `get_task_error_details`         | Detailed error info with logs and QA reports                   |
| `recover_stuck_task`             | Recover tasks stuck in recovery mode                           |
| `submit_task_fix_request`        | Submit fix guidance for failing tasks                          |
| `get_task_logs`                  | Phase-specific logs (planning, coding, validation)             |
| `get_rdr_batches`                | Get pending recovery batches by problem type                   |
| `process_rdr_batch`              | Process a batch of tasks through the recovery system           |
| `trigger_auto_restart`           | Restart app with build on crash/error detection                |
| `test_force_recovery`            | Force tasks into recovery mode for testing                     |

### RDR System (Recover, Debug, Resend)

Automatic 6-priority recovery system that detects stuck/failed tasks and sends a detailed prompt to the Master LLM through the MCP system so it acts on the tasks:

| Priority | Name              | When                     | Action                                          |
| -------- | ----------------- | ------------------------ | ----------------------------------------------- |
| P1       | Auto-CONTINUE     | Task not in expected board | Sets `start_requested`, task self-recovers   |
| P2       | Auto-RECOVER      | Task in recovery mode    | Clears stuck state, restarts                    |
| P3       | Request Changes   | P1 failed 3+ times       | Writes detailed fix request with error analysis |
| P4       | Auto-fix JSON     | Corrupted plan files     | Rebuilds valid JSON structure                   |
| P5       | Manual Debug      | Pattern detection needed | Root cause investigation                        |
| P6       | Delete & Recreate or Change AC code and Rebuild | Last resort              | Delete the task and recreate or Change AC code and rebuild if the case                    |

Automatic escalation: tasks that enter Recovery become P2, then P3 after 3 attempts on P1 scaling up to P6B. Attempt counters reset on app startup.

### Auto-Shutdown Monitor

Monitors all running tasks and automatically shuts down the computer when all tasks reach completion. Start X number of tasks, go to sleep, computer powers off when done.

- Status-based completion detection (`done` / `pr_created` / `human_review`)
- Worktree-aware (reads real progress, not stale main copies)
- Configurable via UI toggle or MCP `wait_for_human_review` tool

### Auto-Refresh (Real-Time UI Updates)

File watcher detects all plan status changes and pushes updates to the Kanban board in real-time (~1 second). No manual refresh needed when MCP tools modify task files.

### Task Chaining (CI/CD-Style Pipelines)

Chain tasks to auto-start sequentially on task creation with the status start_requested on tasks:

```
Task A (creates and starts) --> Task B (creates and starts) --> Task C (creates and starts)
```

Configurable per-task with optional human approval gates between steps.

### Output Monitor

Monitors Claude Code session state via JSONL transcripts. Distinguishes between user sessions and task agent sessions to prevent false busy-state detection.

### Watchdog Process

External wrapper process that monitors Auto-Claude health, detects crashes, and can auto-restart. It spawns Electron as a child process and watches it from outside. The watchdog does **not** run when launching the app directly (`.exe`, `npm run dev`).

<details>
<summary><strong>Quick Setup (Windows)</strong></summary>

1. Rename `Aperant-MCP.example.bat` to `Aperant-MCP.bat`
2. Edit the path in the `.bat` to point to your install directory:
   ```bat
   set AUTO_CLAUDE_DIR=C:\Users\YourName\path\to\Aperant-MCP
   ```
3. Double-click the `.bat` to launch with watchdog
4. **Optional — pin to taskbar:** Create a shortcut with target:
   ```
   cmd.exe /c "C:\Users\YourName\path\to\Aperant-MCP\Aperant-MCP.bat"
   ```
   Then right-click the shortcut → Pin to taskbar. You can set the icon to `apps\frontend\resources\icon.ico` from the repo.

</details>

<details>
<summary><strong>Quick Setup (macOS/Linux)</strong></summary>

Create a shell script equivalent (e.g. `auto-claude-mcp.sh`):
```bash
#!/bin/bash
cd "$(dirname "$0")/apps/frontend"
echo "Starting Auto-Claude with crash recovery watchdog..."
npx tsx src/main/watchdog/launcher.ts ../../node_modules/.bin/electron out/main/index.js
```
Make it executable: `chmod +x auto-claude-mcp.sh`

</details>

### Window Manager (Windows)

PowerShell-based message delivery that sends RDR recovery prompts directly to Claude Code's terminal via clipboard paste. Handles VS Code window detection, focus management, and busy-state checking.

### Additional Features

- **Crash Recovery** - Automatic recovery from app crashes with state preservation
- **Graceful Restart** - Clean restart with build when errors detected
- **Rate Limit Handling** - Detection and intelligent waiting for API rate limits
- **HuggingFace Integration** - OAuth flow and repository management
- **Worktree-Aware Architecture** - All subsystems prefer worktree data over stale main project data

---

**Autonomous multi-agent coding framework that plans, builds, and validates software for you. Check the original repo:** https://github.com/AndyMik90/Auto-Claude

![Aperant-MCP Kanban Board](.github/assets/Auto-Claude-Kanban.png)

### Stable Release

<!-- STABLE_VERSION_BADGE -->
[![Stable](https://img.shields.io/badge/stable-2.7.5-blue?style=flat-square)](https://github.com/AndyMik90/Auto-Claude/releases/tag/v2.7.5)
<!-- STABLE_VERSION_BADGE_END -->

<!-- STABLE_DOWNLOADS -->
| Platform | Download |
|----------|----------|
| **Windows** | [Auto-Claude-2.7.5-win32-x64.exe](https://github.com/AndyMik90/Auto-Claude/releases/download/v2.7.5/Auto-Claude-2.7.5-win32-x64.exe) |
| **macOS (Apple Silicon)** | [Auto-Claude-2.7.5-darwin-arm64.dmg](https://github.com/AndyMik90/Auto-Claude/releases/download/v2.7.5/Auto-Claude-2.7.5-darwin-arm64.dmg) |
| **macOS (Intel)** | [Auto-Claude-2.7.5-darwin-x64.dmg](https://github.com/AndyMik90/Auto-Claude/releases/download/v2.7.5/Auto-Claude-2.7.5-darwin-x64.dmg) |
| **Linux** | [Auto-Claude-2.7.5-linux-x86_64.AppImage](https://github.com/AndyMik90/Auto-Claude/releases/download/v2.7.5/Auto-Claude-2.7.5-linux-x86_64.AppImage) |
| **Linux (Debian)** | [Auto-Claude-2.7.5-linux-amd64.deb](https://github.com/AndyMik90/Auto-Claude/releases/download/v2.7.5/Auto-Claude-2.7.5-linux-amd64.deb) |
| **Linux (Flatpak)** | [Auto-Claude-2.7.5-linux-x86_64.flatpak](https://github.com/AndyMik90/Auto-Claude/releases/download/v2.7.5/Auto-Claude-2.7.5-linux-x86_64.flatpak) |
<!-- STABLE_DOWNLOADS_END -->

### Beta Release

> ⚠️ Beta releases may contain bugs and breaking changes. [View all releases](https://github.com/AndyMik90/Auto-Claude/releases)

<!-- BETA_VERSION_BADGE -->
[![Beta](https://img.shields.io/badge/beta-2.7.6--beta.5-orange?style=flat-square)](https://github.com/AndyMik90/Auto-Claude/releases/tag/v2.7.6-beta.5)
<!-- BETA_VERSION_BADGE_END -->

<!-- BETA_DOWNLOADS -->
| Platform | Download |
|----------|----------|
| **Windows** | [Auto-Claude-2.7.6-beta.5-win32-x64.exe](https://github.com/AndyMik90/Auto-Claude/releases/download/v2.7.6-beta.5/Auto-Claude-2.7.6-beta.5-win32-x64.exe) |
| **macOS (Apple Silicon)** | [Auto-Claude-2.7.6-beta.5-darwin-arm64.dmg](https://github.com/AndyMik90/Auto-Claude/releases/download/v2.7.6-beta.5/Auto-Claude-2.7.6-beta.5-darwin-arm64.dmg) |
| **macOS (Intel)** | [Auto-Claude-2.7.6-beta.5-darwin-x64.dmg](https://github.com/AndyMik90/Auto-Claude/releases/download/v2.7.6-beta.5/Auto-Claude-2.7.6-beta.5-darwin-x64.dmg) |
| **Linux** | [Auto-Claude-2.7.6-beta.5-linux-x86_64.AppImage](https://github.com/AndyMik90/Auto-Claude/releases/download/v2.7.6-beta.5/Auto-Claude-2.7.6-beta.5-linux-x86_64.AppImage) |
| **Linux (Debian)** | [Auto-Claude-2.7.6-beta.5-linux-amd64.deb](https://github.com/AndyMik90/Auto-Claude/releases/download/v2.7.6-beta.5/Auto-Claude-2.7.6-beta.5-linux-amd64.deb) |
| **Linux (Flatpak)** | [Auto-Claude-2.7.6-beta.5-linux-x86_64.flatpak](https://github.com/AndyMik90/Auto-Claude/releases/download/v2.7.6-beta.5/Auto-Claude-2.7.6-beta.5-linux-x86_64.flatpak) |
<!-- BETA_DOWNLOADS_END -->

> All releases include SHA256 checksums and VirusTotal scan results for security verification.

---

## Requirements

- **Claude Pro/Max subscription** - [Get one here](https://claude.ai/upgrade)
- **Claude Code CLI** - `npm install -g @anthropic-ai/claude-code`
- **Git repository** - Your project must be initialized as a git repo

---

## Project Structure

```
Auto-Claude/
├── apps/
│   ├── backend/     # Python agents, specs, QA pipeline
│   └── frontend/    # Electron desktop application
├── guides/          # Additional documentation
├── tests/           # Test suite
└── scripts/         # Build utilities
```

---

## License

**AGPL-3.0** - GNU Affero General Public License v3.0

Aperant-MCP is free to use. If you modify and distribute it, or run it as a service, your code must also be open source under AGPL-3.0.

Commercial licensing available for closed-source use cases.

---
