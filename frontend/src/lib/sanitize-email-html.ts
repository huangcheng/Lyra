/**
 * Render-pass sanitize for attacker-controlled email HTML.
 *
 * Two profiles:
 * - `sanitizeEmailHtml` (strict, in-page): forbids `class` (Tailwind
 *   piggyback overlays) and `<style>` (global CSS leaks); used wherever the
 *   markup lands in the app document (e.g. compose quotes).
 * - `sanitizeEmailHtmlForFrame` (iframe): the body renders inside a
 *   sandboxed iframe (MailBodyFrame), so `class` and `<style>` are safe and
 *   MUST survive — HTML email layout depends on them.
 *
 * Both force safe link targets and drop active/embed content.
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
 * DOMPurify options for the sandboxed-iframe renderer. Active content stays
 * forbidden; `<style>`/`class` pass through because the iframe document has
 * none of the app's CSS and scripts cannot run there. `FORCE_BODY` keeps a
 * leading `<style>` in the body — without it the fragment parser relocates
 * it to the head and the returned body markup loses the author CSS.
 */
export const EMAIL_HTML_FRAME_PURIFY_CONFIG: Config = {
  FORBID_TAGS: ['iframe', 'object', 'embed', 'form', 'meta', 'link', 'base'],
  FORCE_BODY: true,
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

/** Sanitize email HTML for rendering inside MailBodyFrame. */
export function sanitizeEmailHtmlForFrame(html: string): string {
  ensureHooks();
  return DOMPurify.sanitize(html, EMAIL_HTML_FRAME_PURIFY_CONFIG);
}
