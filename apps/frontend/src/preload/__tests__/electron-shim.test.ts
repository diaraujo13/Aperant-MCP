// @vitest-environment jsdom
//
// Tests the Tauri compatibility shim (`electron-shim.ts`):
//   - It only mounts when `__TAURI_INTERNALS__` is present (no-op in Electron).
//   - Implemented methods route to the right Tauri command with the right args.
//   - Un-ported methods return a graceful IPCResult failure (event-style stubs
//     return a no-op unsubscribe).
//
// The shim has module-level side effects, so each test resets modules and
// rebuilds the window to get a clean mount.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const invokeMock = vi.fn();
const listenMock = vi.fn();

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: (...args: unknown[]) => listenMock(...args),
}));

async function mountShim(): Promise<void> {
  vi.resetModules();
  // Activate the Tauri branch in the shim.
  (window as unknown as { __TAURI_INTERNALS__: unknown }).__TAURI_INTERNALS__ = {};
  // Provide a deterministic UA so the platform branch is stable.
  Object.defineProperty(navigator, 'userAgent', {
    value: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) AppleWebKit/605.1.15',
    configurable: true,
  });
  await import('../electron-shim');
}

describe('electron-shim (Tauri)', () => {
  beforeEach(() => {
    invokeMock.mockReset();
    listenMock.mockReset();
    listenMock.mockResolvedValue(() => {});
    delete (window as unknown as { electronAPI?: unknown }).electronAPI;
    delete (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
    delete (window as unknown as { platform?: unknown }).platform;
  });

  afterEach(() => {
    delete (window as unknown as { electronAPI?: unknown }).electronAPI;
    delete (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  });

  it('mounts window.electronAPI when __TAURI_INTERNALS__ is present', async () => {
    await mountShim();
    expect(window.electronAPI).toBeDefined();
    expect(typeof window.electronAPI).toBe('object');
  });

  it('does not mount when __TAURI_INTERNALS__ is absent (Electron path)', async () => {
    vi.resetModules();
    // No __TAURI_INTERNALS__ → shim should be a no-op.
    await import('../electron-shim');
    expect((window as unknown as { electronAPI?: unknown }).electronAPI).toBeUndefined();
  });

  it('detects platform from userAgent', async () => {
    await mountShim();
    expect(window.platform).toEqual({
      isWindows: false,
      isMacOS: true,
      isLinux: false,
      isUnix: true,
    });
  });

  it('routes desktop_state_get through invoke', async () => {
    invokeMock.mockResolvedValueOnce({ success: true, data: { pinned: false, projects: [] } });
    await mountShim();
    const api = window.electronAPI as unknown as {
      getDesktopState: () => Promise<unknown>;
    };
    const result = await api.getDesktopState();
    expect(invokeMock).toHaveBeenCalledWith('desktop_state_get', undefined);
    expect(result).toEqual({ success: true, data: { pinned: false, projects: [] } });
  });

  it('passes args through to invoke for desktop_pin_set', async () => {
    invokeMock.mockResolvedValueOnce({ success: true, data: { pinned: true } });
    await mountShim();
    const api = window.electronAPI as unknown as {
      setDesktopPinEnabled: (enabled: boolean) => Promise<unknown>;
    };
    await api.setDesktopPinEnabled(true);
    expect(invokeMock).toHaveBeenCalledWith('desktop_pin_set', { enabled: true });
  });

  it('wraps thrown invoke errors into IPCResult failure', async () => {
    invokeMock.mockRejectedValueOnce(new Error('rust panicked'));
    await mountShim();
    const api = window.electronAPI as unknown as {
      getDesktopState: () => Promise<{ success: boolean; error?: string }>;
    };
    const result = await api.getDesktopState();
    expect(result.success).toBe(false);
    expect(result.error).toBe('rust panicked');
  });

  it('subscribes via listen() and returns unsubscribe fn for onDesktopStateChanged', async () => {
    const unlisten = vi.fn();
    listenMock.mockResolvedValueOnce(unlisten);
    await mountShim();
    const api = window.electronAPI as unknown as {
      onDesktopStateChanged: (cb: (s: unknown) => void) => () => void;
    };
    const unsubscribe = api.onDesktopStateChanged(() => {});
    expect(listenMock).toHaveBeenCalledWith('desktop:state:changed', expect.any(Function));
    // Invoke the unsubscribe and let the listen promise resolve.
    unsubscribe();
    await Promise.resolve();
    await Promise.resolve();
    expect(unlisten).toHaveBeenCalled();
  });

  it('returns IPCResult failure for un-ported async methods (default stub)', async () => {
    await mountShim();
    const api = window.electronAPI as unknown as {
      getSomeUnportedThing: () => Promise<{ success: boolean; error?: string }>;
    };
    const result = await api.getSomeUnportedThing();
    expect(result.success).toBe(false);
    expect(result.error).toMatch(/not implemented/);
  });

  it('returns no-op unsubscribe for un-ported event subscribers (on* stub)', async () => {
    await mountShim();
    const api = window.electronAPI as unknown as {
      onSomeUnportedEvent: (cb: () => void) => () => void;
    };
    const unsubscribe = api.onSomeUnportedEvent(() => {});
    expect(typeof unsubscribe).toBe('function');
    expect(() => unsubscribe()).not.toThrow();
  });

  it('routes settings_get through invoke', async () => {
    invokeMock.mockResolvedValueOnce({ success: true, data: { theme: 'dusk' } });
    await mountShim();
    const api = window.electronAPI as unknown as {
      getSettings: () => Promise<unknown>;
    };
    await api.getSettings();
    expect(invokeMock).toHaveBeenCalledWith('settings_get', undefined);
  });

  it('returns app version via raw invoke (not IPCResult-wrapped)', async () => {
    invokeMock.mockResolvedValueOnce('2.7.6-beta.5');
    await mountShim();
    const api = window.electronAPI as unknown as {
      getAppVersion: () => Promise<string>;
    };
    const version = await api.getAppVersion();
    expect(version).toBe('2.7.6-beta.5');
  });

  it('falls back to "0.0.0" when app_version invoke throws', async () => {
    invokeMock.mockRejectedValueOnce(new Error('command not found'));
    await mountShim();
    const api = window.electronAPI as unknown as {
      getAppVersion: () => Promise<string>;
    };
    expect(await api.getAppVersion()).toBe('0.0.0');
  });
});
