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

  const implemented: Record<string, unknown> = {
    ...desktopAPI,
    ...settingsAPI,
    ...claudeCodeAPI,
    ...projectAPI,
    ...fileAndDebugAPI,
    ...taskAPI,
    ...terminalAPI,
    recordActivity: (source: string) => {
      void invoke('activity_record', { source }).catch(() => {
        // Phase 1 spike: activity_record handler not ported yet. Swallow.
      });
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
      // Sub-namespace (e.g., window.electronAPI.github) — recursive proxy
      // The renderer accesses `window.electronAPI.github.getPRReview(...)` etc.
      // Phase 2 will replace these with real per-domain implementations.
      if (name === 'github' || name === 'queue') {
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
