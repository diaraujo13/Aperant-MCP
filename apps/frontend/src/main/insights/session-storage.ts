import { existsSync, readFileSync, writeFileSync, mkdirSync, readdirSync, unlinkSync } from 'fs';
import { readdir, readFile } from 'fs/promises';
import path from 'path';
import type { InsightsSession, InsightsSessionSummary } from '../../shared/types';
import { InsightsPaths } from './paths';

/**
 * Session storage manager
 * Handles persisting and loading sessions from the filesystem
 */
export class SessionStorage {
  private paths: InsightsPaths;

  constructor(paths: InsightsPaths) {
    this.paths = paths;
  }

  /**
   * Generate a title from the first user message
   */
  generateTitle(message: string): string {
    // Truncate to first 50 characters and clean up
    const title = message.trim().replace(/\n/g, ' ').slice(0, 50);
    return title.length < message.trim().length ? `${title}...` : title;
  }

  /**
   * Load a specific session from disk
   */
  loadSessionById(projectPath: string, sessionId: string): InsightsSession | null {
    const sessionPath = this.paths.getSessionPath(projectPath, sessionId);
    if (!existsSync(sessionPath)) return null;

    try {
      const content = readFileSync(sessionPath, 'utf-8');
      const session = JSON.parse(content) as InsightsSession;
      // Convert date strings back to Date objects
      session.createdAt = new Date(session.createdAt);
      session.updatedAt = new Date(session.updatedAt);
      session.messages = session.messages.map(m => ({
        ...m,
        timestamp: new Date(m.timestamp),
        // Convert toolsUsed timestamps if present
        toolsUsed: m.toolsUsed?.map(t => ({
          ...t,
          timestamp: new Date(t.timestamp)
        }))
      }));
      return session;
    } catch {
      return null;
    }
  }

  /**
   * Save session to disk
   */
  saveSession(projectPath: string, session: InsightsSession): void {
    const sessionsDir = this.paths.getSessionsDir(projectPath);
    if (!existsSync(sessionsDir)) {
      mkdirSync(sessionsDir, { recursive: true });
    }

    const sessionPath = this.paths.getSessionPath(projectPath, session.id);
    writeFileSync(sessionPath, JSON.stringify(session, null, 2), 'utf-8');
  }

  /**
   * Delete a session from disk
   */
  deleteSession(projectPath: string, sessionId: string): boolean {
    const sessionPath = this.paths.getSessionPath(projectPath, sessionId);
    if (!existsSync(sessionPath)) return false;

    try {
      unlinkSync(sessionPath);
      return true;
    } catch {
      return false;
    }
  }

  /**
   * List all sessions for a project
   */
  listSessions(projectPath: string): InsightsSessionSummary[] {
    const sessionsDir = this.paths.getSessionsDir(projectPath);
    if (!existsSync(sessionsDir)) return [];

    try {
      const files = readdirSync(sessionsDir).filter(f => f.endsWith('.json'));
      const sessions: InsightsSessionSummary[] = [];

      for (const file of files) {
        try {
          const content = readFileSync(path.join(sessionsDir, file), 'utf-8');
          const session = JSON.parse(content) as InsightsSession;

          // Generate title if not present
          let title = session.title;
          if (!title && session.messages.length > 0) {
            const firstUserMessage = session.messages.find(m => m.role === 'user');
            title = firstUserMessage
              ? this.generateTitle(firstUserMessage.content)
              : 'Untitled Conversation';
          }

          sessions.push({
            id: session.id,
            projectId: session.projectId,
            title: title || 'New Conversation',
            messageCount: session.messages.length,
            createdAt: new Date(session.createdAt),
            updatedAt: new Date(session.updatedAt)
          });
        } catch {
          // Skip invalid session files
        }
      }

      // Sort by updatedAt descending (most recent first)
      return sessions.sort((a, b) =>
        new Date(b.updatedAt).getTime() - new Date(a.updatedAt).getTime()
      );
    } catch {
      return [];
    }
  }

  async searchSessions(projectPath: string, query: string): Promise<Array<{
    sessionId: string;
    sessionTitle: string;
    messageId: string;
    role: 'user' | 'assistant';
    snippet: string;
    updatedAt: Date;
  }>> {
    const normalizedQuery = query.trim().toLowerCase();
    if (!normalizedQuery) return [];

    const tokens = normalizedQuery.split(/\s+/).filter(Boolean);
    const sessionsDir = this.paths.getSessionsDir(projectPath);
    if (!existsSync(sessionsDir)) return [];

    const matches: Array<{
      sessionId: string;
      sessionTitle: string;
      messageId: string;
      role: 'user' | 'assistant';
      snippet: string;
      updatedAt: Date;
    }> = [];

    try {
      const files = (await readdir(sessionsDir)).filter(f => f.endsWith('.json'));

      for (const file of files) {
        try {
          const content = await readFile(path.join(sessionsDir, file), 'utf-8');
          const session = JSON.parse(content) as InsightsSession;
          const sessionTitle = session.title || 'New Conversation';
          const updatedAt = new Date(session.updatedAt);

          const normalizedTitle = sessionTitle.toLowerCase();
          const titleMatchesAll = tokens.every((token) => normalizedTitle.includes(token));

          let pushedFromContent = false;
          for (const message of session.messages || []) {
            const rawContent = typeof message.content === 'string' ? message.content : '';
            const normalizedContent = rawContent.toLowerCase();
            const contentMatchesAll = tokens.every((token) => normalizedContent.includes(token));

            if (!contentMatchesAll) {
              continue;
            }

            matches.push({
              sessionId: session.id,
              sessionTitle,
              messageId: message.id,
              role: message.role === 'assistant' ? 'assistant' : 'user',
              snippet: this.buildSnippet(rawContent, tokens),
              updatedAt
            });
            pushedFromContent = true;
          }

          if (titleMatchesAll && !pushedFromContent) {
            const firstMessage = session.messages?.[0];
            if (firstMessage) {
              matches.push({
                sessionId: session.id,
                sessionTitle,
                messageId: firstMessage.id,
                role: firstMessage.role === 'assistant' ? 'assistant' : 'user',
                snippet: this.buildSnippet(sessionTitle, tokens),
                updatedAt
              });
            }
          }
        } catch {
          // Skip invalid session files
        }
      }
    } catch {
      return [];
    }

    return matches.sort((a, b) => b.updatedAt.getTime() - a.updatedAt.getTime());
  }

  /**
   * Get current session ID for a project
   */
  getCurrentSessionId(projectPath: string): string | null {
    const currentPath = this.paths.getCurrentSessionPath(projectPath);
    if (!existsSync(currentPath)) return null;

    try {
      const content = readFileSync(currentPath, 'utf-8');
      const data = JSON.parse(content);
      return data.currentSessionId || null;
    } catch {
      return null;
    }
  }

  /**
   * Save current session ID pointer
   */
  saveCurrentSessionId(projectPath: string, sessionId: string): void {
    const insightsDir = this.paths.getInsightsDir(projectPath);
    if (!existsSync(insightsDir)) {
      mkdirSync(insightsDir, { recursive: true });
    }

    const currentPath = this.paths.getCurrentSessionPath(projectPath);
    writeFileSync(currentPath, JSON.stringify({ currentSessionId: sessionId }, null, 2), 'utf-8');
  }

  /**
   * Clear current session pointer
   */
  clearCurrentSessionId(projectPath: string): void {
    const currentPath = this.paths.getCurrentSessionPath(projectPath);
    if (existsSync(currentPath)) {
      unlinkSync(currentPath);
    }
  }

  /**
   * Migrate old session format to new multi-session format
   */
  migrateOldSession(projectPath: string): void {
    const oldSessionPath = this.paths.getOldSessionPath(projectPath);
    if (!existsSync(oldSessionPath)) return;

    try {
      const content = readFileSync(oldSessionPath, 'utf-8');
      const oldSession = JSON.parse(content) as InsightsSession;

      // Only migrate if it has messages
      if (oldSession.messages && oldSession.messages.length > 0) {
        // Ensure sessions directory exists
        const sessionsDir = this.paths.getSessionsDir(projectPath);
        if (!existsSync(sessionsDir)) {
          mkdirSync(sessionsDir, { recursive: true });
        }

        // Generate title from first user message
        const firstUserMessage = oldSession.messages.find(m => m.role === 'user');
        const title = firstUserMessage
          ? this.generateTitle(firstUserMessage.content)
          : 'Imported Conversation';

        // Create new session with title
        const newSession: InsightsSession = {
          ...oldSession,
          title
        };

        // Save as new session file
        this.saveSession(projectPath, newSession);

        // Set as current session
        this.saveCurrentSessionId(projectPath, oldSession.id);
      }

      // Remove old session file
      unlinkSync(oldSessionPath);
    } catch {
      // Ignore migration errors
    }
  }

  private buildSnippet(text: string, tokens: string[]): string {
    const trimmed = text.replace(/\s+/g, ' ').trim();
    if (!trimmed) {
      return '';
    }

    const normalized = trimmed.toLowerCase();
    const firstToken = tokens.find((token) => normalized.includes(token)) || tokens[0];
    const matchIndex = normalized.indexOf(firstToken);
    if (matchIndex === -1) {
      return trimmed.slice(0, 160);
    }

    const start = Math.max(0, matchIndex - 60);
    const end = Math.min(trimmed.length, matchIndex + 120);
    const prefix = start > 0 ? '…' : '';
    const suffix = end < trimmed.length ? '…' : '';
    return `${prefix}${trimmed.slice(start, end)}${suffix}`;
  }
}
