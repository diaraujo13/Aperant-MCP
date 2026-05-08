// Tauri compatibility shim. When the renderer runs inside a Tauri webview, this
// mounts a `window.electronAPI` surface that mirrors the Electron preload API,
// proxying every call through Tauri's `invoke()` instead of `ipcRenderer.invoke()`.
//
// In Electron, `__TAURI_INTERNALS__` is undefined, so this module is a no-op.
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import type {
  DesktopProjectActivation,
  DesktopStateSnapshot,
} from '../shared/types/desktop';
import type { ElectronAPI } from '../shared/types/ipc';

declare global {
  interface Window {
    __TAURI_INTERNALS__?: unknown;
  }
}

function isTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

interface IPCResult<T> {
  success: boolean;
  data?: T;
  error?: string;
}

async function safeInvoke<T>(
  command: string,
  args?: Record<string, unknown>
): Promise<IPCResult<T>> {
  try {
    return (await invoke<IPCResult<T>>(command, args)) as IPCResult<T>;
  } catch (err) {
    return {
      success: false,
      error: err instanceof Error ? err.message : String(err),
    } as IPCResult<T>;
  }
}

function makeUnsubscribe(unlistenPromise: Promise<UnlistenFn>): () => void {
  return () => {
    void unlistenPromise.then((u) => u()).catch(() => {});
  };
}

// Generic stub for un-ported domain methods. Pattern-matches on method name
// to return shapes the renderer expects: events return unsubscribe fns,
// everything else returns a rejected IPCResult.
function makeStub(methodName: string): (...args: unknown[]) => unknown {
  // Event subscriber: starts with "on" + uppercase letter, returns unsubscribe fn
  if (/^on[A-Z]/.test(methodName)) {
    return (..._args: unknown[]) => {
      // No-op subscribe; returns no-op unsubscribe
      return () => {};
    };
  }
  // Synchronous void method: contains "record" or "send" — returns nothing
  if (/^(record|send|emit|notify)/i.test(methodName)) {
    return (..._args: unknown[]) => undefined;
  }
  // Default: async method returning IPCResult-shaped failure
  return async (..._args: unknown[]) => ({
    success: false,
    error: `[shim] '${methodName}' not implemented in Phase 1 (Tauri migration in progress)`,
  });
}

if (isTauri() && typeof window.electronAPI === 'undefined') {
  const ua = navigator.userAgent.toLowerCase();
  window.platform = {
    isWindows: ua.includes('windows'),
    isMacOS: ua.includes('mac os') || ua.includes('macintosh'),
    isLinux: ua.includes('linux') && !ua.includes('android'),
    isUnix: !ua.includes('windows'),
  };
  window.DEBUG = false;

  const desktopAPI = {
    getDesktopState: () =>
      safeInvoke<DesktopStateSnapshot>('desktop_state_get'),
    setDesktopPinEnabled: (enabled: boolean) =>
      safeInvoke<DesktopStateSnapshot>('desktop_pin_set', { enabled }),
    associateProjectToCurrentDesktop: (projectId: string) =>
      safeInvoke<DesktopStateSnapshot>('desktop_project_associate', { projectId }),
    clearProjectDesktopAssociation: (projectId: string) =>
      safeInvoke<DesktopStateSnapshot>('desktop_project_clear', { projectId }),
    onDesktopStateChanged: (callback: (state: DesktopStateSnapshot) => void) => {
      const p = listen<DesktopStateSnapshot>('desktop:state:changed', (event) =>
        callback(event.payload)
      );
      return makeUnsubscribe(p);
    },
    onDesktopProjectActivated: (
      callback: (activation: DesktopProjectActivation) => void
    ) => {
      const p = listen<DesktopProjectActivation>('desktop:project:activate', (event) =>
        callback(event.payload)
      );
      return makeUnsubscribe(p);
    },
  };

  // Settings domain (Phase 2 round 1) — getSettings/saveSettings persist to the
  // SAME settings.json the Electron build uses (~/Library/Application Support/auto-claude-ui/
  // on macOS), so settings round-trip cleanly between the two builds during the
  // parallel ship period.
  const settingsAPI = {
    getSettings: () =>
      safeInvoke<Record<string, unknown>>('settings_get'),
    saveSettings: (settings: Record<string, unknown>) =>
      safeInvoke<null>('settings_save', { settings }),
    getCliToolsInfo: () =>
      safeInvoke<unknown>('settings_get_cli_tools_info'),
    getClaudeCodeOnboardingStatus: () =>
      safeInvoke<unknown>('settings_claude_code_get_onboarding_status'),
    getProviderAccounts: () =>
      safeInvoke<unknown>('provider_accounts_get'),
    setSpellCheckLanguages: (language: string) =>
      safeInvoke<unknown>('spellcheck_set_languages', { language }),
    getSourceEnv: () =>
      safeInvoke<unknown>('autobuild_source_env_get'),
    // Raw-value methods (NOT IPCResult-wrapped to match Electron contract)
    getAppVersion: async () => {
      try {
        return await invoke<string>('app_version');
      } catch {
        return '0.0.0';
      }
    },
    getSentryDsn: async () => {
      try {
        return await invoke<string>('get_sentry_dsn');
      } catch {
        return '';
      }
    },
    getSentryConfig: async () => {
      try {
        return await invoke<{
          dsn: string;
          tracesSampleRate: number;
          profilesSampleRate: number;
        }>('get_sentry_config');
      } catch {
        return { dsn: '', tracesSampleRate: 0, profilesSampleRate: 0 };
      }
    },
    notifySentryStateChanged: (_enabled: boolean) => {
      // Phase 6 (Sentry split): wire to a Rust panic hook + browser SDK.
      // For now, no-op — Sentry is disabled in the Tauri build.
    },
  };

  // Claude Code domain (Phase 2 round 2) — CLI detection, version checks,
  // install command generation, active-path persistence to settings.json.
  const claudeCodeAPI = {
    checkClaudeCodeVersion: () =>
      safeInvoke<unknown>('claude_code_check_version'),
    installClaudeCode: () =>
      safeInvoke<unknown>('claude_code_install'),
    getClaudeCodeVersions: () =>
      safeInvoke<unknown>('claude_code_get_versions'),
    installClaudeCodeVersion: (version: string) =>
      safeInvoke<unknown>('claude_code_install_version', { version }),
    getClaudeCodeInstallations: () =>
      safeInvoke<unknown>('claude_code_get_installations'),
    setClaudeCodeActivePath: (cliPath: string) =>
      safeInvoke<unknown>('claude_code_set_active_path', { cliPath }),
  };

  // Project domain (Phase 2 round 3) — projects.json CRUD, tab state, kanban prefs.
  // Reads/writes the SAME file the Electron build uses (<userData>/store/projects.json)
  // so projects added in Electron show up immediately in Tauri and vice versa.
  const projectAPI = {
    getProjects: () => safeInvoke<unknown[]>('project_list'),
    addProject: (projectPath: string) =>
      safeInvoke<unknown>('project_add', { projectPath }),
    removeProject: (projectId: string) =>
      safeInvoke<null>('project_remove', { projectId }),
    updateProjectSettings: (projectId: string, settings: Record<string, unknown>) =>
      safeInvoke<null>('project_update_settings', { projectId, settings }),
    setAutoResumeAfterRateLimit: (projectId: string, enabled: boolean) =>
      safeInvoke<unknown>('project_set_auto_resume_after_rate_limit', {
        projectId,
        enabled,
      }),
    setRdrEnabled: (projectId: string, enabled: boolean) =>
      safeInvoke<unknown>('project_set_rdr_enabled', { projectId, enabled }),
    getTabState: () => safeInvoke<unknown>('tab_state_get'),
    saveTabState: (tabState: unknown) =>
      safeInvoke<null>('tab_state_save', { tabState }),
    getKanbanPreferences: (projectId: string) =>
      safeInvoke<unknown>('kanban_preferences_get', { projectId }),
    saveKanbanPreferences: (projectId: string, preferences: unknown) =>
      safeInvoke<null>('kanban_preferences_save', { projectId, preferences }),
  };

  // File explorer + screenshot + debug + diagnostics (Phase 2 round 4 light).
  // file_* are real (list/read with 10MB cap and dir-first sort).
  // screenshot/debug-clipboard/all of diagnostics are sensible stubs that
  // keep the renderer from crashing on subsystem calls that aren't ported yet.
  const fileAndDebugAPI = {
    listDirectory: (dirPath: string) =>
      safeInvoke<unknown[]>('file_explorer_list', { dirPath }),
    readFile: (filePath: string) =>
      safeInvoke<string>('file_explorer_read', { filePath }),
    getSources: () => invoke('screenshot_get_sources'),
    capture: (options: unknown) =>
      invoke('screenshot_capture', { options }),
    getDebugInfo: () => invoke('debug_get_info'),
    openLogsFolder: () => invoke('debug_open_logs_folder'),
    copyDebugInfo: () => invoke('debug_copy_debug_info'),
    getRecentErrors: (maxCount?: number) =>
      invoke('debug_get_recent_errors', { maxCount }),
    listLogFiles: () => invoke('debug_list_log_files'),
    triggerCrash: () => invoke('debug_trigger_crash'),
    getUsageState: () =>
      safeInvoke<unknown>('diag_get_usage_state'),
    getRdrState: () =>
      safeInvoke<unknown>('diag_get_rdr_state'),
    forceUsageFetch: () =>
      safeInvoke<unknown>('diag_force_usage_fetch'),
    sendTestRdr: () =>
      safeInvoke<unknown>('diag_send_test_rdr'),
  };

  // Task domain (Phase 2 round 4a + 4e) — CRUD + archive read/write the same
  // .auto-claude/specs/ directories the Electron build uses. Execution
  // (start/stop/recover), worktree, logs, and IDE integration remain Proxy
  // stubs until the corresponding subsystem rounds.
  const taskAPI = {
    getTasks: (projectId: string, options?: unknown) =>
      safeInvoke<unknown[]>('task_list', { projectId, options }),
    createTask: (
      projectId: string,
      title: string,
      description: string,
      metadata?: unknown,
    ) =>
      safeInvoke<unknown>('task_create', {
        projectId,
        title,
        description,
        metadata,
      }),
    deleteTask: (taskId: string) =>
      safeInvoke<null>('task_delete', { taskId }),
    updateTask: (taskId: string, updates: unknown) =>
      safeInvoke<unknown>('task_update', { taskId, updates }),
    archiveTasks: (projectId: string, taskIds: string[], version?: string) =>
      safeInvoke<boolean>('task_archive', { projectId, taskIds, version }),
    unarchiveTasks: (projectId: string, taskIds: string[]) =>
      safeInvoke<boolean>('task_unarchive', { projectId, taskIds }),
    toggleTaskRdr: (taskId: string, disabled: boolean) =>
      safeInvoke<boolean>('task_toggle_rdr', { taskId, disabled }),
  };

  // Specs file watcher (Phase 6b) — emits specs:changed when implementation_plan.json
  // is created or modified, so the Kanban board auto-refreshes without polling.
  // task_watch_project is called by the renderer when a project is opened;
  // watch_all_projects runs automatically at app startup for all registered projects.
  const watcherAPI = {
    watchProject: (projectId: string, projectPath: string) =>
      safeInvoke<boolean>('task_watch_project', { projectId, projectPath }),
    unwatchProject: (projectId: string) =>
      safeInvoke<boolean>('task_unwatch_project', { projectId }),
    onSpecsChanged: (
      callback: (payload: { projectId: string; specId: string }) => void,
    ) => {
      const p = listen<{ projectId: string; specId: string }>(
        'specs:changed',
        (e) => callback(e.payload),
      );
      return makeUnsubscribe(p);
    },
  };

  // Agent execution subsystem (Phase 5) — Python backend spawned by Rust's tokio
  // runtime. agent_start returns immediately; output arrives via Tauri events.
  // Worktree management, queue ordering, IDE integration, profile switching,
  // log persistence, and multi-account rate-limit switching remain stubs.
  const agentAPI = {
    startTask: (taskId: string, projectPath: string, specId: string) =>
      safeInvoke<{ started: boolean }>('agent_start', { taskId, projectPath, specId }),
    stopTask: (taskId: string) =>
      safeInvoke<null>('agent_stop', { taskId }),
    recoverTask: (taskId: string, projectPath: string, specId: string) =>
      safeInvoke<{ started: boolean }>('agent_recover', { taskId, projectPath, specId }),
    onAgentOutput: (callback: (payload: { taskId: string; stream: string; data: string }) => void) => {
      const p = listen<{ taskId: string; stream: string; data: string }>(
        'agent:output',
        (e) => callback(e.payload),
      );
      return makeUnsubscribe(p);
    },
    onAgentStateChanged: (callback: (payload: { taskId: string; state: string }) => void) => {
      const p = listen<{ taskId: string; state: string }>(
        'agent:state',
        (e) => callback(e.payload),
      );
      return makeUnsubscribe(p);
    },
    onAgentExit: (callback: (payload: { taskId: string; code: number | null }) => void) => {
      const p = listen<{ taskId: string; code: number | null }>(
        'agent:exit',
        (e) => callback(e.payload),
      );
      return makeUnsubscribe(p);
    },
  };

  // Terminal subsystem (Phase 4 spike) — real PTY via portable-pty Rust crate.
  // Output streams via Tauri events; renderer xterm.js sees keystrokes round-trip.
  // Claude integration / session restore / worktree config remain Proxy stubs
  // until their dedicated rounds.
  const terminalAPI = {
    createTerminal: (options: {
      id: string;
      cwd?: string;
      shell?: string;
      cols?: number;
      rows?: number;
      env?: Record<string, string>;
    }) => safeInvoke<{ id: string }>('terminal_create', { options }),
    destroyTerminal: (id: string) =>
      safeInvoke<null>('terminal_destroy', { id }),
    sendTerminalInput: (id: string, data: string) => {
      // Fire-and-forget to match the Electron contract (it uses ipcRenderer.send)
      void invoke('terminal_input', { id, data }).catch(() => {});
    },
    resizeTerminal: (id: string, cols: number, rows: number) =>
      safeInvoke<{ success: boolean }>('terminal_resize', { id, cols, rows }),
    onTerminalOutput: (callback: (id: string, data: string) => void) => {
      // Tauri emits a single payload object; renderer expects (id, data) positional
      const p = listen<{ id: string; data: string }>(
        'terminal:output',
        (event) => callback(event.payload.id, event.payload.data),
      );
      return makeUnsubscribe(p);
    },
    onTerminalExit: (
      callback: (id: string, code: number | null) => void,
    ) => {
      const p = listen<{ id: string; code: number | null }>(
        'terminal:exit',
        (event) => callback(event.payload.id, event.payload.code),
      );
      return makeUnsubscribe(p);
    },
    checkPtyAlive: (id: string) =>
      safeInvoke<boolean>('terminal_check_alive', { id }),
  };

  // GitHub domain (Phase 7) — all calls delegate to gh CLI via Rust commands.
  // Events (onPRReviewProgress, etc.) are stubs: the Python runner that emits
  // them is not yet ported. They return no-op unsubscribe functions so the
  // renderer can register listeners without crashing.
  const noopUnsub = () => {};
  const githubAPI = {
    // auth / cli
    checkCLI: () => safeInvoke<unknown>('github_check_cli'),
    checkAuth: () => safeInvoke<unknown>('github_check_auth'),
    getToken: () => safeInvoke<unknown>('github_get_token'),
    getUser: () => safeInvoke<unknown>('github_get_user'),
    startAuth: () => safeInvoke<unknown>('github_start_auth'),
    detectRepo: (projectPath: string) =>
      safeInvoke<unknown>('github_detect_repo', { projectPath }),
    getBranches: (projectId: string) =>
      safeInvoke<unknown>('github_get_branches', { projectId }),
    listUserRepos: () => safeInvoke<unknown>('github_list_user_repos'),
    listOrgs: () => safeInvoke<unknown>('github_list_orgs'),
    createRepo: (repoName: string, isPrivate: boolean) =>
      safeInvoke<unknown>('github_create_repo', { repoName, isPrivate }),
    addRemote: (projectPath: string, repoUrl: string) =>
      safeInvoke<unknown>('github_add_remote', { projectPath, repoUrl }),
    // repository
    checkGitHubConnection: (projectId: string) =>
      safeInvoke<unknown>('github_check_connection', { projectId }),
    getRepositories: () => safeInvoke<unknown>('github_get_repositories'),
    // issues
    getIssues: (projectId: string, state?: string, page?: number) =>
      safeInvoke<unknown>('github_get_issues', { projectId, state, page }),
    getIssue: (projectId: string, issueNumber: number) =>
      safeInvoke<unknown>('github_get_issue', { projectId, issueNumber }),
    getIssueComments: (projectId: string, issueNumber: number) =>
      safeInvoke<unknown>('github_get_issue_comments', { projectId, issueNumber }),
    importIssues: (projectId: string, issueNumbers: number[]) =>
      safeInvoke<unknown>('github_import_issues', { projectId, issueNumbers }),
    // PRs — read
    listPRs: (projectId: string) =>
      safeInvoke<unknown>('github_pr_list', { projectId }),
    listMorePRs: (projectId: string, page?: number) =>
      safeInvoke<unknown>('github_pr_list_more', { projectId, page }),
    getPR: (projectId: string, prNumber: number) =>
      safeInvoke<unknown>('github_pr_get', { projectId, prNumber }),
    getPRDiff: (projectId: string, prNumber: number) =>
      safeInvoke<unknown>('github_pr_get_diff', { projectId, prNumber }),
    getPRReview: (projectId: string, prNumber: number) =>
      safeInvoke<unknown>('github_pr_get_review', { projectId, prNumber }),
    getPRReviewsBatch: (projectId: string, prNumbers: number[]) =>
      safeInvoke<unknown>('github_pr_get_reviews_batch', { projectId, prNumbers }),
    checkNewCommits: (projectId: string, prNumber: number) =>
      safeInvoke<unknown>('github_pr_check_new_commits', { projectId, prNumber }),
    checkMergeReadiness: (projectId: string, prNumber: number) =>
      safeInvoke<unknown>('github_pr_check_merge_readiness', { projectId, prNumber }),
    getPRLogs: (projectId: string, prNumber: number) =>
      safeInvoke<unknown>('github_pr_get_logs', { projectId, prNumber }),
    getWorkflowsAwaitingApproval: (projectId: string, prNumber: number) =>
      safeInvoke<unknown>('github_workflows_awaiting_approval', { projectId, prNumber }),
    // PRs — write
    postPRReview: (projectId: string, prNumber: number, reviewBody: string, event?: string) =>
      safeInvoke<unknown>('github_pr_post_review', { projectId, prNumber, reviewBody, event }),
    deletePRReview: (projectId: string, prNumber: number, reviewId: number) =>
      safeInvoke<unknown>('github_pr_delete_review', { projectId, prNumber, reviewId }),
    mergePR: (projectId: string, prNumber: number) =>
      safeInvoke<unknown>('github_pr_merge', { projectId, prNumber }),
    assignPR: (projectId: string, prNumber: number, assignee: string) =>
      safeInvoke<unknown>('github_pr_assign', { projectId, prNumber, assignee }),
    postPRComment: (projectId: string, prNumber: number, comment: string) =>
      safeInvoke<unknown>('github_pr_post_comment', { projectId, prNumber, comment }),
    markReviewPosted: (projectId: string, prNumber: number) =>
      safeInvoke<unknown>('github_pr_mark_review_posted', { projectId, prNumber }),
    updatePRBranch: (projectId: string, prNumber: number) =>
      safeInvoke<unknown>('github_pr_update_branch', { projectId, prNumber }),
    approveWorkflow: (projectId: string, runId: number) =>
      safeInvoke<unknown>('github_workflow_approve', { projectId, runId }),
    // config / local state
    getAutoFixConfig: (projectId: string) =>
      safeInvoke<unknown>('github_autofix_get_config', { projectId }),
    saveAutoFixConfig: (projectId: string, config: unknown) =>
      safeInvoke<unknown>('github_autofix_save_config', { projectId, config }),
    getTriageConfig: (projectId: string) =>
      safeInvoke<unknown>('github_triage_get_config', { projectId }),
    saveTriageConfig: (projectId: string, config: unknown) =>
      safeInvoke<unknown>('github_triage_save_config', { projectId, config }),
    getTriageResults: (projectId: string) =>
      safeInvoke<unknown>('github_triage_get_results', { projectId }),
    createRelease: (projectId: string, version: string, releaseNotes: string, draft?: boolean) =>
      safeInvoke<unknown>('github_create_release', { projectId, version, releaseNotes, draft }),
    // ai stubs (deferred — python runner)
    runPRReview: (projectId: string, prNumber: number) =>
      safeInvoke<unknown>('github_pr_review', { projectId, prNumber }),
    cancelPRReview: (projectId: string, prNumber: number) =>
      safeInvoke<unknown>('github_pr_review_cancel', { projectId, prNumber }),
    runFollowupReview: (projectId: string, prNumber: number) =>
      safeInvoke<unknown>('github_pr_followup_review', { projectId, prNumber }),
    investigateIssue: (projectId: string, issueNumber: number) =>
      safeInvoke<unknown>('github_investigate_issue', { projectId, issueNumber }),
    startAutoFix: (projectId: string, issueNumber: number) =>
      safeInvoke<unknown>('github_autofix_start', { projectId, issueNumber }),
    stopAutoFix: (projectId: string) =>
      safeInvoke<unknown>('github_autofix_stop', { projectId }),
    getAutoFixQueue: (projectId: string) =>
      safeInvoke<unknown>('github_autofix_get_queue', { projectId }),
    checkNewIssues: (projectId: string) =>
      safeInvoke<unknown>('github_autofix_check_new', { projectId }),
    batchAutoFix: (projectId: string, issueNumbers: number[]) =>
      safeInvoke<unknown>('github_autofix_batch', { projectId, issueNumbers }),
    getBatches: (projectId: string) =>
      safeInvoke<unknown>('github_autofix_get_batches', { projectId }),
    analyzeIssuesPreview: (projectId: string) =>
      safeInvoke<unknown>('github_autofix_analyze_preview', { projectId }),
    approveBatches: (projectId: string, batches: unknown) =>
      safeInvoke<unknown>('github_autofix_approve_batches', { projectId, batches }),
    triageRun: (projectId: string, issueNumbers: number[]) =>
      safeInvoke<unknown>('github_triage_run', { projectId, issueNumbers }),
    triageApplyLabels: (projectId: string, results: unknown) =>
      safeInvoke<unknown>('github_triage_apply_labels', { projectId, results }),
    suggestVersion: (projectId: string) =>
      safeInvoke<unknown>('github_suggest_version', { projectId }),
    startStatusPolling: (projectId: string, prNumbers: number[]) =>
      safeInvoke<unknown>('github_pr_status_poll_start', { projectId, prNumbers }),
    stopStatusPolling: (projectId: string) =>
      safeInvoke<unknown>('github_pr_status_poll_stop', { projectId }),
    getPRMemory: (projectId: string, prNumber: number) =>
      safeInvoke<unknown>('github_pr_memory_get', { projectId, prNumber }),
    searchPRMemory: (projectId: string, query: string) =>
      safeInvoke<unknown>('github_pr_memory_search', { projectId, query }),
    // events
    onGitHubAuthChanged: (callback: (payload: { oldUsername: string | null; newUsername: string }) => void) => {
      const p = listen<{ oldUsername: string | null; newUsername: string }>(
        'github:auth:changed',
        (e) => callback(e.payload),
      );
      return makeUnsubscribe(p);
    },
    // PR review events — payload always includes projectId; renderer callback receives (projectId, data)
    onPRReviewProgress: (callback: (projectId: string, progress: unknown) => void) => {
      const p = listen<{ projectId: string }>('github:pr:review:progress', (e) =>
        callback(e.payload.projectId, e.payload)
      );
      return makeUnsubscribe(p);
    },
    onPRReviewComplete: (callback: (projectId: string, result: unknown) => void) => {
      const p = listen<{ projectId: string }>('github:pr:review:complete', (e) =>
        callback(e.payload.projectId, e.payload)
      );
      return makeUnsubscribe(p);
    },
    onPRReviewError: (callback: (projectId: string, data: unknown) => void) => {
      const p = listen<{ projectId: string }>('github:pr:review:error', (e) =>
        callback(e.payload.projectId, e.payload)
      );
      return makeUnsubscribe(p);
    },
    onPRLogsUpdated: (_cb: unknown) => noopUnsub,
    onPRStatusUpdate: (_cb: unknown) => noopUnsub,
    onAutoFixProgress: (_cb: unknown) => noopUnsub,
    onAutoFixComplete: (_cb: unknown) => noopUnsub,
    onAutoFixError: (_cb: unknown) => noopUnsub,
    onBatchProgress: (_cb: unknown) => noopUnsub,
    onBatchComplete: (_cb: unknown) => noopUnsub,
    onBatchError: (_cb: unknown) => noopUnsub,
    onAnalyzePreviewProgress: (_cb: unknown) => noopUnsub,
    onAnalyzePreviewComplete: (_cb: unknown) => noopUnsub,
    onAnalyzePreviewError: (_cb: unknown) => noopUnsub,
  };

  // Top-level GitHub auth methods (renderer calls window.electronAPI.checkGitHubCli(),
  // not window.electronAPI.github.checkCLI()). Mirrors the Electron preload contract.
  const githubAuthTopLevel = {
    checkGitHubCli: () => safeInvoke<unknown>('github_check_cli'),
    checkGitHubAuth: () => safeInvoke<unknown>('github_check_auth'),
    startGitHubAuth: () => safeInvoke<unknown>('github_start_auth'),
    onGitHubAuthDeviceCode: (
      callback: (data: { deviceCode: string | null; authUrl: string; browserOpened: boolean }) => void
    ) => {
      const p = listen<{ deviceCode: string | null; authUrl: string; browserOpened: boolean }>(
        'github:auth:device-code',
        (e) => callback(e.payload),
      );
      return makeUnsubscribe(p);
    },
  };

  // Shell / OS operations
  const shellAPI = {
    openExternal: (url: string) => invoke<void>('shell_open_external', { url }).catch(() => {}),
    openTerminal: (dirPath: string) => safeInvoke<void>('shell_open_terminal', { dirPath }),
    selectDirectory: () => invoke<string | null>('shell_select_directory').catch(() => null),
    getDefaultProjectLocation: () => invoke<string | null>('shell_get_default_project_location').catch(() => null),
    createProjectFolder: (location: string, name: string, initGit: boolean) =>
      safeInvoke<unknown>('shell_create_project_folder', { location, name, initGit }),
    searchAllProjects: (query: string) =>
      safeInvoke<unknown[]>('shell_search_all_projects', { query }),
  };

  // Git operations
  const gitAPI = {
    getGitBranches: (projectPath: string) =>
      safeInvoke<string[]>('git_get_branches', { projectPath }),
    getGitBranchesWithInfo: (projectPath: string) =>
      safeInvoke<unknown>('git_get_branches_with_info', { projectPath }),
    getCurrentGitBranch: (projectPath: string) =>
      safeInvoke<string | null>('git_get_current_branch', { projectPath }),
    detectMainBranch: (projectPath: string) =>
      safeInvoke<string | null>('git_detect_main_branch', { projectPath }),
    checkGitStatus: (projectPath: string) =>
      safeInvoke<unknown>('git_check_status', { projectPath }),
    initializeGit: (projectPath: string) =>
      safeInvoke<unknown>('git_initialize', { projectPath }),
  };

  // Worktree operations
  const worktreeAPI = {
    getWorktreeStatus: (taskId: string) =>
      safeInvoke<unknown>('worktree_get_status', { taskId }),
    getWorktreeDiff: (taskId: string) =>
      safeInvoke<unknown>('worktree_get_diff', { taskId }),
    mergeWorktree: (taskId: string, options?: unknown) =>
      safeInvoke<unknown>('worktree_merge', { taskId, noCommit: (options as Record<string, unknown>)?.noCommit }),
    mergeWorktreePreview: (taskId: string) =>
      safeInvoke<unknown>('worktree_merge_preview', { taskId }),
    createWorktreePR: (taskId: string, options?: unknown) =>
      safeInvoke<unknown>('worktree_create_pr', { taskId, options }),
    discardWorktree: (taskId: string, skipStatusChange?: boolean) =>
      safeInvoke<unknown>('worktree_discard', { taskId, skipStatusChange }),
    discardOrphanedWorktree: (projectId: string, specName: string) =>
      safeInvoke<unknown>('worktree_discard_orphaned', { projectId, specName }),
    clearStagedState: (taskId: string) =>
      safeInvoke<unknown>('worktree_clear_staged', { taskId }),
    listWorktrees: (projectId: string, options?: unknown) =>
      safeInvoke<unknown>('worktree_list', { projectId, includeStats: (options as Record<string, unknown>)?.includeStats }),
    worktreeOpenInIDE: (worktreePath: string, ide: string, customPath?: string) =>
      safeInvoke<unknown>('worktree_open_in_ide', { worktreePath, ide, customPath }),
    worktreeOpenInTerminal: (worktreePath: string, terminal: string, customPath?: string) =>
      safeInvoke<unknown>('worktree_open_in_terminal', { worktreePath, terminal, customPath }),
    worktreeDetectTools: () =>
      safeInvoke<unknown>('worktree_detect_tools'),
    checkWorktreeChanges: (taskId: string) =>
      safeInvoke<unknown>('worktree_check_changes', { taskId }),
  };

  // Extended task operations
  const taskExtAPI = {
    submitReview: (taskId: string, approved: boolean, feedback?: string, images?: unknown) =>
      safeInvoke<void>('task_submit_review', { taskId, approved, feedback, images }),
    updateTaskStatus: (taskId: string, status: string, options?: unknown) =>
      safeInvoke<unknown>('task_update_status', { taskId, status, options }),
    recoverStuckTask: (taskId: string, options?: unknown) =>
      safeInvoke<unknown>('agent_recover', { taskId, projectPath: (options as Record<string, unknown>)?.projectPath ?? '', specId: taskId }),
    checkTaskRunning: (taskId: string) =>
      safeInvoke<boolean>('agent_check_running', { taskId }),
    resumePausedTask: (taskId: string) =>
      safeInvoke<void>('task_resume_paused', { taskId }),
    refineTaskDescription: (description: string) =>
      safeInvoke<string>('task_refine_description', { description }),
    loadImageThumbnail: (projectPath: string, specId: string, imagePath: string) =>
      safeInvoke<string>('task_load_image_thumbnail', { projectPath, specId, imagePath }),
    getTaskLogs: (projectId: string, specId: string) =>
      safeInvoke<unknown>('task_get_logs', { projectId, specId }),
    watchTaskLogs: (projectId: string, specId: string) =>
      safeInvoke<void>('task_watch_logs', { projectId, specId }),
    unwatchTaskLogs: (specId: string) =>
      safeInvoke<void>('task_unwatch_logs', { specId }),
    autoRecoverAllTasks: (_projectId: string) =>
      Promise.resolve({ success: false, error: 'not_ported' }),
  };

  // Task event listeners — mapped from agent:* events emitted by the Rust backend
  const taskEventAPI = {
    onTaskListRefresh: (callback: (projectId: string) => void) => {
      const p = listen<{ projectId: string; specId: string }>('specs:changed', (e) =>
        callback(e.payload.projectId)
      );
      return makeUnsubscribe(p);
    },
    onTaskAutoRefresh: (_callback: unknown) => () => {},
    onTaskAutoStart: (_callback: unknown) => () => {},
    onTaskStatusChanged: (_callback: unknown) => () => {},
    onTaskRegressionDetected: (_callback: unknown) => () => {},
    onDebugEvent: (_callback: unknown) => () => {},
    onTaskProgress: (_callback: unknown) => () => {},
    onTaskError: (callback: (taskId: string, error: string, projectId?: string) => void) => {
      const p = listen<{ taskId: string; stream: string; data: string }>(
        'agent:output',
        (e) => {
          if (e.payload.stream === 'stderr') {
            callback(e.payload.taskId, e.payload.data);
          }
        }
      );
      return makeUnsubscribe(p);
    },
    onTaskLog: (callback: (taskId: string, log: string, projectId?: string) => void) => {
      const p = listen<{ taskId: string; stream: string; data: string }>(
        'agent:output',
        (e) => callback(e.payload.taskId, e.payload.data)
      );
      return makeUnsubscribe(p);
    },
    onTaskStatusChange: (callback: (taskId: string, status: string, projectId?: string) => void) => {
      const p = listen<{ taskId: string; state: string }>(
        'agent:state',
        (e) => {
          // Map Rust agent state to renderer TaskStatus
          const stateToStatus: Record<string, string> = {
            running: 'in_progress',
            stopped: 'done',
            crashed: 'error',
          };
          const status = stateToStatus[e.payload.state] ?? e.payload.state;
          callback(e.payload.taskId, status);
        }
      );
      return makeUnsubscribe(p);
    },
    onTaskExecutionProgress: (_callback: unknown) => () => {},
    onMergeProgress: (_callback: unknown) => () => {},
    onTaskLogsChanged: (_callback: unknown) => () => {},
    onTaskLogsStream: (_callback: unknown) => () => {},
  };

  // Project extended operations
  const projectExtAPI = {
    getProjectEnv: (projectId: string) =>
      safeInvoke<unknown>('project_env_get', { projectId }),
    updateProjectEnv: (projectId: string, config: unknown) =>
      safeInvoke<void>('project_env_update', { projectId, config }),
    initializeProject: (projectId: string) =>
      safeInvoke<unknown>('project_initialize', { projectId }),
    checkProjectVersion: (projectId: string) =>
      safeInvoke<unknown>('project_check_version', { projectId }),
    onProjectAutomationSettingsChanged: (_callback: unknown) => () => {},
    checkClaudeAuth: (_projectId: string) =>
      safeInvoke<unknown>('settings_get').then(r =>
        ({ success: true, data: { status: 'authenticated', claudeAuth: r.data } })
      ),
    invokeClaudeSetup: (_projectId: string) =>
      safeInvoke<unknown>('settings_get'),
    // Context operations (require Python backend)
    getProjectContext: (_projectId: string) =>
      ({ success: false, error: 'context_not_ported' } as unknown as Promise<unknown>),
    refreshProjectIndex: (_projectId: string) =>
      Promise.resolve({ success: false, error: 'context_not_ported' }),
    getMemoryStatus: (_projectId: string) =>
      Promise.resolve({ success: false, error: 'memory_not_ported' }),
    searchMemories: (_projectId: string, _query: string) =>
      Promise.resolve({ success: false, error: 'memory_not_ported' }),
    getRecentMemories: (_projectId: string) =>
      Promise.resolve({ success: false, error: 'memory_not_ported' }),
    getMemoryInfrastructureStatus: () =>
      Promise.resolve({ success: false, error: 'memory_not_ported' }),
    listMemoryDatabases: () =>
      Promise.resolve({ success: false, error: 'memory_not_ported' }),
    testMemoryConnection: () =>
      Promise.resolve({ success: false, error: 'memory_not_ported' }),
    validateLLMApiKey: () =>
      Promise.resolve({ success: false, error: 'memory_not_ported' }),
    testGraphitiConnection: () =>
      Promise.resolve({ success: false, error: 'memory_not_ported' }),
  };

  // Claude profile management
  const profilesAPI = {
    getClaudeProfiles: () =>
      safeInvoke<unknown>('claude_profiles_get'),
    saveClaudeProfile: (profile: unknown) =>
      safeInvoke<unknown>('claude_profile_save', { profile }),
    deleteClaudeProfile: (profileId: string) =>
      safeInvoke<void>('claude_profile_delete', { profileId }),
    renameClaudeProfile: (profileId: string, newName: string) =>
      safeInvoke<void>('claude_profile_rename', { profileId, newName }),
    setActiveClaudeProfile: (profileId: string) =>
      safeInvoke<void>('claude_profile_set_active', { profileId }),
    switchClaudeProfile: (terminalId: string, profileId: string) =>
      safeInvoke<void>('claude_profile_switch', { terminalId, profileId }),
    initializeClaudeProfile: (profileId: string) =>
      safeInvoke<void>('claude_profile_initialize', { profileId }),
    setClaudeProfileToken: (profileId: string, token: string, email?: string) =>
      safeInvoke<void>('claude_profile_set_token', { profileId, token, email }),
    authenticateClaudeProfile: (profileId: string) =>
      safeInvoke<unknown>('claude_profile_authenticate', { profileId }),
    verifyClaudeProfileAuth: (profileId: string) =>
      safeInvoke<unknown>('claude_profile_verify_auth', { profileId }),
    getAutoSwitchSettings: () =>
      safeInvoke<unknown>('claude_auto_switch_get'),
    updateAutoSwitchSettings: (settings: unknown) =>
      safeInvoke<void>('claude_auto_switch_update', { settings }),
    // Usage monitoring stubs (not ported)
    requestUsageUpdate: () => Promise.resolve({ success: false, data: null }),
    requestAllProfilesUsage: () => Promise.resolve({ success: false, data: null }),
    onUsageUpdated: (_cb: unknown) => () => {},
    onProactiveSwapNotification: (_cb: unknown) => () => {},
    onAllProfilesUsageUpdated: (_cb: unknown) => () => {},
    onSDKRateLimit: (_cb: unknown) => () => {},
    onAuthFailure: (_cb: unknown) => () => {},
    retryWithProfile: () => Promise.resolve({ success: false, error: 'not_ported' }),
    fetchClaudeUsage: () => Promise.resolve({ success: false, error: 'not_ported' }),
    getBestAvailableProfile: () => Promise.resolve({ success: true, data: null }),
    getAccountPriorityOrder: () => Promise.resolve({ success: true, data: [] }),
    setAccountPriorityOrder: () => Promise.resolve({ success: true }),
    // Provider accounts
    saveProviderAccount: () => Promise.resolve({ success: false, error: 'not_ported' }),
    updateProviderAccount: () => Promise.resolve({ success: false, error: 'not_ported' }),
    deleteProviderAccount: () => Promise.resolve({ success: false, error: 'not_ported' }),
    setProviderAccountOrder: () => Promise.resolve({ success: false, error: 'not_ported' }),
    codexAuthLogin: () => Promise.resolve({ success: false, error: 'not_ported' }),
    codexAuthStatus: () => Promise.resolve({ success: false, error: 'not_ported' }),
    codexAuthLogout: () => Promise.resolve({ success: false, error: 'not_ported' }),
  };

  // API Profile management (custom endpoints)
  const apiProfilesAPI = {
    getAPIProfiles: () => safeInvoke<unknown>('settings_get').then(r => ({
      success: r.success,
      data: (r.data as Record<string, unknown>)?.apiProfiles ?? { profiles: [], activeProfileId: null },
    })),
    saveAPIProfile: () => Promise.resolve({ success: false, error: 'not_ported' }),
    updateAPIProfile: () => Promise.resolve({ success: false, error: 'not_ported' }),
    deleteAPIProfile: () => Promise.resolve({ success: false, error: 'not_ported' }),
    setActiveAPIProfile: () => Promise.resolve({ success: false, error: 'not_ported' }),
    testConnection: () => Promise.resolve({ success: false, error: 'not_ported' }),
    discoverModels: () => Promise.resolve({ success: false, error: 'not_ported' }),
  };

  // RDR operations stubs (Windows-specific or Python backend)
  const rdrStubAPI = {
    triggerRdrProcessing: () => Promise.resolve({ success: false, error: 'not_ported' }),
    pingRdrImmediate: () => Promise.resolve({ success: false, error: 'not_ported' }),
    getVSCodeWindows: () => Promise.resolve({ success: true, data: [] }),
    sendRdrToWindow: () => Promise.resolve({ success: false, error: 'not_ported' }),
    sendTestRdrToWindow: () => Promise.resolve({ success: false, error: 'not_ported' }),
    getRdrBatchDetails: () => Promise.resolve({ success: true, data: { batches: [], taskDetails: [] } }),
    isClaudeCodeBusy: () => Promise.resolve({ success: true, data: false }),
    getRdrCooldownStatus: () => Promise.resolve({ success: true, data: { paused: false, reason: '', rateLimitResetAt: 0 } }),
    onRdrRateLimited: (_cb: unknown) => () => {},
    onRdrRateLimitCleared: (_cb: unknown) => () => {},
    onRateLimitAutoResume: (_cb: unknown) => () => {},
    startRateLimitWait: () => Promise.resolve({ success: false, error: 'not_ported' }),
    cancelRateLimitWait: () => Promise.resolve({ success: false, error: 'not_ported' }),
    onRateLimitWaitProgress: (_cb: unknown) => () => {},
    onRateLimitWaitComplete: (_cb: unknown) => () => {},
    getAssignedWindow: () => Promise.resolve({ success: true, data: null }),
    setAssignedWindow: () => Promise.resolve({ success: false, error: 'not_ported' }),
    getAutoShutdownStatus: () => Promise.resolve({ success: true, data: { enabled: false } }),
    setAutoShutdown: () => Promise.resolve({ success: false, error: 'not_ported' }),
    cancelAutoShutdown: () => Promise.resolve({ success: false, error: 'not_ported' }),
  };

  // Terminal extended stubs (session mgmt requires Python backend)
  const terminalExtAPI = {
    invokeClaudeInTerminal: (_id: string, _cwd?: string) => {},
    generateTerminalName: () => Promise.resolve({ success: true, data: 'terminal' }),
    setTerminalTitle: () => {},
    setTerminalWorktreeConfig: () => {},
    getTerminalSessions: () => Promise.resolve({ success: true, data: [] }),
    restoreTerminalSession: () => Promise.resolve({ success: false, error: 'not_ported' }),
    clearTerminalSessions: () => Promise.resolve({ success: true }),
    resumeClaudeInTerminal: () => {},
    activateDeferredClaudeResume: () => {},
    getTerminalSessionDates: () => Promise.resolve({ success: true, data: [] }),
    getTerminalSessionsForDate: () => Promise.resolve({ success: true, data: [] }),
    restoreTerminalSessionsFromDate: () => Promise.resolve({ success: false, error: 'not_ported' }),
    saveTerminalBuffer: () => Promise.resolve(undefined),
    checkTerminalPtyAlive: (terminalId: string) =>
      safeInvoke<boolean>('terminal_check_alive', { id: terminalId }),
    updateTerminalDisplayOrders: () => Promise.resolve({ success: true }),
    createTerminalWorktree: () => Promise.resolve({ success: false, error: 'not_ported' }),
    listTerminalWorktrees: () => Promise.resolve({ success: true, data: [] }),
    removeTerminalWorktree: () => Promise.resolve({ success: false, error: 'not_ported' }),
    listOtherWorktrees: () => Promise.resolve({ success: true, data: [] }),
    onTerminalTitleChange: (_cb: unknown) => () => {},
    onTerminalWorktreeConfigChange: (_cb: unknown) => () => {},
    onTerminalClaudeSession: (_cb: unknown) => () => {},
    onTerminalRateLimit: (_cb: unknown) => () => {},
    onTerminalOAuthToken: (_cb: unknown) => () => {},
    onTerminalAuthCreated: (_cb: unknown) => () => {},
    onTerminalClaudeBusy: (_cb: unknown) => () => {},
    onTerminalClaudeExit: (_cb: unknown) => () => {},
    onTerminalOnboardingComplete: (_cb: unknown) => () => {},
    onTerminalPendingResume: (_cb: unknown) => () => {},
    onTerminalProfileChanged: (_cb: unknown) => () => {},
    onTerminalOAuthCodeNeeded: (_cb: unknown) => () => {},
    submitOAuthCode: () => Promise.resolve({ success: false, error: 'not_ported' }),
  };

  // Features backed by Python backend — return graceful not-ported errors (no console noise)
  const pythonBackendStubAPI = {
    // GitHub top-level (investigation)
    getGitHubRepositories: (projectId: string) =>
      safeInvoke<unknown>('github_get_repositories'),
    getGitHubIssues: (projectId: string, state?: string, page?: number) =>
      safeInvoke<unknown>('github_get_issues', { projectId, state, page }),
    getGitHubIssue: (projectId: string, issueNumber: number) =>
      safeInvoke<unknown>('github_get_issue', { projectId, issueNumber }),
    checkGitHubConnection: (projectId: string) =>
      safeInvoke<unknown>('github_check_connection', { projectId }),
    investigateGitHubIssue: () => {},
    getIssueComments: (projectId: string, issueNumber: number) =>
      safeInvoke<unknown>('github_get_issue_comments', { projectId, issueNumber }),
    importGitHubIssues: (projectId: string, issueNumbers: number[]) =>
      safeInvoke<unknown>('github_import_issues', { projectId, issueNumbers }),
    createGitHubRelease: (projectId: string, version: string, releaseNotes: string, options?: unknown) =>
      safeInvoke<unknown>('github_create_release', { projectId, version, releaseNotes, draft: (options as Record<string, unknown>)?.draft }),
    getGitHubToken: () =>
      safeInvoke<unknown>('github_get_token'),
    getGitHubUser: () =>
      safeInvoke<unknown>('github_get_user'),
    listGitHubUserRepos: () =>
      safeInvoke<unknown>('github_list_user_repos'),
    detectGitHubRepo: (projectPath: string) =>
      safeInvoke<unknown>('github_detect_repo', { projectPath }),
    getGitHubBranches: (projectId: string) =>
      safeInvoke<unknown>('github_get_branches', { projectId }),
    createGitHubRepo: (repoName: string, options?: unknown) =>
      safeInvoke<unknown>('github_create_repo', { repoName, isPrivate: (options as Record<string, unknown>)?.isPrivate }),
    addGitRemote: (projectPath: string, repoFullName: string) =>
      safeInvoke<unknown>('github_add_remote', { projectPath, repoUrl: repoFullName }),
    listGitHubOrgs: () =>
      safeInvoke<unknown>('github_list_orgs'),
    detectHuggingFaceRepo: () =>
      Promise.resolve({ success: false, error: 'not_ported' }),
    checkHuggingFaceCli: () =>
      Promise.resolve({ success: false, error: 'not_ported' }),
    checkHuggingFaceAuth: () =>
      Promise.resolve({ success: false, error: 'not_ported' }),
    getHuggingFaceToken: () =>
      Promise.resolve({ success: false, error: 'not_ported' }),
    huggingFaceLogin: () =>
      Promise.resolve({ success: false, error: 'not_ported' }),
    huggingFaceLoginWithToken: () =>
      Promise.resolve({ success: false, error: 'not_ported' }),
    installHuggingFaceCli: () =>
      Promise.resolve({ success: false, error: 'not_ported' }),
    onGitHubInvestigationProgress: (_cb: unknown) => () => {},
    onGitHubInvestigationComplete: (_cb: unknown) => () => {},
    onGitHubInvestigationError: (_cb: unknown) => () => {},
    // GitLab — not ported
    getGitLabProjects: () => Promise.resolve({ success: false, error: 'gitlab_not_ported' }),
    getGitLabIssues: () => Promise.resolve({ success: false, error: 'gitlab_not_ported' }),
    getGitLabIssue: () => Promise.resolve({ success: false, error: 'gitlab_not_ported' }),
    getGitLabIssueNotes: () => Promise.resolve({ success: false, error: 'gitlab_not_ported' }),
    checkGitLabConnection: () => Promise.resolve({ success: false, error: 'gitlab_not_ported' }),
    investigateGitLabIssue: () => {},
    importGitLabIssues: () => Promise.resolve({ success: false, error: 'gitlab_not_ported' }),
    createGitLabRelease: () => Promise.resolve({ success: false, error: 'gitlab_not_ported' }),
    getGitLabMergeRequests: () => Promise.resolve({ success: false, error: 'gitlab_not_ported' }),
    getGitLabMergeRequest: () => Promise.resolve({ success: false, error: 'gitlab_not_ported' }),
    createGitLabMergeRequest: () => Promise.resolve({ success: false, error: 'gitlab_not_ported' }),
    updateGitLabMergeRequest: () => Promise.resolve({ success: false, error: 'gitlab_not_ported' }),
    getGitLabMRReview: () => Promise.resolve(null),
    runGitLabMRReview: () => {},
    runGitLabMRFollowupReview: () => {},
    postGitLabMRReview: () => Promise.resolve(false),
    postGitLabMRNote: () => Promise.resolve(false),
    mergeGitLabMR: () => Promise.resolve(false),
    assignGitLabMR: () => Promise.resolve(false),
    approveGitLabMR: () => Promise.resolve(false),
    cancelGitLabMRReview: () => Promise.resolve(false),
    checkGitLabMRNewCommits: () => Promise.resolve({ hasNewCommits: false }),
    onGitLabMRReviewProgress: (_cb: unknown) => () => {},
    onGitLabMRReviewComplete: (_cb: unknown) => () => {},
    onGitLabMRReviewError: (_cb: unknown) => () => {},
    checkGitLabCli: () => Promise.resolve({ success: false, error: 'gitlab_not_ported' }),
    installGitLabCli: () => Promise.resolve({ success: false, error: 'gitlab_not_ported' }),
    checkGitLabAuth: () => Promise.resolve({ success: false, error: 'gitlab_not_ported' }),
    startGitLabAuth: () => Promise.resolve({ success: false, error: 'gitlab_not_ported' }),
    getGitLabToken: () => Promise.resolve({ success: false, error: 'gitlab_not_ported' }),
    getGitLabUser: () => Promise.resolve({ success: false, error: 'gitlab_not_ported' }),
    listGitLabUserProjects: () => Promise.resolve({ success: false, error: 'gitlab_not_ported' }),
    detectGitLabProject: () => Promise.resolve({ success: true, data: null }),
    getGitLabBranches: () => Promise.resolve({ success: false, error: 'gitlab_not_ported' }),
    createGitLabProject: () => Promise.resolve({ success: false, error: 'gitlab_not_ported' }),
    addGitLabRemote: () => Promise.resolve({ success: false, error: 'gitlab_not_ported' }),
    listGitLabGroups: () => Promise.resolve({ success: false, error: 'gitlab_not_ported' }),
    onGitLabInvestigationProgress: (_cb: unknown) => () => {},
    onGitLabInvestigationComplete: (_cb: unknown) => () => {},
    onGitLabInvestigationError: (_cb: unknown) => () => {},
    // Linear — not ported
    getLinearTeams: () => Promise.resolve({ success: false, error: 'linear_not_ported' }),
    getLinearProjects: () => Promise.resolve({ success: false, error: 'linear_not_ported' }),
    getLinearIssues: () => Promise.resolve({ success: false, error: 'linear_not_ported' }),
    importLinearIssues: () => Promise.resolve({ success: false, error: 'linear_not_ported' }),
    checkLinearConnection: () => Promise.resolve({ success: false, error: 'linear_not_ported' }),
    // Roadmap — not ported
    getRoadmap: () => Promise.resolve({ success: true, data: null }),
    getRoadmapStatus: () => Promise.resolve({ success: true, data: { isRunning: false } }),
    saveRoadmap: () => Promise.resolve({ success: false, error: 'not_ported' }),
    generateRoadmap: () => {},
    refreshRoadmap: () => {},
    stopRoadmap: () => Promise.resolve({ success: false, error: 'not_ported' }),
    updateFeatureStatus: () => Promise.resolve({ success: false, error: 'not_ported' }),
    convertFeatureToSpec: () => Promise.resolve({ success: false, error: 'not_ported' }),
    saveRoadmapProgress: () => Promise.resolve({ success: true }),
    loadRoadmapProgress: () => Promise.resolve({ success: true, data: null }),
    clearRoadmapProgress: () => Promise.resolve({ success: true }),
    onRoadmapProgress: (_cb: unknown) => () => {},
    onRoadmapComplete: (_cb: unknown) => () => {},
    onRoadmapError: (_cb: unknown) => () => {},
    onRoadmapStopped: (_cb: unknown) => () => {},
    // Ideation — not ported
    getIdeation: () => Promise.resolve({ success: true, data: null }),
    generateIdeation: () => {},
    refreshIdeation: () => {},
    stopIdeation: () => Promise.resolve({ success: false, error: 'not_ported' }),
    updateIdeaStatus: () => Promise.resolve({ success: false, error: 'not_ported' }),
    convertIdeaToTask: () => Promise.resolve({ success: false, error: 'not_ported' }),
    dismissIdea: () => Promise.resolve({ success: false, error: 'not_ported' }),
    dismissAllIdeas: () => Promise.resolve({ success: false, error: 'not_ported' }),
    archiveIdea: () => Promise.resolve({ success: false, error: 'not_ported' }),
    deleteIdea: () => Promise.resolve({ success: false, error: 'not_ported' }),
    deleteMultipleIdeas: () => Promise.resolve({ success: false, error: 'not_ported' }),
    onIdeationProgress: (_cb: unknown) => () => {},
    onIdeationLog: (_cb: unknown) => () => {},
    onIdeationComplete: (_cb: unknown) => () => {},
    onIdeationError: (_cb: unknown) => () => {},
    onIdeationStopped: (_cb: unknown) => () => {},
    onIdeationTypeComplete: (_cb: unknown) => () => {},
    onIdeationTypeFailed: (_cb: unknown) => () => {},
    // Insights — not ported
    getInsightsSession: () => Promise.resolve({ success: true, data: null }),
    sendInsightsMessage: () => {},
    clearInsightsSession: () => Promise.resolve({ success: false, error: 'not_ported' }),
    createTaskFromInsights: () => Promise.resolve({ success: false, error: 'not_ported' }),
    listInsightsSessions: () => Promise.resolve({ success: true, data: [] }),
    newInsightsSession: () => Promise.resolve({ success: false, error: 'not_ported' }),
    switchInsightsSession: () => Promise.resolve({ success: true, data: null }),
    deleteInsightsSession: () => Promise.resolve({ success: false, error: 'not_ported' }),
    renameInsightsSession: () => Promise.resolve({ success: false, error: 'not_ported' }),
    updateInsightsModelConfig: () => Promise.resolve({ success: false, error: 'not_ported' }),
    onInsightsStreamChunk: (_cb: unknown) => () => {},
    onInsightsStatus: (_cb: unknown) => () => {},
    onInsightsError: (_cb: unknown) => () => {},
    onInsightsSessionUpdated: (_cb: unknown) => () => {},
    // Changelog — partially ported (git ops work, generation requires Python)
    getChangelogDoneTasks: () => Promise.resolve({ success: true, data: [] }),
    loadTaskSpecs: () => Promise.resolve({ success: true, data: [] }),
    generateChangelog: () => {},
    saveChangelog: () => Promise.resolve({ success: false, error: 'not_ported' }),
    readExistingChangelog: () => Promise.resolve({ success: true, data: null }),
    suggestChangelogVersion: () => Promise.resolve({ success: false, error: 'not_ported' }),
    suggestChangelogVersionFromCommits: () => Promise.resolve({ success: false, error: 'not_ported' }),
    getChangelogBranches: (projectId: string) => gitAPI.getGitBranchesWithInfo(projectId),
    getChangelogTags: () => Promise.resolve({ success: true, data: [] }),
    getChangelogCommitsPreview: () => Promise.resolve({ success: false, error: 'not_ported' }),
    saveChangelogImage: () => Promise.resolve({ success: false, error: 'not_ported' }),
    readLocalImage: () => Promise.resolve({ success: false, error: 'not_ported' }),
    onChangelogGenerationProgress: (_cb: unknown) => () => {},
    onChangelogGenerationComplete: (_cb: unknown) => () => {},
    onChangelogGenerationError: (_cb: unknown) => () => {},
    // Releases — not ported
    getReleaseableVersions: () => Promise.resolve({ success: true, data: [] }),
    runReleasePreflightCheck: () => Promise.resolve({ success: false, error: 'not_ported' }),
    createRelease: () => {},
    onReleaseProgress: (_cb: unknown) => () => {},
    onReleaseComplete: (_cb: unknown) => () => {},
    onReleaseError: (_cb: unknown) => () => {},
    // App updates — Tauri has own update mechanism
    checkAppUpdate: () => Promise.resolve({ success: true, data: null }),
    downloadAppUpdate: () => Promise.resolve({ success: false, error: 'use_tauri_updater' }),
    downloadStableUpdate: () => Promise.resolve({ success: false, error: 'use_tauri_updater' }),
    installAppUpdate: () => {},
    getDownloadedAppUpdate: () => Promise.resolve({ success: true, data: null }),
    onAppUpdateAvailable: (_cb: unknown) => () => {},
    onAppUpdateDownloaded: (_cb: unknown) => () => {},
    onAppUpdateProgress: (_cb: unknown) => () => {},
    onAppUpdateStableDowngrade: (_cb: unknown) => () => {},
    onAppUpdateReadOnlyVolume: (_cb: unknown) => () => {},
    onAppUpdateError: (_cb: unknown) => () => {},
    // Ollama — not ported
    checkOllamaStatus: () => Promise.resolve({ success: false, error: 'ollama_not_ported' }),
    checkOllamaInstalled: () => Promise.resolve({ success: false, error: 'ollama_not_ported' }),
    installOllama: () => Promise.resolve({ success: false, error: 'ollama_not_ported' }),
    listOllamaModels: () => Promise.resolve({ success: false, error: 'ollama_not_ported' }),
    listOllamaEmbeddingModels: () => Promise.resolve({ success: false, error: 'ollama_not_ported' }),
    pullOllamaModel: () => Promise.resolve({ success: false, error: 'ollama_not_ported' }),
    onDownloadProgress: (_cb: unknown) => () => {},
    // MCP health — not ported
    checkMcpHealth: () => Promise.resolve({ success: false, error: 'not_ported' }),
    testMcpConnection: () => Promise.resolve({ success: false, error: 'not_ported' }),
    // Source env extras
    updateSourceEnv: () => safeInvoke<void>('settings_save', { settings: {} }),
    checkSourceToken: () => safeInvoke<unknown>('autobuild_source_env_get'),
  };

  const implemented: Record<string, unknown> = {
    ...desktopAPI,
    ...settingsAPI,
    ...claudeCodeAPI,
    ...projectAPI,
    ...fileAndDebugAPI,
    ...taskAPI,
    ...watcherAPI,
    ...agentAPI,
    ...terminalAPI,
    ...githubAuthTopLevel,
    ...shellAPI,
    ...gitAPI,
    ...worktreeAPI,
    ...taskExtAPI,
    ...taskEventAPI,
    ...projectExtAPI,
    ...profilesAPI,
    ...apiProfilesAPI,
    ...rdrStubAPI,
    ...terminalExtAPI,
    ...pythonBackendStubAPI,
    github: githubAPI,
    recordActivity: (source: string) => {
      void invoke('activity_record', { source }).catch(() => {});
    },
  };

  // Proxy: implemented methods pass through; everything else returns a stub
  // that won't crash the renderer. Logs each first-time stub access so we can
  // see during dev which un-ported methods the renderer touches at boot.
  const stubbedMethods = new Set<string>();
  const proxy = new Proxy(implemented, {
    get(target, prop, _receiver) {
      const name = String(prop);
      if (name in target) {
        return target[name];
      }
      // Sub-namespace (e.g., window.electronAPI.queue) — recursive proxy
      if (name === 'queue') {
        return new Proxy(
          {},
          {
            get(_t, sub) {
              const subName = String(sub);
              const fullName = `${name}.${subName}`;
              if (!stubbedMethods.has(fullName)) {
                stubbedMethods.add(fullName);
                // eslint-disable-next-line no-console
                console.debug(`[shim] stub: ${fullName}`);
              }
              return makeStub(subName);
            },
          }
        );
      }
      if (!stubbedMethods.has(name)) {
        stubbedMethods.add(name);
        // eslint-disable-next-line no-console
        console.debug(`[shim] stub: ${name}`);
      }
      return makeStub(name);
    },
  });

  window.electronAPI = proxy as unknown as ElectronAPI;

  // Debug surface: raw invoke + listen + a test helper for the DevTools console.
  // Gated on import.meta.env.DEV so Vite tree-shakes the entire block out of
  // production bundles — prod renderers should never have direct invoke access.
  // (Codex Phase 2 round 2.5: closes XSS-to-IPC escalation surface.)
  if (import.meta.env.DEV) {
    (window as unknown as { __tauriDebug: unknown }).__tauriDebug = {
      invoke,
      listen,
      test: async (cmd: string = 'desktop_state_get', args?: Record<string, unknown>) => {
        const t0 = performance.now();
        try {
          const result = await invoke(cmd, args);
          return { ok: true, ms: performance.now() - t0, result };
        } catch (err) {
          return { ok: false, ms: performance.now() - t0, err: String(err) };
        }
      },
    };
  }

  // eslint-disable-next-line no-console
  console.info(
    '[electron-shim] Tauri shim mounted (desktop domain real, rest stubbed). __tauriDebug exposed.'
  );
}

export {};
