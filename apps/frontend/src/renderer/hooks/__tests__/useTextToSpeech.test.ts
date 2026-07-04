// @vitest-environment jsdom
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { renderHook, act } from '@testing-library/react';
import { useTextToSpeech, stripMarkdownForSpeech } from '../useTextToSpeech';

class MockUtterance {
  text: string;
  lang = '';
  onend: (() => void) | null = null;
  onerror: (() => void) | null = null;
  constructor(text: string) {
    this.text = text;
  }
}

describe('stripMarkdownForSpeech', () => {
  it('removes fenced code blocks entirely', () => {
    expect(stripMarkdownForSpeech('before\n```js\nconst x = 1;\n```\nafter')).toBe('before after');
  });

  it('keeps inline code content without backticks', () => {
    expect(stripMarkdownForSpeech('run `npm test` now')).toBe('run npm test now');
  });

  it('keeps link text and drops URLs', () => {
    expect(stripMarkdownForSpeech('see [the docs](https://example.com) here')).toBe(
      'see the docs here'
    );
  });

  it('removes heading markers, list bullets and emphasis', () => {
    expect(stripMarkdownForSpeech('## Title\n- item **bold** and __strong__')).toBe(
      'Title item bold and strong'
    );
  });

  it('returns empty string for markdown-only input', () => {
    expect(stripMarkdownForSpeech('```\ncode\n```')).toBe('');
  });
});

describe('useTextToSpeech', () => {
  let speakSpy: ReturnType<typeof vi.fn>;
  let cancelSpy: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    speakSpy = vi.fn();
    cancelSpy = vi.fn();
    vi.stubGlobal('speechSynthesis', { speak: speakSpy, cancel: cancelSpy });
    vi.stubGlobal('SpeechSynthesisUtterance', MockUtterance);
  });

  it('reports supported and speaks cleaned text', () => {
    const { result } = renderHook(() => useTextToSpeech());
    expect(result.current.isSupported).toBe(true);

    act(() => {
      result.current.speak('hello **world**', 'pt-BR');
    });

    expect(cancelSpy).toHaveBeenCalled();
    expect(speakSpy).toHaveBeenCalledTimes(1);
    const utterance = speakSpy.mock.calls[0][0] as MockUtterance;
    expect(utterance.text).toBe('hello world');
    expect(utterance.lang).toBe('pt-BR');
    expect(result.current.speaking).toBe(true);
  });

  it('does not speak when text is markdown-only noise', () => {
    const { result } = renderHook(() => useTextToSpeech());
    act(() => {
      result.current.speak('```\n\n```');
    });
    expect(speakSpy).not.toHaveBeenCalled();
    expect(result.current.speaking).toBe(false);
  });

  it('stop() cancels and clears speaking state', () => {
    const { result } = renderHook(() => useTextToSpeech());
    act(() => {
      result.current.speak('some text');
    });
    act(() => {
      result.current.stop();
    });
    expect(cancelSpy).toHaveBeenCalledTimes(2); // once before speak, once on stop
    expect(result.current.speaking).toBe(false);
  });

  it('clears speaking state when the utterance ends on its own', () => {
    const { result } = renderHook(() => useTextToSpeech());
    act(() => {
      result.current.speak('some text');
    });
    const utterance = speakSpy.mock.calls[0][0] as MockUtterance;
    act(() => {
      utterance.onend?.();
    });
    expect(result.current.speaking).toBe(false);
  });

  it('cancels pending speech on unmount without touching state', () => {
    const { result, unmount } = renderHook(() => useTextToSpeech());
    act(() => {
      result.current.speak('some text');
    });
    unmount();
    expect(cancelSpy).toHaveBeenCalledTimes(2); // pre-speak + unmount cleanup
  });
});
