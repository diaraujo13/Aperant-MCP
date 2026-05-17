import { beforeEach, describe, expect, it, vi } from 'vitest';
import { useReviewCommentsStore } from '../review-comments-store';
import type { ReviewComment, TriageReport } from '../../../shared/types';

function installApi(overrides: Record<string, (...a: unknown[]) => unknown>) {
  const api = {
    listReviewComments: vi.fn(async () => ({ success: true, data: [] })),
    getReviewFilePatch: vi.fn(async () => ({ success: true, data: { file: 'x', patch: '' } })),
    addReviewComment: vi.fn(),
    deleteReviewComment: vi.fn(),
    finalizeReviewTriage: vi.fn(),
    finalizeReviewApply: vi.fn(),
    ...overrides,
  };
  (globalThis as unknown as { window: { electronAPI: unknown } }).window = { electronAPI: api };
}

const sampleComment: ReviewComment = {
  id: 'c1',
  file: 'src/foo.ts',
  line: 10,
  side: 'RIGHT',
  body: 'looks fishy',
  status: 'open',
  createdAt: new Date().toISOString(),
};

describe('useReviewCommentsStore', () => {
  beforeEach(() => {
    useReviewCommentsStore.setState({ byTask: {} });
  });

  it('loads comments and stores them by taskId', async () => {
    installApi({
      listReviewComments: vi.fn(async () => ({ success: true, data: [sampleComment] })),
    });

    await useReviewCommentsStore.getState().loadComments('task-1');
    expect(useReviewCommentsStore.getState().byTask['task-1'].comments).toHaveLength(1);
  });

  it('adds a comment and appends to the cache', async () => {
    installApi({
      listReviewComments: vi.fn(async () => ({ success: true, data: [] })),
      addReviewComment: vi.fn(async () => ({ success: true, data: sampleComment })),
    });

    await useReviewCommentsStore.getState().loadComments('task-1');
    const added = await useReviewCommentsStore
      .getState()
      .addComment('task-1', { file: 'src/foo.ts', line: 10, side: 'RIGHT', body: 'looks fishy' });
    expect(added).toEqual(sampleComment);
    expect(useReviewCommentsStore.getState().byTask['task-1'].comments).toHaveLength(1);
  });

  it('seeds overrides from the triage report', async () => {
    const report: TriageReport = {
      decisions: [
        { commentId: 'c1', verdict: 'follow_up', rationale: 'out of scope', followUpTitle: 'add X' },
      ],
      summary: 'one follow-up',
    };
    installApi({
      listReviewComments: vi.fn(async () => ({ success: true, data: [sampleComment] })),
      finalizeReviewTriage: vi.fn(async () => ({ success: true, data: report })),
    });

    await useReviewCommentsStore.getState().loadComments('task-1');
    const result = await useReviewCommentsStore.getState().runTriage('task-1');
    expect(result).toEqual(report);
    const overrides = useReviewCommentsStore.getState().byTask['task-1'].overriddenDecisions;
    expect(overrides['c1'].verdict).toBe('follow_up');
  });

  it('applyTriage sends current overrides and resets state', async () => {
    const apply = vi.fn(async () => ({
      success: true,
      data: { redoCount: 0, followUpCount: 1, wontfixCount: 0, createdSpecIds: ['002-add-x'] },
    }));
    installApi({
      listReviewComments: vi.fn(async () => ({ success: true, data: [sampleComment] })),
      finalizeReviewApply: apply,
    });
    await useReviewCommentsStore.getState().loadComments('task-1');
    useReviewCommentsStore.getState().overrideDecision('task-1', {
      commentId: 'c1',
      verdict: 'follow_up',
      rationale: 'scope',
      followUpTitle: 'add X',
    });
    const result = await useReviewCommentsStore.getState().applyTriage('task-1');
    expect(result?.createdSpecIds).toEqual(['002-add-x']);
    expect(apply).toHaveBeenCalledOnce();
    expect(useReviewCommentsStore.getState().byTask['task-1'].triageReport).toBeNull();
  });
});
