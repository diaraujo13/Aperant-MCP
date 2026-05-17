import { describe, expect, it } from 'vitest';
import { parseUnifiedDiff } from '../unified-diff';

describe('parseUnifiedDiff', () => {
  it('returns empty hunks for empty input', () => {
    expect(parseUnifiedDiff('')).toEqual({ hunks: [] });
  });

  it('parses a single hunk with adds, dels and context', () => {
    const patch = [
      'diff --git a/x.ts b/x.ts',
      'index 1111..2222 100644',
      '--- a/x.ts',
      '+++ b/x.ts',
      '@@ -1,3 +1,4 @@',
      ' const a = 1;',
      '-const b = 2;',
      '+const b = 3;',
      '+const c = 4;',
      ' const d = 5;',
    ].join('\n');

    const { hunks } = parseUnifiedDiff(patch);
    expect(hunks).toHaveLength(1);
    const lines = hunks[0].lines;
    // 1 context + 1 del + 2 adds + 1 context
    expect(lines).toHaveLength(5);
    expect(lines[0]).toMatchObject({ kind: 'context', oldLine: 1, newLine: 1 });
    expect(lines[1]).toMatchObject({ kind: 'del', oldLine: 2, newLine: null });
    expect(lines[2]).toMatchObject({ kind: 'add', oldLine: null, newLine: 2 });
    expect(lines[3]).toMatchObject({ kind: 'add', oldLine: null, newLine: 3 });
    expect(lines[4]).toMatchObject({ kind: 'context', oldLine: 3, newLine: 4 });
  });

  it('handles multiple hunks', () => {
    const patch = [
      '@@ -1,1 +1,1 @@',
      '-old',
      '+new',
      '@@ -10,1 +10,1 @@',
      '-foo',
      '+bar',
    ].join('\n');
    const { hunks } = parseUnifiedDiff(patch);
    expect(hunks).toHaveLength(2);
    expect(hunks[0].lines).toHaveLength(2);
    expect(hunks[1].lines[1]).toMatchObject({ kind: 'add', content: 'bar', newLine: 10 });
  });

  it('skips "No newline" markers', () => {
    const patch = ['@@ -1 +1 @@', '-a', '+b', '\\ No newline at end of file'].join('\n');
    const { hunks } = parseUnifiedDiff(patch);
    expect(hunks[0].lines).toHaveLength(2);
  });
});
