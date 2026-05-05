/**
 * Platform-agnostic RDR Message Sender
 *
 * Provides configurable message sending to Master LLM (Claude Code, Cursor, etc.)
 * Supports custom command templates with variable substitution or platform-specific defaults.
 */

import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { exec, execFile } from 'child_process';
import { RDR_PLATFORM_DEFAULT_TEMPLATE } from '../../shared/constants/config';
import { isMacOS, isWindows } from './index';
import { getWindowsSafeTempDir, runWindowsPowerShell } from './windows/powershell-runner';

export interface SendMessageResult {
  success: boolean;
  error?: string;
}

const DEFAULT_MACOS_APP_CANDIDATES = [
  'Visual Studio Code',
  'Cursor',
  'Windsurf',
  'Terminal',
  'iTerm',
  'Warp',
  'Ghostty',
  'WezTerm',
  'Hyper',
  'Tabby',
  'Alacritty',
];

const MACOS_TERMINAL_APP_NAMES = new Set([
  'Terminal',
  'iTerm',
  'Warp',
  'Ghostty',
  'WezTerm',
  'Hyper',
  'Tabby',
  'Alacritty',
]);

/**
 * Send RDR message to Master LLM using custom template or platform default
 *
 * @param identifier - Window identifier (PID or title pattern)
 * @param message - RDR message to send
 * @param customTemplate - Optional custom command template with {{variables}}
 * @returns Promise with success/error result
 */
export async function sendRdrMessage(
  identifier: string | number,
  message: string,
  customTemplate?: string
): Promise<SendMessageResult> {
  if (!message) {
    return { success: false, error: 'Message cannot be empty' };
  }

  // Write message to temp file (always, for security and compatibility)
  const tempDir = isWindows() ? getWindowsSafeTempDir() : os.tmpdir();
  const messagePath = path.join(tempDir, `rdr-message-${Date.now()}.txt`);
  let scriptPath: string | null = null;

  try {
    await fs.promises.writeFile(messagePath, message, 'utf8');

    const normalizedTemplate = customTemplate?.trim() ?? '';
    const usePlatformDefaultTemplate = normalizedTemplate === RDR_PLATFORM_DEFAULT_TEMPLATE;
    const useCustomTemplate =
      normalizedTemplate !== '' &&
      !usePlatformDefaultTemplate &&
      isTemplateCompatibleWithCurrentPlatform(normalizedTemplate);

    if (normalizedTemplate !== '' && !usePlatformDefaultTemplate && !useCustomTemplate) {
      console.warn('[RDR Sender] Ignoring incompatible RDR mechanism for current platform; using platform default instead');
    }

    // If custom template provided, use it
    if (useCustomTemplate) {
      // Detect script type
      const trimmedTemplate = normalizedTemplate;
      const isPowerShellScript = isPowerShellTemplate(trimmedTemplate);
      const isShellScript = isShellScriptTemplate(trimmedTemplate);
      const isBatchScript = isBatchScriptTemplate(trimmedTemplate);

      if (isPowerShellScript) {
        // PowerShell script (.ps1) - Windows
        scriptPath = path.join(tempDir, `rdr-script-${Date.now()}.ps1`);

        // Substitute variables in the script
        const scriptContent = substituteVariables(trimmedTemplate, {
          message: escapeForShell(message),
          messagePath: messagePath.replace(/\\/g, '\\\\'), // Escape backslashes for PowerShell
          identifier: identifier.toString(),
          scriptPath: scriptPath
        });

        // Write script to file
        await fs.promises.writeFile(scriptPath, scriptContent, 'utf8');

        // Execute script through the shared safe Windows PowerShell runner
        console.log('[RDR Sender] Using PowerShell script template');
        const runnerResult = await runWindowsPowerShell({
          script: scriptContent,
          timeoutMs: 10000,
          mode: 'file',
          scriptPath,
        });

        // Clean up temp files
        await fs.promises.unlink(messagePath).catch(() => {});
        await fs.promises.unlink(scriptPath).catch(() => {});

        return runnerResult.ok
          ? { success: true }
          : { success: false, error: runnerResult.stderr || 'PowerShell script failed' };
      } else if (isShellScript) {
        // Shell script (.sh) - macOS/Linux
        scriptPath = path.join(tempDir, `rdr-script-${Date.now()}.sh`);

        // Substitute variables in the script
        const scriptContent = substituteVariables(trimmedTemplate, {
          message: escapeForShell(message),
          messagePath,
          identifier: identifier.toString(),
          scriptPath: scriptPath
        });

        // Write script to file
        await fs.promises.writeFile(scriptPath, scriptContent, 'utf8');

        // Make script executable
        await fs.promises.chmod(scriptPath, 0o755);

        // Execute script
        const command = `/bin/bash "${scriptPath}"`;
        console.log('[RDR Sender] Using shell script template');
        const result = await executeCommand(command);

        // Clean up temp files
        await fs.promises.unlink(messagePath).catch(() => {});
        await fs.promises.unlink(scriptPath).catch(() => {});

        return result;
      } else if (isBatchScript) {
        // Batch script (.bat) - Windows
        scriptPath = path.join(tempDir, `rdr-script-${Date.now()}.bat`);

        // Substitute variables in the script
        const scriptContent = substituteVariables(trimmedTemplate, {
          message: escapeForShell(message),
          messagePath,
          identifier: identifier.toString(),
          scriptPath: scriptPath
        });

        // Write script to file
        await fs.promises.writeFile(scriptPath, scriptContent, 'utf8');

        // Execute script
        const command = `cmd.exe /c "${scriptPath}"`;
        console.log('[RDR Sender] Using batch script template');
        const result = await executeCommand(command);

        // Clean up temp files
        await fs.promises.unlink(messagePath).catch(() => {});
        await fs.promises.unlink(scriptPath).catch(() => {});

        return result;
      } else {
        // It's a regular command template
        const command = substituteVariables(trimmedTemplate, {
          message: escapeForShell(message),
          messagePath,
          identifier: identifier.toString(),
          scriptPath: scriptPath || ''
        });

        console.log('[RDR Sender] Using command template:', trimmedTemplate);
        const result = await executeCommand(command);

        // Clean up temp file
        await fs.promises.unlink(messagePath).catch(() => {});

        return result;
      }
    }

    // Otherwise, use platform-specific default
    console.log('[RDR Sender] Using platform default');
    const result = await sendWithPlatformDefault(identifier, message, messagePath);

    // Clean up temp file (platform default may have already cleaned it)
    await fs.promises.unlink(messagePath).catch(() => {});

    return result;
  } catch (error) {
    // Clean up temp files on error
    await fs.promises.unlink(messagePath).catch(() => {});
    if (scriptPath) {
      await fs.promises.unlink(scriptPath).catch(() => {});
    }

    const errorMessage = error instanceof Error ? error.message : String(error);
    console.error('[RDR Sender] Error sending message:', errorMessage);
    return { success: false, error: errorMessage };
  }
}

/**
 * Send message using platform-specific default method
 */
async function sendWithPlatformDefault(
  identifier: string | number,
  message: string,
  messagePath: string
): Promise<SendMessageResult> {
  if (isWindows()) {
    // Use Windows PowerShell clipboard method (existing implementation)
    const { sendMessageToWindow } = await import('./windows/window-manager');
    return sendMessageToWindow(identifier, message);
  }

  if (isMacOS()) {
    console.log('[RDR Sender] macOS default: osascript clipboard paste');
    return sendToMacOSClaudeCode(identifier, messagePath);
  } else {
    // Linux/other Unix: retain the existing ccli fallback
    const template = 'ccli --message "$(cat \'{{messagePath}}\')"';
    const command = substituteVariables(template, {
      message: '',
      messagePath,
      identifier: identifier.toString(),
      scriptPath: ''
    });

    console.log('[RDR Sender] Unix default: ccli command');
    return executeCommand(command);
  }
}

function isPowerShellTemplate(template: string): boolean {
  const isMultiLine = template.includes('\n');
  return isMultiLine && (template.startsWith('$') || template.includes('$ProgressPreference'));
}

function isShellScriptTemplate(template: string): boolean {
  const isMultiLine = template.includes('\n');
  return isMultiLine && (template.startsWith('#!/bin/bash') || template.startsWith('#!/bin/sh') || template.startsWith('#!'));
}

function isBatchScriptTemplate(template: string): boolean {
  return template.includes('\n') && template.startsWith('@echo');
}

function isTemplateCompatibleWithCurrentPlatform(template: string): boolean {
  if (isWindows()) {
    return true;
  }

  return !isPowerShellTemplate(template) && !isBatchScriptTemplate(template);
}

function getMacOSAppCandidates(identifier: string | number): string[] {
  if (typeof identifier !== 'string') {
    return DEFAULT_MACOS_APP_CANDIDATES;
  }

  const normalizedIdentifier = identifier.trim().toLowerCase();
  if (!normalizedIdentifier) {
    return DEFAULT_MACOS_APP_CANDIDATES;
  }

  const prioritized = DEFAULT_MACOS_APP_CANDIDATES.filter((appName) =>
    appName.toLowerCase().includes(normalizedIdentifier) ||
    normalizedIdentifier.includes(appName.toLowerCase())
  );

  const deduped = new Set<string>([...prioritized, ...DEFAULT_MACOS_APP_CANDIDATES]);
  return [...deduped];
}

function escapeAppleScriptString(value: string): string {
  return value
    .replace(/\\/g, '\\\\')
    .replace(/"/g, '\\"');
}

function isMacOSTerminalApp(appName: string): boolean {
  return MACOS_TERMINAL_APP_NAMES.has(appName);
}

async function sendToMacOSClaudeCode(
  identifier: string | number,
  messagePath: string,
): Promise<SendMessageResult> {
  const appNames = getMacOSAppCandidates(identifier);
  const appNamesLiteral = appNames
    .map((appName) => `"${escapeAppleScriptString(appName)}"`)
    .join(', ');

  const script = `
set messagePath to "${escapeAppleScriptString(messagePath)}"
set appNames to {${appNamesLiteral}}

do shell script "/usr/bin/pbcopy < " & quoted form of messagePath

set activatedApp to ""
tell application "System Events"
  repeat with appName in appNames
    if exists application process (contents of appName) then
      set activatedApp to contents of appName
      exit repeat
    end if
  end repeat
end tell

if activatedApp is "" then
  repeat with appName in appNames
    try
      tell application (contents of appName) to activate
      set activatedApp to contents of appName
      exit repeat
    end try
  end repeat
else
  tell application activatedApp to activate
end if

if activatedApp is "" then
  error "No supported Claude Code host app found. Open Claude Code in VS Code, Cursor, Windsurf, Terminal, iTerm, Warp, or Ghostty."
end if

delay 0.2

tell application "System Events"
  keystroke "v" using command down
  if activatedApp is not "Terminal" and activatedApp is not "iTerm" and activatedApp is not "Warp" and activatedApp is not "Ghostty" and activatedApp is not "WezTerm" and activatedApp is not "Hyper" and activatedApp is not "Tabby" and activatedApp is not "Alacritty" then
    delay 0.1
    key code 36
  end if
end tell

return activatedApp
  `.trim();

  const result = await executeFileCommand('/usr/bin/osascript', ['-e', script]);
  if (!result.success) {
    return {
      success: false,
      error: normalizeMacOSAutomationError(result.error),
    };
  }

  const activatedApp = result.output || 'unknown app';
  if (activatedApp !== 'unknown app' && isMacOSTerminalApp(activatedApp)) {
    console.log('[RDR Sender] macOS message pasted into terminal host without auto-submit:', activatedApp);
  } else {
    console.log('[RDR Sender] macOS message pasted and submitted in:', activatedApp);
  }
  return { success: true };
}

/**
 * Substitute template variables with actual values
 *
 * Supported variables:
 * - {{message}} - Escaped message text
 * - {{messagePath}} - Absolute path to temp file with message
 * - {{identifier}} - Window identifier (PID or title)
 * - {{scriptPath}} - Path to generated script (Windows only)
 */
function substituteVariables(
  template: string,
  vars: Record<string, string>
): string {
  return template.replace(/\{\{(\w+)\}\}/g, (match, key) => {
    return vars[key] ?? match;
  });
}

/**
 * Escape message for shell command line
 * Handles quotes, newlines, and special characters
 */
function escapeForShell(message: string): string {
  // Escape single quotes by replacing ' with '\''
  // This works in bash: 'can'\''t' becomes "can't"
  return message.replace(/'/g, "'\\''");
}

/**
 * Execute shell command and return result
 */
function executeCommand(command: string): Promise<SendMessageResult> {
  return new Promise((resolve) => {
    console.log('[RDR Sender] Executing command:', command);

    exec(
      command,
      {
        timeout: 10000,
        windowsHide: true
      },
      (error, stdout, stderr) => {
        if (error) {
          console.error('[RDR Sender] Command failed:', error.message);
          console.error('[RDR Sender] stderr:', stderr);
          resolve({
            success: false,
            error: stderr || error.message
          });
        } else {
          console.log('[RDR Sender] Command succeeded');
          console.log('[RDR Sender] stdout:', stdout);
          resolve({ success: true });
        }
      }
    );
  });
}

function executeFileCommand(
  file: string,
  args: string[],
): Promise<SendMessageResult & { output?: string }> {
  return new Promise((resolve) => {
    execFile(
      file,
      args,
      {
        timeout: 10000,
        windowsHide: true,
      },
      (error, stdout, stderr) => {
        if (error) {
          console.error('[RDR Sender] Command failed:', error.message);
          console.error('[RDR Sender] stderr:', stderr);
          resolve({
            success: false,
            error: stderr || error.message,
          });
          return;
        }

        resolve({
          success: true,
          output: stdout.trim(),
        });
      }
    );
  });
}

function normalizeMacOSAutomationError(error?: string): string {
  const fallbackMessage = 'macOS direct-send failed.';
  if (!error) {
    return `${fallbackMessage} Enable Accessibility and Automation permissions, then retry.`;
  }

  const normalizedError = error.toLowerCase();

  if (
    normalizedError.includes('not authorized') ||
    normalizedError.includes('not authorised') ||
    normalizedError.includes('assistive access') ||
    normalizedError.includes('1743') ||
    normalizedError.includes('1002')
  ) {
    return `${error.trim()} Enable Auto-Claude in System Settings > Privacy & Security > Accessibility, and allow Automation access to control System Events / your editor or terminal.`;
  }

  if (normalizedError.includes('no supported claude code host app found')) {
    return `${error.trim()} Open Claude Code in VS Code, Cursor, Windsurf, Terminal, iTerm, Warp, Ghostty, WezTerm, Hyper, Tabby, or Alacritty, then retry.`;
  }

  return error;
}
