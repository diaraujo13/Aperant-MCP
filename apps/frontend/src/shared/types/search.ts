export interface GlobalSearchTaskResult {
  id: string;
  type: 'task';
  projectId: string;
  projectName: string;
  taskId: string;
  specId: string;
  title: string;
  status: string;
  snippet: string;
  matchedField: 'title' | 'description' | 'specId' | 'subtask';
  updatedAt: Date;
}

export interface GlobalSearchConversationResult {
  id: string;
  type: 'conversation';
  projectId: string;
  projectName: string;
  sessionId: string;
  sessionTitle: string;
  messageId: string;
  role: 'user' | 'assistant';
  snippet: string;
  updatedAt: Date;
}

export type GlobalSearchResult =
  | GlobalSearchTaskResult
  | GlobalSearchConversationResult;
