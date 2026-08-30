/**
 * Recipient chip input — Gmail/Fastmail style address pills + free-type field.
 *
 * Committed addresses render as removable pills; Enter/Tab/comma/semicolon or
 * blur commits the pending text; Backspace on an empty field pops the last pill.
 */

import { X } from 'lucide-react';

import { avatarTone, cn, getInitials } from '@/lib/utils';

interface RecipientsInputProps {
  id: string;
  chips: string[];
  /** Pending (uncommitted) text in the trailing input. */
  input: string;
  onChipsChange: (chips: string[]) => void;
  onInputChange: (value: string) => void;
  placeholder?: string;
  autoFocus?: boolean;
}

/** Split a raw chunk into addresses on comma/semicolon boundaries. */
export function splitAddresses(raw: string): string[] {
  return raw
    .split(/[,;]/)
    .map((s) => s.trim())
    .filter(Boolean);
}

export function RecipientsInput({
  id,
  chips,
  input,
  onChipsChange,
  onInputChange,
  placeholder,
  autoFocus,
}: RecipientsInputProps) {
  const commit = (raw?: string) => {
    const parts = splitAddresses(raw ?? input);
    if (parts.length === 0) return;
    onChipsChange([...chips, ...parts.filter((p) => !chips.includes(p))]);
    onInputChange('');
  };

  return (
    <div
      className="flex min-h-11 cursor-text flex-wrap items-center gap-1.5 py-1.5"
      onClick={(e) => {
        const inputEl = e.currentTarget.querySelector('input');
        inputEl?.focus();
      }}
    >
      {chips.map((addr, i) => (
        <span
          key={`${addr}-${i}`}
          className="flex max-w-52 items-center gap-1.5 rounded-full border border-border/70 bg-muted/40 py-0.5 pl-1 pr-1.5 text-xs"
        >
          <span
            aria-hidden
            className={cn(
              'flex size-4.5 shrink-0 items-center justify-center rounded-full text-[8px] font-semibold',
              avatarTone(addr),
            )}
          >
            {getInitials(addr)}
          </span>
          <span className="min-w-0 truncate">{addr}</span>
          <button
            type="button"
            className="relative flex size-4 shrink-0 items-center justify-center rounded-full text-muted-foreground transition-colors before:absolute before:-inset-1.5 before:content-[''] hover:bg-accent hover:text-foreground"
            aria-label={`Remove ${addr}`}
            onClick={(e) => {
              e.stopPropagation();
              onChipsChange(chips.filter((_, j) => j !== i));
            }}
          >
            <X className="size-2.5" aria-hidden />
          </button>
        </span>
      ))}
      <input
        id={id}
        className="h-7 min-w-24 flex-1 bg-transparent text-sm outline-none placeholder:text-muted-foreground/60"
        value={input}
        placeholder={chips.length === 0 ? placeholder : undefined}
        autoFocus={autoFocus}
        onChange={(e) => {
          const v = e.target.value;
          if (/[,;]/.test(v)) {
            commit(v);
          } else {
            onInputChange(v);
          }
        }}
        onKeyDown={(e) => {
          if (e.key === 'Enter' || e.key === 'Tab') {
            if (input.trim()) {
              e.preventDefault();
              commit();
            }
          } else if (e.key === 'Backspace' && !input && chips.length > 0) {
            onChipsChange(chips.slice(0, -1));
          }
        }}
        onBlur={() => commit()}
      />
    </div>
  );
}
