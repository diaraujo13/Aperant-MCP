import { Square, Volume2 } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { useTextToSpeech } from '../../hooks/useTextToSpeech';
import { Button } from './button';

interface SpeakButtonProps {
  /** Text (plain or markdown) to read aloud. */
  text: string;
  className?: string;
}

/**
 * Read-aloud toggle for AI output, docs, and other long-form text.
 * Renders nothing when the Web Speech API is unavailable.
 */
export function SpeakButton({ text, className }: SpeakButtonProps) {
  const { t, i18n } = useTranslation('common');
  const { isSupported, speaking, speak, stop } = useTextToSpeech();

  if (!isSupported) return null;

  const label = speaking ? t('tts.stop') : t('tts.speak');

  return (
    <Button
      type="button"
      variant="ghost"
      size="icon"
      className={className}
      onClick={() => (speaking ? stop() : speak(text, i18n.language))}
      aria-label={label}
      title={label}
    >
      {speaking ? <Square className="h-4 w-4" /> : <Volume2 className="h-4 w-4" />}
    </Button>
  );
}
