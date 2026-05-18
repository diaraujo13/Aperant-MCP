# Phase 5 — Task Execution (Rust → Python)

**Date:** 2026-05-06
**Branch:** tauri-migration
**Goal:** Eliminate UI thread blocking and high memory usage in the Tauri desktop build by moving agent process spawning and output streaming from Node.js/Electron into the Rust async runtime.

---

## Problem

The Electron build manages agent subprocesses (planner/coder/QA pipeline) inside the Node.js main process. This causes:

- Main UI thread blocking during any heavy operation
- High memory consumption with multiple projects open
- Crashes under parallel agent load
- Jank on the renderer from synchronous IPC processing

## Goal

Port task execution (`startTask`, `stopTask`, `recoverTask`) to Tauri Phase 5 so:

1. Agent processes are spawned and managed by Rust's async runtime (tokio)
2. stdout/stderr output streams to the renderer via Tauri events — never blocking the UI
3. The Python backend (`apps/backend/run.py`) runs unchanged as a child process
4. The `electron-shim.ts` exposes real implementations instead of stubs

---

## Architecture

```
Renderer (React)
    │  invoke('agent_start', { taskId, projectPath, specId })
    ▼
Tauri Rust backend
    ├── api/agent.rs          ← commands: agent_start / agent_stop / agent_recover
    ├── agent/manager.rs      ← AgentManager: HashMap<taskId, RunningAgent>
    │       tokio::process::Command spawns python run.py
    │       tokio task reads stdout/stderr async
    │       emits Tauri events: agent:output, agent:state, agent:exit
    └── state.rs              ← AgentManager added to AppState
```

`agent_start` returns immediately after spawning. The Python process runs independently. Output arrives at the renderer via Tauri events — same infrastructure as the terminal (Phase 4b).

---

## Python Invocation

```
<python>  apps/backend/run.py  --spec <specId>  --project <projectPath>  [--recover]
```

Python resolution order:
1. `<projectPath>/apps/backend/.venv/bin/python`
2. `<workspaceRoot>/.venv/bin/python`
3. `python3` on PATH
4. `python` on PATH (fallback)

`--recover` flag is passed for `agent_recover` calls.

---

## Rust Data Structures

```rust
// agent/manager.rs
struct RunningAgent {
    task_id: String,
    child: tokio::process::Child,
    started_at: std::time::SystemTime,
}

pub struct AgentManager {
    agents: Arc<Mutex<HashMap<String, RunningAgent>>>,
}
```

`AgentManager` is added to `AppState` in `state.rs`.

---

## Tauri Commands (api/agent.rs)

| Command | Signature | Notes |
|---|---|---|
| `agent_start` | `(task_id, project_path, spec_id) → IpcResult<{started: bool}>` | Spawns python, returns immediately |
| `agent_stop` | `(task_id) → IpcResult<()>` | Kills child process |
| `agent_recover` | `(task_id, project_path, spec_id) → IpcResult<{started: bool}>` | Same as start + `--recover` flag |

---

## Tauri Events (renderer subscription)

| Event | Payload | Trigger |
|---|---|---|
| `agent:output` | `{ taskId: string, stream: "stdout"\|"stderr", data: string }` | Each line from python process |
| `agent:state` | `{ taskId: string, state: "running"\|"stopped"\|"crashed" }` | State transitions |
| `agent:exit` | `{ taskId: string, code: number\|null }` | Process exits |

---

## electron-shim.ts additions

```ts
const agentAPI = {
  startTask: (taskId: string, projectPath: string, specId: string) =>
    safeInvoke<{ started: boolean }>('agent_start', { taskId, projectPath, specId }),
  stopTask: (taskId: string) =>
    safeInvoke<null>('agent_stop', { taskId }),
  recoverTask: (taskId: string, projectPath: string, specId: string) =>
    safeInvoke<{ started: boolean }>('agent_recover', { taskId, projectPath, specId }),
  onAgentOutput: (callback: (payload: { taskId: string; stream: string; data: string }) => void) => {
    const p = listen<{ taskId: string; stream: string; data: string }>(
      'agent:output', (e) => callback(e.payload)
    );
    return makeUnsubscribe(p);
  },
  onAgentStateChanged: (callback: (payload: { taskId: string; state: string }) => void) => {
    const p = listen<{ taskId: string; state: string }>(
      'agent:state', (e) => callback(e.payload)
    );
    return makeUnsubscribe(p);
  },
  onAgentExit: (callback: (payload: { taskId: string; code: number | null }) => void) => {
    const p = listen<{ taskId: string; code: number | null }>(
      'agent:exit', (e) => callback(e.payload)
    );
    return makeUnsubscribe(p);
  },
};
```

These merge into the `implemented` object in the shim alongside `taskAPI`.

---

## Error Handling

| Situation | Behavior |
|---|---|
| Python not found | `agent_start` returns `IpcResult { success: false, error: "python_not_found" }` |
| Task already running | Returns `IpcResult { success: false, error: "already_running" }` — no second process spawned |
| Process crash (non-zero exit) | Emits `agent:state { state: "crashed" }` then `agent:exit { code }` |
| `agent_stop` on unknown task | Silent no-op, returns success |
| Mutex poison | Returns `IpcResult { success: false, error: "internal_error" }` |

---

## Out of Scope (stubs remain)

- Worktree management
- Agent queue ordering / priority
- IDE integration
- Claude profile switching during execution
- Agent log persistence
- Multi-account rate limit switching
