/**
 * Inline code review types
 *
 * Per-line comments on a task's diff (GitHub-style) and the AI triage that
 * decides whether each comment requires re-doing work on the same task or
 * spawning a follow-up task.
 *
 * Persisted at: .auto-claude/specs/{specId}/review_comments.json
 */

export type ReviewCommentSide = 'LEFT' | 'RIGHT';

export type ReviewCommentStatus = 'open' | 'resolved' | 'outdated';

export interface ReviewComment {
  id: string;
  file: string;
  /** 1-indexed line number in the new file (RIGHT side) or old file (LEFT side). */
  line: number;
  side: ReviewCommentSide;
  body: string;
  status: ReviewCommentStatus;
  createdAt: string;
  updatedAt?: string;
  /** Optional author label (defaults to "human" — placeholder for future multi-reviewer). */
  author?: string;
}

export interface ReviewCommentsFile {
  comments: ReviewComment[];
  version: 1;
}

export type TriageVerdict = 'redo' | 'follow_up' | 'wontfix';

export interface TriageDecision {
  commentId: string;
  verdict: TriageVerdict;
  rationale: string;
  /** Suggested title when verdict is `follow_up`. Empty otherwise. */
  followUpTitle?: string;
  /** Suggested description when verdict is `follow_up`. */
  followUpDescription?: string;
}

export interface TriageReport {
  decisions: TriageDecision[];
  /** Free-form summary the classifier produced — shown in the confirmation modal. */
  summary?: string;
}

/** Patch returned by TASK_REVIEW_FILE_PATCH — raw unified-diff text. */
export interface ReviewFilePatch {
  file: string;
  patch: string;
  /** True when the patch was truncated due to size limits. */
  truncated?: boolean;
}

/** Result of TASK_FINALIZE_REVIEW_APPLY. */
export interface FinalizeReviewResult {
  redoCount: number;
  followUpCount: number;
  wontfixCount: number;
  /** Spec IDs of follow-up tasks created (if any). */
  createdSpecIds: string[];
}
