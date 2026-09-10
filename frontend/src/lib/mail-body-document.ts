/**
 * Sheet document for the sandboxed email body iframe (MailBodyFrame).
 *
 * The base styles act as defaults only: author markup (with its own inline
 * styles and `<style>` blocks) comes later in document order and wins ties.
 * The sheet is paper-white in both app themes — HTML mail expects a light
 * canvas unless the sender ships their own dark media queries.
 */

const SHEET_CSS = `
  html, body { margin: 0; padding: 0; }
  body {
    background: #ffffff;
    color: #18181b;
    font-size: 14px;
    line-height: 1.55;
    overflow-wrap: break-word;
    word-wrap: break-word;
    overflow-anchor: auto;
  }
  img { max-width: 100%; height: auto; vertical-align: middle; }
  table { max-width: 100%; }
  pre, code { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 0.92em; }
  pre { white-space: pre-wrap; }
  a { color: #1f6feb; text-decoration: underline; text-underline-offset: 2px; }
  a:hover { color: #1a5fd0; }
  blockquote { margin: 0; padding-left: 0.8em; border-left: 3px solid #e4e4e7; color: #52525b; }
  h1, h2, h3, h4 { line-height: 1.3; }
  hr { border: 0; border-top: 1px solid #e4e4e7; }
`;

/** Wrap sanitized body markup in the sheet document. */
export function buildMailBodyDocument(html: string): string {
  return (
    '<!doctype html><html><head><meta charset="utf-8">' +
    '<base target="_blank">' +
    `<style>${SHEET_CSS}</style>` +
    '</head><body>' +
    html +
    '</body></html>'
  );
}
