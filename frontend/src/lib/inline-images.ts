/**
 * Compose-side inline image plumbing: object-URL ↔ cid: rewriting, Content-ID
 * generation, base64 for the drafts JSON. Pure helpers; the only side effects
 * are `URL.createObjectURL` and the caller-provided `fetchBlob`.
 */

import { normalizeCid } from '@/lib/attachments';

export interface InlineImageEntry {
  file: File;
  contentId: string;
}

export interface InlineImagePart {
  filename: string;
  contentType: string;
  contentId: string;
  file: File;
}

/** RFC 2045 msg-id-style Content-ID value (brackets added by the backend). */
export function newContentId(): string {
  return `${crypto.randomUUID()}@lyra`;
}

/**
 * Rewrite tracked object-URL image srcs to `cid:` refs (RFC 2392) and collect
 * each referenced image once. Untracked/remote URLs pass through unchanged.
 */
export function extractInlineImages(
  html: string,
  urlToImage: ReadonlyMap<string, InlineImageEntry>,
): { html: string; parts: InlineImagePart[] } {
  if (!html.includes('blob:')) return { html, parts: [] };
  const parts: InlineImagePart[] = [];
  const seen = new Set<string>();
  const rewritten = html.replace(/src=["'](blob:[^"']+)["']/g, (match, url: string) => {
    const entry = urlToImage.get(url);
    if (!entry) return match;
    if (!seen.has(url)) {
      seen.add(url);
      parts.push({
        filename: entry.file.name || 'image',
        contentType: entry.file.type || 'image/png',
        contentId: entry.contentId,
        file: entry.file,
      });
    }
    return `src="cid:${entry.contentId}"`;
  });
  return { html: rewritten, parts };
}

/** Attachment metadata for an inline part of an existing message/draft. */
export interface InlineSourceMeta {
  id: string;
  filename?: string;
  contentType?: string;
  contentId?: string;
}

/**
 * Reopened draft / quoted body: rewrite `cid:` refs to fresh object URLs and
 * map each URL back to its bytes + original Content-ID (reused on re-send).
 */
export async function resolveInlineSources(
  html: string,
  sources: InlineSourceMeta[],
  fetchBlob: (id: string) => Promise<Blob>,
): Promise<{ html: string; urlToImage: Map<string, InlineImageEntry> }> {
  const urlToImage = new Map<string, InlineImageEntry>();
  if (!html.toLowerCase().includes('cid:')) return { html, urlToImage };
  let out = html;
  for (const source of sources) {
    if (!source.contentId) continue;
    const cid = normalizeCid(source.contentId);
    if (!out.toLowerCase().includes(`cid:${cid}`)) continue;
    try {
      const blob = await fetchBlob(source.id);
      const file = new File([blob], source.filename || 'image', {
        type: source.contentType || blob.type || 'image/png',
      });
      const url = URL.createObjectURL(blob);
      urlToImage.set(url, { file, contentId: cid });
      out = out.replace(
        new RegExp(`src=["']cid:${cid.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}["']`, 'gi'),
        `src="${url}"`,
      );
    } catch {
      // Broken part: leave the cid ref inert; send drops it via extractInlineImages.
    }
  }
  return { html: out, urlToImage };
}

/** File → base64 (chunked; large images don't blow the call stack). */
export async function fileToBase64(file: File): Promise<string> {
  const bytes = new Uint8Array(await file.arrayBuffer());
  let binary = '';
  const CHUNK = 0x8000;
  for (let i = 0; i < bytes.length; i += CHUNK) {
    binary += String.fromCharCode(...bytes.subarray(i, i + CHUNK));
  }
  return btoa(binary);
}

/** Pick a collision-free filename: `name`, else `name-2.ext`, `name-3.ext`, …
 * Filenames must be unique within one compose — the backend's
 * `apply_inline_meta` matches inline parts to `files` parts by filename. */
export function uniqueFileName(name: string, taken: ReadonlySet<string>): string {
  if (!taken.has(name)) return name;
  const dot = name.lastIndexOf('.');
  const stem = dot > 0 ? name.slice(0, dot) : name;
  const ext = dot > 0 ? name.slice(dot) : '';
  for (let i = 2; ; i += 1) {
    const candidate = `${stem}-${i}${ext}`;
    if (!taken.has(candidate)) return candidate;
  }
}

/** Same bytes/type under a new name (no copy when the name is unchanged). */
export function renamedFile(file: File, name: string): File {
  return name === file.name ? file : new File([file], name, { type: file.type });
}
