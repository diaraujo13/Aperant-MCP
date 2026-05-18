/**
 * Minimal unified-diff parser tailored for our review UI.
 *
 * We deliberately do NOT pull `parse-diff` from npm — the format we need is
 * narrow (single-file patches produced by `git diff`) and the dep would add
 * weight to the Electron bundle for one ~40-line function.
 */

export type DiffLineKind = 'context' | 'add' | 'del' | 'hunk';

export interface DiffLine {
  kind: DiffLineKind;
  content: string;
  /** 1-indexed line number in the OLD file (null for added lines). */
  oldLine: number | null;
  /** 1-indexed line number in the NEW file (null for deleted lines). */
  newLine: number | null;
}

export interface DiffHunk {
  header: string;
  lines: DiffLine[];
}

export interface ParsedDiff {
  hunks: DiffHunk[];
}

const HUNK_RE = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/;

export function parseUnifiedDiff(patch: string): ParsedDiff {
  if (!patch) return { hunks: [] };
  const lines = patch.split('\n');
  const hunks: DiffHunk[] = [];
  let current: DiffHunk | null = null;
  let oldLine = 0;
  let newLine = 0;

  for (const raw of lines) {
    // Skip file headers — we only consume hunks.
    if (raw.startsWith('diff ') || raw.startsWith('index ') || raw.startsWith('--- ') || raw.startsWith('+++ ')) {
      continue;
    }
    const hunkMatch = HUNK_RE.exec(raw);
    if (hunkMatch) {
      current = { header: raw, lines: [] };
      hunks.push(current);
      oldLine = parseInt(hunkMatch[1], 10);
      newLine = parseInt(hunkMatch[2], 10);
      continue;
    }
    if (!current) continue;
    if (raw.startsWith('+')) {
      current.lines.push({ kind: 'add', content: raw.slice(1), oldLine: null, newLine });
      newLine++;
    } else if (raw.startsWith('-')) {
      current.lines.push({ kind: 'del', content: raw.slice(1), oldLine, newLine: null });
      oldLine++;
    } else if (raw.startsWith('\\')) {
    } else {
      // Context line (starts with space or empty trailing line).
      const content = raw.startsWith(' ') ? raw.slice(1) : raw;
      current.lines.push({ kind: 'context', content, oldLine, newLine });
      oldLine++;
      newLine++;
    }
  }

  return { hunks };
}
