/**
 * Sender mail-client identification. The backend persists the raw
 * `User-Agent` / `X-Mailer` / `X-MimeOLE` header; this module maps it to a
 * client family. Unrecognized or absent values return null — a quiet miss
 * beats a noisy "unknown" badge.
 */

/** Stable family id; `components/mailer-chip.tsx` maps it to an icon. */
export type MailerId =
  | 'thunderbird'
  | 'apple-mail'
  | 'outlook'
  | 'foxmail'
  | 'qqmail'
  | 'netease'
  | 'k9mail'
  | 'fairemail'
  | 'evolution'
  | 'mutt'
  | 'spike'
  | 'mailspring'
  | 'generic';

export interface MailerMatch {
  id: MailerId;
  /** Client family name (proper noun — never localized). */
  name: string;
}

/** Order matters: first match wins. Substring rules on the raw header value. */
const RULES: Array<{ pattern: RegExp; id: MailerId; name: string }> = [
  { pattern: /thunderbird|betterbird/i, id: 'thunderbird', name: 'Thunderbird' },
  { pattern: /apple mail|x-apple-mail/i, id: 'apple-mail', name: 'Apple Mail' },
  { pattern: /microsoft (office )?outlook|msoffice|mimeole/i, id: 'outlook', name: 'Outlook' },
  { pattern: /foxmail/i, id: 'foxmail', name: 'Foxmail' },
  { pattern: /qqmail|qq邮箱/i, id: 'qqmail', name: 'QQ Mail' },
  { pattern: /网易|netease/i, id: 'netease', name: 'NetEase Mail' },
  { pattern: /k-?9 mail|thunder.*android/i, id: 'k9mail', name: 'K-9 Mail' },
  { pattern: /fairemail/i, id: 'fairemail', name: 'FairEmail' },
  { pattern: /evolution/i, id: 'evolution', name: 'Evolution' },
  { pattern: /^(neo)?mutt|alpine|mailx/i, id: 'mutt', name: 'Mutt' },
  { pattern: /yamail|yandex/i, id: 'generic', name: 'Yandex Mail' },
  { pattern: /coremail/i, id: 'generic', name: 'Coremail' },
  { pattern: /zendesk/i, id: 'generic', name: 'Zendesk' },
  { pattern: /spike/i, id: 'spike', name: 'Spike' },
  { pattern: /mailspring/i, id: 'mailspring', name: 'Mailspring' },
];

/**
 * Map a raw mailer header to a client family. `null` when absent or
 * unrecognized — callers render nothing in that case.
 */
export function matchMailer(raw: string | null | undefined): MailerMatch | null {
  const value = raw?.trim();
  if (!value) return null;
  for (const rule of RULES) {
    if (rule.pattern.test(value)) return { id: rule.id, name: rule.name };
  }
  return null;
}
