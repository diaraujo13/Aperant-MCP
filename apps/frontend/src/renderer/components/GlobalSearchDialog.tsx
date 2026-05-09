import { useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { Search, Loader2, MessageSquare, SquareCheckBig } from 'lucide-react';
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription } from './ui/dialog';
import { Input } from './ui/input';
import { ScrollArea } from './ui/scroll-area';
import { Badge } from './ui/badge';
import { cn } from '../lib/utils';
import type { GlobalSearchResult } from '../../shared/types';

interface GlobalSearchDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSelectResult: (result: GlobalSearchResult) => void | Promise<void>;
}

export function GlobalSearchDialog({
  open,
  onOpenChange,
  onSelectResult
}: GlobalSearchDialogProps) {
  const { t } = useTranslation('common');
  const [query, setQuery] = useState('');
  const [results, setResults] = useState<GlobalSearchResult[]>([]);
  const [isSearching, setIsSearching] = useState(false);
  const inputRef = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    if (!open) {
      return;
    }

    const timeout = setTimeout(() => {
      inputRef.current?.focus();
      inputRef.current?.select();
    }, 0);

    return () => clearTimeout(timeout);
  }, [open]);

  useEffect(() => {
    if (!open) {
      return;
    }

    const trimmedQuery = query.trim();
    if (!trimmedQuery) {
      setResults([]);
      setIsSearching(false);
      return;
    }

    let cancelled = false;
    const timeout = setTimeout(async () => {
      setIsSearching(true);
      try {
        const response = await window.electronAPI.searchAllProjects(trimmedQuery);
        if (!cancelled) {
          setResults(response.success && response.data ? response.data : []);
        }
      } finally {
        if (!cancelled) {
          setIsSearching(false);
        }
      }
    }, 300);

    return () => {
      cancelled = true;
      clearTimeout(timeout);
    };
  }, [open, query]);

  const groupedResults = useMemo(() => ({
    tasks: results.filter((result) => result.type === 'task'),
    conversations: results.filter((result) => result.type === 'conversation')
  }), [results]);

  const handleSelect = async (result: GlobalSearchResult) => {
    await onSelectResult(result);
    onOpenChange(false);
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-3xl">
        <DialogHeader>
          <DialogTitle>{t('globalSearch.title')}</DialogTitle>
          <DialogDescription>
            {t('globalSearch.description')}
          </DialogDescription>
        </DialogHeader>

        <div className="relative">
          <Search className="pointer-events-none absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            ref={inputRef}
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder={t('globalSearch.placeholder')}
            className="pl-9"
          />
        </div>

        <ScrollArea className="max-h-[60vh]">
          <div className="space-y-6 pr-4">
            {isSearching && (
              <div className="flex items-center gap-2 py-6 text-sm text-muted-foreground">
                <Loader2 className="h-4 w-4 animate-spin" />
                {t('globalSearch.searching')}
              </div>
            )}

            {!isSearching && !query.trim() && (
              <div className="py-6 text-sm text-muted-foreground">
                {t('globalSearch.empty')}
              </div>
            )}

            {!isSearching && query.trim() && results.length === 0 && (
              <div className="py-6 text-sm text-muted-foreground">
                {t('globalSearch.noMatches')}
              </div>
            )}

            {groupedResults.tasks.length > 0 && (
              <ResultSection title={t('globalSearch.sections.tasks')} count={groupedResults.tasks.length}>
                {groupedResults.tasks.map((result) => (
                  <SearchResultButton
                    key={result.id}
                    onClick={() => handleSelect(result)}
                    title={result.title}
                    subtitle={result.projectName}
                    snippet={result.snippet}
                    badge={result.status}
                    icon={<SquareCheckBig className="h-4 w-4 text-primary" />}
                  />
                ))}
              </ResultSection>
            )}

            {groupedResults.conversations.length > 0 && (
              <ResultSection title={t('globalSearch.sections.conversations')} count={groupedResults.conversations.length}>
                {groupedResults.conversations.map((result) => (
                  <SearchResultButton
                    key={result.id}
                    onClick={() => handleSelect(result)}
                    title={result.sessionTitle}
                    subtitle={`${result.projectName} • ${result.role === 'assistant' ? t('globalSearch.roles.assistant') : t('globalSearch.roles.you')}`}
                    snippet={result.snippet}
                    badge={t('globalSearch.badges.insights')}
                    icon={<MessageSquare className="h-4 w-4 text-info" />}
                  />
                ))}
              </ResultSection>
            )}
          </div>
        </ScrollArea>
      </DialogContent>
    </Dialog>
  );
}

function ResultSection({
  title,
  count,
  children
}: {
  title: string;
  count: number;
  children: ReactNode;
}) {
  return (
    <section className="space-y-2">
      <div className="flex items-center gap-2 text-xs font-semibold uppercase tracking-wide text-muted-foreground">
        <span>{title}</span>
        <Badge variant="secondary" className="text-[10px]">
          {count}
        </Badge>
      </div>
      <div className="space-y-2">{children}</div>
    </section>
  );
}

function SearchResultButton({
  onClick,
  title,
  subtitle,
  snippet,
  badge,
  icon
}: {
  onClick: () => void;
  title: string;
  subtitle: string;
  snippet: string;
  badge: string;
  icon: ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        'flex w-full items-start gap-3 rounded-lg border border-border bg-card px-3 py-3 text-left transition-colors',
        'hover:bg-accent hover:text-accent-foreground'
      )}
    >
      <div className="mt-0.5 shrink-0">{icon}</div>
      <div className="min-w-0 flex-1 space-y-1">
        <div className="flex items-center gap-2">
          <div className="truncate text-sm font-medium">{title}</div>
          <Badge variant="outline" className="shrink-0 text-[10px]">
            {badge}
          </Badge>
        </div>
        <div className="text-xs text-muted-foreground">{subtitle}</div>
        <div className="line-clamp-2 text-sm text-muted-foreground">{snippet}</div>
      </div>
    </button>
  );
}
