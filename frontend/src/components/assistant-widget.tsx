/**
 * AI assistant widget: floating bubble (bottom-right) → chat dialog.
 * Standard widget pattern; shows only when the assistant feature is
 * enabled+configured. When a message is open, it rides along as context
 * with a one-tap Summarize action.
 */

import { Sparkles, Trash2, X } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';

import { Button } from '@/components/ui/button';
import {
  clearAiChat,
  fetchAiChat,
  fetchAiSettings,
  sendAiChat,
  type AiChatMessage,
} from '@/lib/ai-api';
import { assistantAvailable, summarizePrompt } from '@/lib/assistant';
import { describePendingAction, type PendingAction } from '@/lib/assistant-actions';
import { actOnMessages } from '@/lib/conversation-actions';
import { t, type SupportedLocale } from '@/i18n';
import { useMailStore } from '@/stores/mail';
import { useUIStore } from '@/stores/ui';
import { cn } from '@/lib/utils';

function Bubble({ open, onToggle }: { open: boolean; onToggle: () => void }) {
  return (
    <button
      type="button"
      aria-label={t(useUIStore.getState().locale, 'assistant.title')}
      aria-expanded={open}
      onClick={onToggle}
      className={cn(
        'fixed right-5 bottom-5 z-50 flex size-12 items-center justify-center',
        'rounded-full border border-border bg-primary text-primary-foreground',
        'shadow-lift transition-transform duration-150 ease-out-quart',
        'hover:scale-105 active:scale-95',
      )}
    >
      {open ? <X className="size-5" aria-hidden /> : <Sparkles className="size-5" aria-hidden />}
    </button>
  );
}

function TypingDots() {
  return (
    <span className="inline-flex items-center gap-1 px-1 py-2" aria-label="…">
      {[0, 1, 2].map((i) => (
        <span
          key={i}
          className="size-1.5 animate-pulse rounded-full bg-muted-foreground"
          style={{ animationDelay: `${i * 160}ms` }}
        />
      ))}
    </span>
  );
}

function ChatPanel({ locale, onClose }: { locale: SupportedLocale; onClose: () => void }) {
  const selectedMessageId = useUIStore((s) => s.selectedMessageId);
  const messages = useMailStore((s) => s.messages);
  const openMessage = selectedMessageId ? messages[selectedMessageId] : undefined;

  const [history, setHistory] = useState<AiChatMessage[] | null>(null);
  const [pending, setPending] = useState<PendingAction[]>([]);
  const [input, setInput] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const listRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    void fetchAiChat()
      .then(setHistory)
      .catch(() => setHistory([]));
  }, []);

  // Keep the latest bubble in view as history grows.
  useEffect(() => {
    listRef.current?.scrollTo({ top: listRef.current.scrollHeight });
  }, [history, busy]);

  const send = async (text: string) => {
    const message = text.trim();
    if (!message || busy) return;
    setBusy(true);
    setError(null);
    setInput('');
    setPending([]);
    setHistory((h) => [...(h ?? []), { role: 'user', content: message, createdAt: '' }]);
    try {
      const { reply, actions } = await sendAiChat(message, openMessage?.id);
      setHistory((h) => [...(h ?? []), { role: 'assistant', content: reply, createdAt: '' }]);
      setPending(actions);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div
      role="dialog"
      aria-label={t(locale, 'assistant.title')}
      className="fixed right-5 bottom-20 z-50 flex h-[min(560px,calc(100dvh-7rem))] w-[min(380px,calc(100vw-2.5rem))] flex-col overflow-hidden rounded-xl border border-border bg-card shadow-lift"
    >
      <div className="flex h-11 shrink-0 items-center gap-2 border-b border-border/60 px-3">
        <Sparkles className="size-4 text-muted-foreground" aria-hidden />
        <span className="text-[13px] font-medium">{t(locale, 'assistant.title')}</span>
        <Button
          variant="ghost"
          size="icon-sm"
          className="ml-auto text-muted-foreground hover:text-foreground"
          aria-label={t(locale, 'assistant.clear')}
          title={t(locale, 'assistant.clear')}
          disabled={busy}
          onClick={() => {
            void clearAiChat()
              .then(() => {
                setHistory([]);
                setPending([]);
              })
              .catch((e) => setError(e instanceof Error ? e.message : String(e)));
          }}
        >
          <Trash2 className="size-4" aria-hidden />
        </Button>
        <Button
          variant="ghost"
          size="icon-sm"
          className="text-muted-foreground hover:text-foreground"
          aria-label={t(locale, 'mail.close')}
          onClick={onClose}
        >
          <X className="size-4" aria-hidden />
        </Button>
      </div>

      {openMessage ? (
        <div className="flex shrink-0 items-center gap-2 border-b border-border/60 bg-accent/40 px-3 py-1.5">
          <span className="min-w-0 flex-1 truncate text-[11px] text-muted-foreground">
            {t(locale, 'assistant.context')}: {openMessage.subject || '(no subject)'}
          </span>
          <Button
            variant="ghost"
            size="sm"
            className="h-6 rounded-full px-2 text-[11px]"
            disabled={busy}
            onClick={() => void send(summarizePrompt(locale))}
          >
            {t(locale, 'assistant.summarize')}
          </Button>
        </div>
      ) : null}

      <div ref={listRef} className="min-h-0 flex-1 space-y-2 overflow-y-auto px-3 py-3">
        {history === null ? (
          <TypingDots />
        ) : history.length === 0 ? (
          <p className="px-1 text-xs text-muted-foreground">{t(locale, 'assistant.emptyHint')}</p>
        ) : (
          history.map((m, i) => (
            <div
              key={i}
              className={cn(
                'max-w-[85%] rounded-[10px] px-3 py-2 text-[13px] leading-relaxed',
                'whitespace-pre-wrap break-words',
                m.role === 'user'
                  ? 'ml-auto bg-accent'
                  : 'border border-border bg-background text-foreground',
              )}
            >
              {m.content}
            </div>
          ))
        )}
        {busy ? <TypingDots /> : null}
        {error ? <div className="text-xs text-destructive">{error}</div> : null}
      </div>

      {pending.length > 0 ? (
        <div className="space-y-1.5 border-t border-border/60 px-3 py-2">
          {pending.map((a, i) => (
            <div
              key={i}
              className="flex items-center gap-2 rounded-lg border border-border bg-background px-2.5 py-2 text-xs"
            >
              <Sparkles className="size-3.5 shrink-0 text-muted-foreground" aria-hidden />
              <span className="min-w-0 flex-1 truncate">
                <span className="text-muted-foreground">{t(locale, 'assistant.proposal')}:</span>{' '}
                {describePendingAction(a, locale)}
              </span>
              <Button
                variant="ghost"
                size="sm"
                className="h-6 rounded-full px-2"
                disabled={busy}
                onClick={() => {
                  setPending((p) => p.filter((_, j) => j !== i));
                  if (a.type === 'openDraft') {
                    useUIStore
                      .getState()
                      .openCompose({ to: a.to, subject: a.subject, body: a.body, mode: 'new' });
                  } else {
                    void actOnMessages([a.messageId], a.action).then((res) => {
                      if (res.error) setError(res.error);
                    });
                  }
                }}
              >
                {t(locale, 'assistant.confirm')}
              </Button>
              <Button
                variant="ghost"
                size="sm"
                className="h-6 rounded-full px-2 text-muted-foreground"
                onClick={() => setPending((p) => p.filter((_, j) => j !== i))}
              >
                {t(locale, 'assistant.discard')}
              </Button>
            </div>
          ))}
        </div>
      ) : null}

      <div className="flex shrink-0 items-end gap-2 border-t border-border/60 p-2">
        <textarea
          value={input}
          rows={Math.min(4, input.split('\n').length)}
          placeholder={t(locale, 'assistant.placeholder')}
          className="max-h-24 min-h-9 flex-1 resize-none rounded-lg border border-input bg-background px-2.5 py-2 text-[13px] outline-none focus-visible:border-ring"
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && !e.shiftKey) {
              e.preventDefault();
              void send(input);
            }
          }}
        />
        <Button
          size="sm"
          className="h-9 rounded-full px-4"
          disabled={busy || !input.trim()}
          onClick={() => void send(input)}
        >
          {t(locale, 'assistant.send')}
        </Button>
      </div>
    </div>
  );
}

/** Mount once at the app root. Renders nothing while unavailable. */
export function AssistantWidget() {
  const [available, setAvailable] = useState(false);
  const [open, setOpen] = useState(false);
  const locale = useUIStore((s) => s.locale);

  useEffect(() => {
    void fetchAiSettings()
      .then((s) => setAvailable(assistantAvailable(s)))
      .catch(() => setAvailable(false));
  }, []);

  if (!available) return null;
  return (
    <>
      {!open ? <Bubble open={false} onToggle={() => setOpen(true)} /> : null}
      {open ? (
        <>
          <Bubble open onToggle={() => setOpen(false)} />
          <ChatPanel locale={locale} onClose={() => setOpen(false)} />
        </>
      ) : null}
    </>
  );
}
