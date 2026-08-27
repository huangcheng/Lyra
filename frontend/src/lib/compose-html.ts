/**
 * Compose-side HTML scaffolding: signature placement, reply/forward quoting,
 * and plain-text → HTML lifting.
 */

const escapeHtml = (s: string) =>
  s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');

/** Multiline plain text → `<p>` rows (escapes HTML). */
export function textToHtml(text: string): string {
  if (!text.trim()) return '';
  return text
    .split(/\n{2,}/)
    .map((para) => `<p>${escapeHtml(para).replace(/\n/g, '<br>')}</p>`)
    .join('');
}

/** Stored signature (plain text or simple HTML) → HTML block. */
export function signatureHtml(signature: string | undefined | null): string {
  const raw = (signature ?? '').trim();
  if (!raw) return '';
  if (raw.startsWith('<')) return raw;
  return `<p>--<br>${escapeHtml(raw).replace(/\n/g, '<br>')}</p>`;
}

export interface QuoteSource {
  fromName: string;
  fromEmail: string;
  date: string;
  bodyHtml?: string;
  bodyText?: string;
}

/**
 * Thunderbird-style quoted reply body:
 *   [signature]
 *   On {date}, {name} wrote:
 *   <blockquote>…original…</blockquote>
 * The signature sits above the quote for replies, below the text for new mail.
 */
export function quotedReplyHtml(source: QuoteSource, signature: string | undefined): string {
  const sig = signatureHtml(signature);
  const attribution = `On ${source.date}, ${source.fromName || source.fromEmail} wrote:`;
  const original = source.bodyHtml?.trim() || textToHtml(source.bodyText ?? '') || '<p></p>';
  return `${sig}<p>${escapeHtml(attribution)}</p><blockquote>${original}</blockquote>`;
}

/** Forward: quoted original under an Fw header; signature at the bottom. */
export function forwardHtml(source: QuoteSource, signature: string | undefined): string {
  const header = textToHtml(
    [
      `-------- Forwarded message --------`,
      `From: ${source.fromName ? `${source.fromName} <${source.fromEmail}>` : source.fromEmail}`,
      `Date: ${source.date}`,
    ].join('\n'),
  );
  const original = source.bodyHtml?.trim() || textToHtml(source.bodyText ?? '') || '<p></p>';
  const sig = signatureHtml(signature);
  return `${header}<blockquote>${original}</blockquote>${sig}`;
}
