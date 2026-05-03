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

  const implemented: Record<string, unknown> = {
    ...desktopAPI,
    ...settingsAPI,
    ...claudeCodeAPI,
    ...projectAPI,
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
