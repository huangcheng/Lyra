/**
 * Attachment download + inline `cid:` resolution helpers.
 *
 * Attachments need the Authorization header, which plain `<img src>` /
 * `<a href>` cannot carry — bytes always go through `apiBlob` and surface
 * as short-lived object URLs.
 */

import { apiBlob } from '@/lib/api-client';
import type { MailAttachment } from '@/types';

/** `1234567` → `1.2 MB`; stays small and locale-neutral. */
export function formatBytes(bytes: number | undefined): string {
  if (bytes == null || Number.isNaN(bytes)) return '';
  if (bytes < 1024) return `${bytes} B`;
  const units = ['KB', 'MB', 'GB'];
  let value = bytes;
  let unit = -1;
  do {
    value /= 1024;
    unit += 1;
  } while (value >= 1024 && unit < units.length - 1);
  return `${value >= 10 ? Math.round(value) : value.toFixed(1)} ${units[unit]}`;
}

/** Content-IDs arrive with or without angle brackets; normalize for lookup. */
export function normalizeCid(cid: string): string {
  return cid.trim().replace(/^<|>$/g, '').toLowerCase();
}

/** Fetch attachment bytes and trigger a browser download. */
export async function downloadAttachment(att: MailAttachment): Promise<void> {
  const blob = await apiBlob(`/attachments/${att.id}/download`);
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = att.filename || 'attachment';
  a.click();
  URL.revokeObjectURL(url);
}

/**
 * Fetch inline attachments and rewrite `src="cid:…"` references in an HTML
 * body to object URLs. Returns the input unchanged when nothing matches.
 * Callers revoke the URLs via the returned cleanup function.
 */
export async function resolveInlineImages(
  html: string,
  attachments: MailAttachment[] | undefined,
): Promise<{ html: string; revoke: () => void }> {
  const inline = (attachments ?? []).filter(
    (a) => a.isInline && a.contentId && html.toLowerCase().includes('cid:'),
  );
  if (inline.length === 0) return { html, revoke: () => {} };

  const urls: string[] = [];
  const byCid = new Map<string, string>();
  try {
    await Promise.all(
      inline.map(async (att) => {
        const blob = await apiBlob(`/attachments/${att.id}/download`);
        const url = URL.createObjectURL(blob);
        urls.push(url);
        byCid.set(normalizeCid(att.contentId as string), url);
      }),
    );
  } catch {
    // Broken inline parts degrade to the original HTML (cid stays inert).
    urls.forEach((u) => URL.revokeObjectURL(u));
    return { html, revoke: () => {} };
  }

  const rewritten = html.replace(/src=["']cid:([^"']+)["']/gi, (match, cid: string) => {
    const url = byCid.get(normalizeCid(cid));
    return url ? `src="${url}"` : match;
  });
  return {
    html: rewritten,
    revoke: () => urls.forEach((u) => URL.revokeObjectURL(u)),
  };
}
