import { useCallback, useEffect, useRef, useState } from 'react';

/**
 * Strips markdown syntax so the speech engine reads prose, not symbols
 * (fences, headings, emphasis markers, link URLs, table pipes).
 */
export function stripMarkdownForSpeech(markdown: string): string {
  return markdown
    .replace(/```[\s\S]*?```/g, ' ')
    .replace(/`([^`]+)`/g, '$1')
    .replace(/!\[([^\]]*)\]\([^)]*\)/g, '$1')
    .replace(/\[([^\]]+)\]\([^)]*\)/g, '$1')
    .replace(/^#{1,6}\s+/gm, '')
    .replace(/^\s*[-*+]\s+/gm, '')
    .replace(/^\s*\d+\.\s+/gm, '')
    .replace(/^\s*>\s?/gm, '')
    .replace(/(\*\*|__|~~)/g, '')
    .replace(/(^|\s)[*_]([^*_]+)[*_](?=\s|$|[.,;:!?])/g, '$1$2')
    .replace(/\|/g, ' ')
    .replace(/\s+/g, ' ')
    .trim();
}

export interface UseTextToSpeechResult {
  /** Whether the Web Speech API is available in this webview. */
  isSupported: boolean;
  /** True while an utterance started by this hook instance is playing. */
  speaking: boolean;
  /** Speak `text` aloud, cancelling any utterance currently playing. */
  speak: (text: string, lang?: string) => void;
  /** Stop all speech output. */
  stop: () => void;
}

/**
 * Text-to-speech via the Web Speech API (`window.speechSynthesis`), available
 * in both Chromium (Electron) and WKWebView (Tauri/macOS) — no native deps.
 *
 * speechSynthesis is a global singleton, so speak() cancels whatever is
 * playing (including utterances from other hook instances) before starting.
 */
export function useTextToSpeech(): UseTextToSpeechResult {
  const isSupported = typeof window !== 'undefined' && 'speechSynthesis' in window;
  const [speaking, setSpeaking] = useState(false);
  const utteranceRef = useRef<SpeechSynthesisUtterance | null>(null);

  useEffect(() => {
    return () => {
      // Detach handlers before cancelling so the unmount cancel doesn't call
      // setState on an unmounted component, then silence our utterance.
      if (utteranceRef.current) {
        utteranceRef.current.onend = null;
        utteranceRef.current.onerror = null;
        window.speechSynthesis.cancel();
      }
    };
  }, []);

  const stop = useCallback(() => {
    if (!isSupported) return;
    window.speechSynthesis.cancel();
    setSpeaking(false);
  }, [isSupported]);

  const speak = useCallback(
    (text: string, lang?: string) => {
      if (!isSupported) return;
      const cleaned = stripMarkdownForSpeech(text);
      if (!cleaned) return;

      window.speechSynthesis.cancel();

      const utterance = new SpeechSynthesisUtterance(cleaned);
      if (lang) {
        utterance.lang = lang;
      }
      utterance.onend = () => {
        utteranceRef.current = null;
        setSpeaking(false);
      };
      utterance.onerror = () => {
        utteranceRef.current = null;
        setSpeaking(false);
      };

      utteranceRef.current = utterance;
      setSpeaking(true);
      window.speechSynthesis.speak(utterance);
    },
    [isSupported]
  );

  return { isSupported, speaking, speak, stop };
}
