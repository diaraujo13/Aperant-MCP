import { create } from 'zustand';
import type {
  ReviewComment,
  ReviewFilePatch,
  TriageReport,
  TriageDecision,
  FinalizeReviewResult,
} from '../../shared/types';

/**
 * Per-task inline review state. Keyed by taskId so the dialog can outlive
 * remounts of TaskDetailModal without losing the comments / triage report.
 */
interface PerTaskState {
  comments: ReviewComment[];
  patches: Record<string, ReviewFilePatch>; // file -> patch
  triageReport: TriageReport | null;
  // User overrides on top of the AI verdict; keyed by comment id.
  overriddenDecisions: Record<string, TriageDecision>;
  loadingComments: boolean;
  triageInFlight: boolean;
}

interface ReviewCommentsState {
  byTask: Record<string, PerTaskState>;

  loadComments: (taskId: string) => Promise<void>;
  loadFilePatch: (taskId: string, file: string, force?: boolean) => Promise<ReviewFilePatch | null>;
  invalidatePatches: (taskId: string) => void;
  addComment: (
    taskId: string,
    input: { file: string; line: number; side: 'LEFT' | 'RIGHT'; body: string }
  ) => Promise<ReviewComment | null>;
  deleteComment: (taskId: string, commentId: string) => Promise<boolean>;
  runTriage: (taskId: string) => Promise<TriageReport | null>;
  overrideDecision: (taskId: string, decision: TriageDecision) => void;
  applyTriage: (taskId: string) => Promise<FinalizeReviewResult | null>;
  reset: (taskId: string) => void;
}

const empty: PerTaskState = {
  comments: [],
  patches: {},
  triageReport: null,
  overriddenDecisions: {},
  loadingComments: false,
  triageInFlight: false,
};

function ensure(state: ReviewCommentsState, taskId: string): PerTaskState {
  return state.byTask[taskId] ?? empty;
}

// Narrow window typing so renderer can call the preload API without a global d.ts edit.
type ReviewAPI = {
  listReviewComments: (taskId: string) => Promise<{ success: boolean; data?: ReviewComment[]; error?: string }>;
  getReviewFilePatch: (
    taskId: string,
    file: string
  ) => Promise<{ success: boolean; data?: ReviewFilePatch; error?: string }>;
  addReviewComment: (
    taskId: string,
    input: { file: string; line: number; side: 'LEFT' | 'RIGHT'; body: string }
  ) => Promise<{ success: boolean; data?: ReviewComment; error?: string }>;
  deleteReviewComment: (taskId: string, commentId: string) => Promise<{ success: boolean; error?: string }>;
  finalizeReviewTriage: (
    taskId: string
  ) => Promise<{ success: boolean; data?: TriageReport; error?: string }>;
  finalizeReviewApply: (
    taskId: string,
    decisions: TriageDecision[]
  ) => Promise<{ success: boolean; data?: FinalizeReviewResult; error?: string }>;
};

function api(): ReviewAPI {
  // electronAPI is exposed via contextBridge / Tauri shim.
  return (window as unknown as { electronAPI: ReviewAPI }).electronAPI;
}

export const useReviewCommentsStore = create<ReviewCommentsState>((set, get) => ({
  byTask: {},

  loadComments: async (taskId) => {
    set((s) => ({ byTask: { ...s.byTask, [taskId]: { ...ensure(s, taskId), loadingComments: true } } }));
    try {
      const res = await api().listReviewComments(taskId);
      const comments = res.success && res.data ? res.data : [];
      set((s) => ({
        byTask: { ...s.byTask, [taskId]: { ...ensure(s, taskId), comments, loadingComments: false } },
      }));
    } catch (err) {
      console.error('[review-store] loadComments failed:', err);
      set((s) => ({ byTask: { ...s.byTask, [taskId]: { ...ensure(s, taskId), loadingComments: false } } }));
    }
  },

  loadFilePatch: async (taskId, file, force = false) => {
    const cached = ensure(get(), taskId).patches[file];
    if (cached && !force) return cached;
    const res = await api().getReviewFilePatch(taskId, file);
    if (!res.success || !res.data) return null;
    set((s) => {
      const current = ensure(s, taskId);
      return {
        byTask: {
          ...s.byTask,
          [taskId]: { ...current, patches: { ...current.patches, [file]: res.data as ReviewFilePatch } },
        },
      };
    });
    return res.data;
  },

  invalidatePatches: (taskId) => {
    set((s) => {
      const current = s.byTask[taskId];
      if (!current) return s;
      return { byTask: { ...s.byTask, [taskId]: { ...current, patches: {} } } };
    });
  },

  addComment: async (taskId, input) => {
    const res = await api().addReviewComment(taskId, input);
    if (!res.success || !res.data) return null;
    set((s) => {
      const current = ensure(s, taskId);
      return {
        byTask: {
          ...s.byTask,
          [taskId]: { ...current, comments: [...current.comments, res.data as ReviewComment] },
        },
      };
    });
    return res.data;
  },

  deleteComment: async (taskId, commentId) => {
    const res = await api().deleteReviewComment(taskId, commentId);
    if (!res.success) return false;
    set((s) => {
      const current = ensure(s, taskId);
      return {
        byTask: {
          ...s.byTask,
          [taskId]: { ...current, comments: current.comments.filter((c) => c.id !== commentId) },
        },
      };
    });
    return true;
  },

  runTriage: async (taskId) => {
    set((s) => ({ byTask: { ...s.byTask, [taskId]: { ...ensure(s, taskId), triageInFlight: true } } }));
    try {
      const res = await api().finalizeReviewTriage(taskId);
      const report = res.success && res.data ? res.data : null;
      set((s) => {
        const current = ensure(s, taskId);
        const overrides: Record<string, TriageDecision> = {};
        if (report) {
          for (const d of report.decisions) overrides[d.commentId] = d;
        }
        return {
          byTask: {
            ...s.byTask,
            [taskId]: { ...current, triageReport: report, overriddenDecisions: overrides, triageInFlight: false },
          },
        };
      });
      return report;
    } catch (err) {
      console.error('[review-store] runTriage failed:', err);
      set((s) => ({ byTask: { ...s.byTask, [taskId]: { ...ensure(s, taskId), triageInFlight: false } } }));
      return null;
    }
  },

  overrideDecision: (taskId, decision) => {
    set((s) => {
      const current = ensure(s, taskId);
      return {
        byTask: {
          ...s.byTask,
          [taskId]: {
            ...current,
            overriddenDecisions: { ...current.overriddenDecisions, [decision.commentId]: decision },
          },
        },
      };
    });
  },

  applyTriage: async (taskId) => {
    const state = ensure(get(), taskId);
    const decisions = Object.values(state.overriddenDecisions);
    if (decisions.length === 0) return null;
    const res = await api().finalizeReviewApply(taskId, decisions);
    if (!res.success || !res.data) return null;
    // After apply the redo comments become resolved; refresh list.
    await get().loadComments(taskId);
    set((s) => ({
      byTask: {
        ...s.byTask,
        [taskId]: { ...ensure(s, taskId), triageReport: null, overriddenDecisions: {} },
      },
    }));
    return res.data;
  },

  reset: (taskId) => {
    set((s) => {
      const next = { ...s.byTask };
      delete next[taskId];
      return { byTask: next };
    });
  },
}));
