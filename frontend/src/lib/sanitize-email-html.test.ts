/**
 * @vitest-environment jsdom
 */
import { describe, expect, it } from 'vitest';

import { sanitizeEmailHtml } from './sanitize-email-html';

describe('sanitizeEmailHtml', () => {
  it('strips class attributes (Tailwind overlay defense)', () => {
    const out = sanitizeEmailHtml('<div class="fixed inset-0 z-50 bg-black/80">x</div>');
    expect(out).not.toContain('class=');
    expect(out).toContain('>x</div>');
  });

  it('drops style tags but keeps inline style', () => {
    const out = sanitizeEmailHtml(
      '<style>body{background:red}</style><p style="color:blue">hi</p>',
    );
    expect(out.toLowerCase()).not.toContain('<style');
    expect(out).toContain('style="color:blue"');
    expect(out).toContain('hi');
  });

  it('forces safe link targets', () => {
    const out = sanitizeEmailHtml('<a href="https://example.com">go</a>');
    expect(out).toContain('target="_blank"');
    expect(out).toContain('rel="noopener noreferrer"');
  });

  it('removes script and event handlers', () => {
    const out = sanitizeEmailHtml('<p onclick="alert(1)">x</p><script>alert(2)</script>');
    expect(out.toLowerCase()).not.toContain('<script');
    expect(out.toLowerCase()).not.toContain('onclick');
  });
});
