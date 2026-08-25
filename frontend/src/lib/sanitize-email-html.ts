/**
 * Render-pass sanitize for attacker-controlled email HTML (in-page model).
 *
 * Defense in depth with backend ammonia ingest. In-page-specific rules:
 * - forbid `class` (Tailwind piggyback overlays)
 * - forbid `<style>` (global CSS leaks); keep inline `style`
 * - force safe link targets
 */

import DOMPurify from 'dompurify';
import type { Config } from 'dompurify';

let hooksInstalled = false;

function ensureHooks(): void {
  if (hooksInstalled) return;
  hooksInstalled = true;
  DOMPurify.addHook('afterSanitizeAttributes', (node) => {
    if (node instanceof Element && node.tagName === 'A') {
      node.setAttribute('target', '_blank');
      node.setAttribute('rel', 'noopener noreferrer');
    }
  });
}

/** DOMPurify options for in-page mail HTML (exported for unit tests). */
export const EMAIL_HTML_PURIFY_CONFIG: Config = {
  FORBID_TAGS: ['iframe', 'object', 'embed', 'form', 'meta', 'link', 'base', 'style'],
  FORBID_ATTR: ['class'],
};

/**
 * Sanitize email HTML for in-page rendering.
 * Deliberately allows inline styles and (after backend rewrite) image URLs
 * that the privacy layer already decided.
 */
export function sanitizeEmailHtml(html: string): string {
  ensureHooks();
  return DOMPurify.sanitize(html, EMAIL_HTML_PURIFY_CONFIG);
}
