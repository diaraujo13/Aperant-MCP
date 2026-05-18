import { Component, useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ErrorInfo, ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { FileCode, MessageSquarePlus, Trash2, Sparkles, AlertTriangle, RefreshCw, ExternalLink } from 'lucide-react';
import { useReviewCommentsStore } from '../../../stores/review-comments-store';
import { parseUnifiedDiff } from '../../../lib/unified-diff';
import { cn } from '../../../lib/utils';
import { Button } from '../../ui/button';
import { Badge } from '../../ui/badge';
import { Textarea } from '../../ui/textarea';
import type {
  WorktreeDiff,
  ReviewComment,
  TriageDecision,
  TriageVerdict,
  FinalizeReviewResult,
} from '../../../../shared/types';

interface InlineReviewProps {
  taskId: string;
  worktreeDiff: WorktreeDiff | null;
  /** Called after the user confirms triage application; lets parent close the modal etc. */
  onFinalized?: (result: FinalizeReviewResult) => void;
}

/**
 * GitHub-style inline code review tab. Wrapped in an internal ErrorBoundary so
 * any runtime crash inside the review surface shows an error message instead
 * of blanking the whole modal — the review tab is new enough that crashing
 * silently would be confusing in production.
 */
export function InlineReview(props: InlineReviewProps) {
  return (
    <InlineReviewBoundary taskId={props.taskId}>
      <InlineReviewInner {...props} />
    </InlineReviewBoundary>
  );
}

// Narrow window typing — preload exposes more, but we only need these two here.
type WindowAPI = {
  electronAPI: {
    getWorktreeDiff: (id: string) => Promise<{ success: boolean; data?: WorktreeDiff; error?: string }>;
    getWorktreeStatus: (id: string) => Promise<{ success: boolean; data?: { worktreePath?: string; exists?: boolean }; error?: string }>;
    worktreeOpenInIDE: (worktreePath: string, ide: string, customPath?: string) => Promise<{ success: boolean; error?: string }>;
  };
};

const POLL_INTERVAL_MS = 5000;

function InlineReviewInner({ taskId, worktreeDiff, onFinalized }: InlineReviewProps) {
  const { t } = useTranslation(['tasks', 'common']);
  // Subscribe selectively to avoid re-render on unrelated state changes.
  const state = useReviewCommentsStore((s) => s.byTask[taskId]);
  const loadComments = useReviewCommentsStore((s) => s.loadComments);
  const loadFilePatch = useReviewCommentsStore((s) => s.loadFilePatch);
  const invalidatePatches = useReviewCommentsStore((s) => s.invalidatePatches);
  const addComment = useReviewCommentsStore((s) => s.addComment);
  const deleteComment = useReviewCommentsStore((s) => s.deleteComment);
  const runTriage = useReviewCommentsStore((s) => s.runTriage);
  const overrideDecision = useReviewCommentsStore((s) => s.overrideDecision);
  const applyTriage = useReviewCommentsStore((s) => s.applyTriage);

  // Self-loaded diff (when parent hook hasn't filled it) and worktree path
  // (needed for "Open in VS Code"). Both refresh on a short polling cadence so
  // the user sees git changes from the agent in near-real-time.
  const [selfDiff, setSelfDiff] = useState<WorktreeDiff | null>(null);
  const [worktreePath, setWorktreePath] = useState<string | null>(null);
  const [diffError, setDiffError] = useState<string | null>(null);
  const [isRefreshing, setIsRefreshing] = useState(false);
  const [lastRefreshAt, setLastRefreshAt] = useState<number>(Date.now());

  // Guard against overlapping refreshes — important when git on the worktree
  // is slow and the 5s interval would otherwise pile up subprocesses.
  const inFlightRef = useRef(false);
  const refresh = useCallback(
    async (manual = false) => {
      if (inFlightRef.current && !manual) return;
      inFlightRef.current = true;
      if (manual) setIsRefreshing(true);
      try {
        const api = (window as unknown as WindowAPI).electronAPI;
        const [diffRes, statusRes] = await Promise.all([
          api.getWorktreeDiff(taskId),
          api.getWorktreeStatus(taskId),
        ]);
        if (diffRes.success && diffRes.data) {
          setSelfDiff(diffRes.data);
          setDiffError(null);
        } else if (diffRes.error) {
          setDiffError(diffRes.error);
        }
        // Clear stale worktreePath when worktree disappears — otherwise
        // "Open in VS Code" targets a deleted directory.
        if (statusRes.success) {
          setWorktreePath(statusRes.data?.worktreePath ?? null);
        }
        invalidatePatches(taskId);
        setLastRefreshAt(Date.now());
      } catch (err) {
        setDiffError(err instanceof Error ? err.message : String(err));
      } finally {
        inFlightRef.current = false;
        if (manual) setIsRefreshing(false);
      }
    },
    [taskId, invalidatePatches]
  );

  // Initial load + reload when taskId changes.
  useEffect(() => {
    void refresh();
  }, [refresh]);

  // Background poll — keeps the diff fresh while the user reviews. The
  // interval is pause-aware: when the document tab is hidden (user switched
  // apps), we skip polling to avoid burning git processes.
  const intervalRef = useRef<number | null>(null);
  useEffect(() => {
    intervalRef.current = window.setInterval(() => {
      if (document.visibilityState === 'visible') void refresh();
    }, POLL_INTERVAL_MS);
    return () => {
      if (intervalRef.current !== null) window.clearInterval(intervalRef.current);
    };
  }, [refresh]);

  const effectiveDiff = (worktreeDiff && Array.isArray(worktreeDiff.files)) ? worktreeDiff : selfDiff;
  const files = Array.isArray(effectiveDiff?.files) ? effectiveDiff!.files : [];
  // null until the first effective diff arrives — useEffect below picks the
  // first file as soon as one becomes available.
  const [selectedFile, setSelectedFile] = useState<string | null>(null);

  // Reload the currently selected file's patch whenever a refresh invalidates
  // the cache — without this effect, the diff pane shows "Loading…" forever
  // after the first poll because invalidatePatches() cleared the cache and
  // the only loader effect depends on selectedFile alone (which didn't change).
  useEffect(() => {
    if (selectedFile) void loadFilePatch(taskId, selectedFile, true);
  }, [lastRefreshAt, selectedFile, taskId, loadFilePatch]);

  const handleOpenInEditor = useCallback(async () => {
    if (!worktreePath) return;
    const api = (window as unknown as WindowAPI).electronAPI;
    await api.worktreeOpenInIDE(worktreePath, 'vscode');
  }, [worktreePath]);
  const [composer, setComposer] = useState<{ line: number; side: 'LEFT' | 'RIGHT'; draft: string } | null>(null);
  const [triageOpen, setTriageOpen] = useState(false);
  const [applying, setApplying] = useState(false);

  useEffect(() => {
    void loadComments(taskId);
  }, [taskId, loadComments]);

  useEffect(() => {
    if (!selectedFile && files.length > 0) setSelectedFile(files[0].path);
  }, [files, selectedFile]);

  useEffect(() => {
    if (selectedFile) void loadFilePatch(taskId, selectedFile);
  }, [selectedFile, taskId, loadFilePatch]);

  const patch = selectedFile ? state?.patches?.[selectedFile] : undefined;
  const parsed = useMemo(() => {
    try {
      return patch ? parseUnifiedDiff(patch.patch ?? '') : null;
    } catch (err) {
      console.error('[InlineReview] parseUnifiedDiff failed:', err);
      return null;
    }
  }, [patch]);

  const allComments: ReviewComment[] = state?.comments ?? [];
  const commentsForFile = allComments.filter((c) => c.file === selectedFile && c.status === 'open');
  const openCount = allComments.filter((c) => c.status === 'open').length;

  const handleAdd = async () => {
    if (!composer || !selectedFile || !composer.draft.trim()) return;
    await addComment(taskId, {
      file: selectedFile,
      line: composer.line,
      side: composer.side,
      body: composer.draft,
    });
    setComposer(null);
  };

  const handleDone = async () => {
    setTriageOpen(true);
    await runTriage(taskId);
  };

  const handleConfirm = async () => {
    setApplying(true);
    try {
      const result = await applyTriage(taskId);
      if (result) onFinalized?.(result);
      setTriageOpen(false);
    } finally {
      setApplying(false);
    }
  };

  // Empty state — surface explicit messaging if the worktree diff couldn't load
  // (Tauri command quirks, missing worktree, etc.).
  if (files.length === 0) {
    return (
      <div className="p-6 space-y-3">
        <div className="text-sm text-muted-foreground">
          {t('tasks:inlineReview.noFiles', 'No changed files')}
        </div>
        <div className="text-xs text-muted-foreground">
          {effectiveDiff?.summary
            || diffError
            || t('tasks:inlineReview.diffUnavailable', 'Diff data not available for this task. The worktree may have been merged or discarded.')}
        </div>
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full min-h-[400px]">
      <div className="flex flex-1 min-h-0 border rounded-md overflow-hidden">
        {/* File list */}
        <div className="w-56 shrink-0 border-r overflow-y-auto bg-secondary/20">
          <ul className="divide-y">
            {files.map((f) => {
              const fileComments = allComments.filter(
                (c) => c.file === f.path && c.status === 'open'
              ).length;
              return (
                <li key={f.path}>
                  <button
                    type="button"
                    onClick={() => setSelectedFile(f.path)}
                    className={cn(
                      'w-full text-left px-3 py-2 text-xs hover:bg-secondary/50 flex items-center gap-2',
                      selectedFile === f.path && 'bg-secondary/70 font-medium'
                    )}
                  >
                    <FileCode className="h-3 w-3 shrink-0" />
                    <span className="truncate flex-1">{f.path}</span>
                    {fileComments > 0 && (
                      <Badge variant="secondary" className="h-4 text-[10px] px-1">
                        {fileComments}
                      </Badge>
                    )}
                  </button>
                </li>
              );
            })}
          </ul>
        </div>

        {/* Diff body */}
        <div className="flex-1 overflow-auto bg-background">
          {!selectedFile ? (
            <div className="p-6 text-sm text-muted-foreground">
              {t('tasks:inlineReview.selectFile', 'Select a file to view the diff.')}
            </div>
          ) : !patch ? (
            <div className="p-6 text-sm text-muted-foreground">
              {t('common:loading', 'Loading…')}
            </div>
          ) : !parsed || parsed.hunks.length === 0 ? (
            <div className="p-6 text-sm text-muted-foreground">
              {t('tasks:inlineReview.noHunks', 'No diff content for this file.')}
            </div>
          ) : (
            <div className="font-mono text-xs">
              {parsed.hunks.map((hunk, hi) => (
                <div key={hi} className="border-b last:border-b-0">
                  <div className="px-2 py-1 bg-info/10 text-info text-[11px] select-none">
                    {hunk.header}
                  </div>
                  {hunk.lines.map((line, li) => {
                    const targetLine = line.newLine ?? line.oldLine ?? 0;
                    const side: 'LEFT' | 'RIGHT' = line.kind === 'del' ? 'LEFT' : 'RIGHT';
                    const lineComments = commentsForFile.filter(
                      (c) => c.line === targetLine && c.side === side
                    );
                    return (
                      <div key={li}>
                        <div
                          className={cn(
                            'group flex items-stretch hover:bg-secondary/30',
                            line.kind === 'add' && 'bg-success/10',
                            line.kind === 'del' && 'bg-destructive/10'
                          )}
                        >
                          <div className="w-10 px-1 text-right text-muted-foreground select-none border-r">
                            {line.oldLine ?? ''}
                          </div>
                          <div className="w-10 px-1 text-right text-muted-foreground select-none border-r">
                            {line.newLine ?? ''}
                          </div>
                          <div className="w-6 text-center select-none">
                            {line.kind === 'add' ? '+' : line.kind === 'del' ? '-' : ' '}
                          </div>
                          <pre className="flex-1 whitespace-pre overflow-x-auto px-2">
                            {line.content}
                          </pre>
                          {targetLine > 0 && (
                            <button
                              type="button"
                              onClick={() => setComposer({ line: targetLine, side, draft: '' })}
                              className="opacity-0 group-hover:opacity-100 transition-opacity px-2 text-info"
                              title={t('tasks:inlineReview.addComment', 'Add comment')}
                            >
                              <MessageSquarePlus className="h-3 w-3" />
                            </button>
                          )}
                        </div>
                        {composer && composer.line === targetLine && composer.side === side && (
                          <div className="p-2 bg-secondary/40 border-y">
                            <Textarea
                              value={composer.draft}
                              onChange={(e) =>
                                setComposer({ ...composer, draft: e.target.value })
                              }
                              placeholder={t(
                                'tasks:inlineReview.commentPlaceholder',
                                'Leave a review comment for the AI to triage…'
                              )}
                              className="text-xs font-sans"
                              rows={3}
                            />
                            <div className="flex justify-end gap-2 mt-2">
                              <Button size="sm" variant="ghost" onClick={() => setComposer(null)}>
                                {t('common:cancel', 'Cancel')}
                              </Button>
                              <Button size="sm" onClick={handleAdd} disabled={!composer.draft.trim()}>
                                {t('tasks:inlineReview.commentSubmit', 'Add comment')}
                              </Button>
                            </div>
                          </div>
                        )}
                        {lineComments.map((c) => (
                          <div
                            key={c.id}
                            className="px-3 py-2 bg-info/5 border-y text-xs font-sans flex items-start gap-2"
                          >
                            <div className="flex-1 whitespace-pre-wrap">{c.body}</div>
                            <button
                              type="button"
                              onClick={() => void deleteComment(taskId, c.id)}
                              className="text-muted-foreground hover:text-destructive"
                              title={t('common:delete', 'Delete')}
                            >
                              <Trash2 className="h-3 w-3" />
                            </button>
                          </div>
                        ))}
                      </div>
                    );
                  })}
                </div>
              ))}
            </div>
          )}
        </div>
      </div>

      <div className="flex justify-between items-center pt-3 gap-2 flex-wrap">
        <div className="text-xs text-muted-foreground flex items-center gap-2">
          {openCount > 0
            ? t('tasks:inlineReview.openComments', { count: openCount, defaultValue: '{{count}} open comment(s)' })
            : t('tasks:inlineReview.noComments', 'No comments yet')}
          <span className="opacity-60">·</span>
          <span className="opacity-60">
            {t('tasks:inlineReview.refreshedAt', 'Refreshed')} {new Date(lastRefreshAt).toLocaleTimeString()}
          </span>
        </div>
        <div className="flex items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() => void refresh(true)}
            disabled={isRefreshing}
            className="gap-1"
            title={t('tasks:inlineReview.refresh', 'Refresh diff')}
          >
            <RefreshCw className={cn('h-3.5 w-3.5', isRefreshing && 'animate-spin')} />
            {t('tasks:inlineReview.refresh', 'Refresh')}
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={() => void handleOpenInEditor()}
            disabled={!worktreePath}
            className="gap-1"
            title={worktreePath || t('tasks:inlineReview.noWorktree', 'No worktree available')}
          >
            <ExternalLink className="h-3.5 w-3.5" />
            {t('tasks:inlineReview.openInVSCode', 'Open in VS Code')}
          </Button>
          <Button onClick={handleDone} className="gap-2">
            <Sparkles className="h-4 w-4" />
            {t('tasks:inlineReview.doneReview', 'Done Review')}
          </Button>
        </div>
      </div>

      {triageOpen && (
        <TriageDialog
          taskId={taskId}
          onClose={() => setTriageOpen(false)}
          onConfirm={handleConfirm}
          applying={applying}
          overrideDecision={overrideDecision}
        />
      )}
    </div>
  );
}

interface TriageDialogProps {
  taskId: string;
  onClose: () => void;
  onConfirm: () => Promise<void> | void;
  applying: boolean;
  overrideDecision: (taskId: string, decision: TriageDecision) => void;
}

function TriageDialog({ taskId, onClose, onConfirm, applying, overrideDecision }: TriageDialogProps) {
  const { t } = useTranslation(['tasks', 'common']);
  const state = useReviewCommentsStore((s) => s.byTask[taskId]);
  const inFlight = state?.triageInFlight;
  const report = state?.triageReport;
  const overrides = state?.overriddenDecisions ?? {};
  const commentMap = new Map((state?.comments ?? []).map((c) => [c.id, c]));

  // ESC closes the dialog — small but expected affordance for keyboard users.
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && !applying) onClose();
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [onClose, applying]);

  return (
    <div
      className="fixed inset-0 z-50 bg-black/50 flex items-center justify-center p-6"
      onClick={onClose}
      role="presentation"
    >
      <div
        className="bg-background border rounded-lg max-w-3xl w-full max-h-[80vh] overflow-hidden flex flex-col"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label={t('tasks:inlineReview.triageTitle', 'AI Review Triage')}
      >
        <div className="p-4 border-b">
          <h3 className="font-semibold flex items-center gap-2">
            <Sparkles className="h-4 w-4 text-info" />
            {t('tasks:inlineReview.triageTitle', 'AI Review Triage')}
          </h3>
          <p className="text-xs text-muted-foreground mt-1">
            {t(
              'tasks:inlineReview.triageDescription',
              'The AI classified each comment. Override any verdict before applying.'
            )}
          </p>
        </div>

        <div className="flex-1 overflow-auto p-4 space-y-3">
          {inFlight ? (
            <div className="text-sm text-muted-foreground py-8 text-center">
              {t('tasks:inlineReview.triageRunning', 'Running classifier…')}
            </div>
          ) : !report || report.decisions.length === 0 ? (
            <div className="text-sm text-muted-foreground py-8 text-center">
              {report?.summary || t('tasks:inlineReview.noDecisions', 'No comments to triage.')}
            </div>
          ) : (
            report.decisions.map((d) => {
              const comment = commentMap.get(d.commentId);
              const current = overrides[d.commentId] ?? d;
              return (
                <div key={d.commentId} className="border rounded-md p-3 text-xs space-y-2">
                  {comment && (
                    <div className="text-muted-foreground">
                      <span className="font-mono">
                        {comment.file}:{comment.line}
                      </span>
                      <div className="mt-1 whitespace-pre-wrap text-foreground">{comment.body}</div>
                    </div>
                  )}
                  <div className="flex items-center gap-2">
                    <label className="text-muted-foreground">
                      {t('tasks:inlineReview.verdict', 'Verdict')}:
                    </label>
                    <select
                      value={current.verdict}
                      onChange={(e) =>
                        overrideDecision(taskId, {
                          ...current,
                          verdict: e.target.value as TriageVerdict,
                        })
                      }
                      className="text-xs border rounded px-2 py-1 bg-background"
                    >
                      <option value="redo">{t('tasks:inlineReview.verdictRedo', 'Redo')}</option>
                      <option value="follow_up">
                        {t('tasks:inlineReview.verdictFollowUp', 'Follow-up task')}
                      </option>
                      <option value="wontfix">{t('tasks:inlineReview.verdictWontfix', 'Wontfix')}</option>
                    </select>
                  </div>
                  {current.verdict === 'follow_up' && (
                    <input
                      type="text"
                      value={current.followUpTitle ?? ''}
                      onChange={(e) =>
                        overrideDecision(taskId, { ...current, followUpTitle: e.target.value })
                      }
                      placeholder={t(
                        'tasks:inlineReview.followUpTitlePlaceholder',
                        'Follow-up task title'
                      )}
                      className="w-full text-xs border rounded px-2 py-1 bg-background"
                    />
                  )}
                  {d.rationale && <div className="text-muted-foreground italic">{d.rationale}</div>}
                </div>
              );
            })
          )}
        </div>

        <div className="p-4 border-t flex justify-end gap-2">
          <Button variant="ghost" onClick={onClose} disabled={applying}>
            {t('common:cancel', 'Cancel')}
          </Button>
          <Button
            onClick={() => void onConfirm()}
            disabled={applying || inFlight || !report || report.decisions.length === 0}
          >
            {applying
              ? t('tasks:inlineReview.applying', 'Applying…')
              : t('tasks:inlineReview.applyDecisions', 'Apply decisions')}
          </Button>
        </div>
      </div>
    </div>
  );
}

// ─── Error boundary ──────────────────────────────────────────────────────────

interface BoundaryProps {
  taskId: string;
  children: ReactNode;
}

interface BoundaryState {
  error: Error | null;
}

class InlineReviewBoundary extends Component<BoundaryProps, BoundaryState> {
  state: BoundaryState = { error: null };

  static getDerivedStateFromError(error: Error): BoundaryState {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    // eslint-disable-next-line no-console
    console.error('[InlineReview] crashed:', error, info);
  }

  componentDidUpdate(prevProps: BoundaryProps): void {
    // Reset on task change so a new task isn't permanently broken because the
    // previous one crashed.
    if (prevProps.taskId !== this.props.taskId && this.state.error) {
      this.setState({ error: null });
    }
  }

  render(): ReactNode {
    if (this.state.error) {
      return (
        <div className="p-6 space-y-3">
          <div className="flex items-center gap-2 text-destructive">
            <AlertTriangle className="h-4 w-4" />
            <span className="font-medium text-sm">Inline review crashed</span>
          </div>
          <pre className="text-xs whitespace-pre-wrap bg-secondary/40 p-3 rounded border overflow-auto max-h-64">
            {this.state.error.message}
            {this.state.error.stack ? `\n\n${this.state.error.stack}` : ''}
          </pre>
          <p className="text-xs text-muted-foreground">
            Check DevTools console for full trace. Switch to another tab and back to retry.
          </p>
        </div>
      );
    }
    return this.props.children;
  }
}
