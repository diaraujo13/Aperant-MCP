/**
 * App Update IPC Handlers
 *
 * Handles IPC communication for Electron app auto-updates.
 * Provides manual controls for checking, downloading, and installing updates.
 */

import { app, ipcMain } from 'electron';
import { IPC_CHANNELS } from '../../shared/constants';
import type { IPCResult, AppUpdateInfo } from '../../shared/types';

const APP_UPDATES_DISABLED_MESSAGE = 'App updates are disabled in this build.';

/**
 * Register all app-update-related IPC handlers
 */
export function registerAppUpdateHandlers(): void {
  console.warn('[IPC] Registering app update handlers');

  /**
   * APP_UPDATE_CHECK: Manually check for updates
   * Updates are disabled, so always report no update available.
   */
  ipcMain.handle(
    IPC_CHANNELS.APP_UPDATE_CHECK,
    async (): Promise<IPCResult<AppUpdateInfo | null>> => {
      return { success: true, data: null };
    }
  );

  /**
   * APP_UPDATE_DOWNLOAD: Manually download update
   * Updates are disabled, so reject the request.
   */
  ipcMain.handle(
    IPC_CHANNELS.APP_UPDATE_DOWNLOAD,
    async (): Promise<IPCResult> => {
      return {
        success: false,
        error: APP_UPDATES_DISABLED_MESSAGE,
      };
    }
  );

  /**
   * APP_UPDATE_DOWNLOAD_STABLE: Download stable version (for downgrade from beta)
   * Updates are disabled, so reject the request.
   */
  ipcMain.handle(
    IPC_CHANNELS.APP_UPDATE_DOWNLOAD_STABLE,
    async (): Promise<IPCResult> => {
      return {
        success: false,
        error: APP_UPDATES_DISABLED_MESSAGE,
      };
    }
  );

  /**
   * APP_UPDATE_INSTALL: Quit and install update
   * Updates are disabled, so reject the request.
   */
  ipcMain.handle(
    IPC_CHANNELS.APP_UPDATE_INSTALL,
    async (): Promise<IPCResult> => {
      return {
        success: false,
        error: APP_UPDATES_DISABLED_MESSAGE,
      };
    }
  );

  /**
   * APP_UPDATE_GET_VERSION: Get current app version
   * Returns the current application version
   */
  ipcMain.handle(
    IPC_CHANNELS.APP_UPDATE_GET_VERSION,
    async (): Promise<string> => {
      return app.getVersion();
    }
  );

  /**
   * APP_UPDATE_GET_DOWNLOADED: Get downloaded update info
   * Returns info about a downloaded update that's ready to install,
   * or null if no update has been downloaded yet.
   * This allows the UI to show "Install and Restart" even if the user
   * opens Settings after the download completed in the background.
   */
  ipcMain.handle(
    IPC_CHANNELS.APP_UPDATE_GET_DOWNLOADED,
    async (): Promise<IPCResult<AppUpdateInfo | null>> => {
      return { success: true, data: null };
    }
  );

  console.warn('[IPC] App update handlers registered successfully');
}
