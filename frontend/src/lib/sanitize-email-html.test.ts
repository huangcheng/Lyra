/**
 * @vitest-environment jsdom
 */
import { describe, expect, it } from 'vitest';

import { sanitizeEmailHtml, sanitizeEmailHtmlForFrame } from './sanitize-email-html';

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

describe('sanitizeEmailHtmlForFrame', () => {
  it('keeps style blocks and class attributes (iframe isolates them)', () => {
    const out = sanitizeEmailHtmlForFrame(
      '<style>.card{padding:32px}</style><div class="card" style="color:blue">hi</div>',
    );
    expect(out).toContain('<style>.card{padding:32px}</style>');
    expect(out).toContain('class="card"');
    expect(out).toContain('style="color:blue"');
  });

  it('still removes active content and event handlers', () => {
    const out = sanitizeEmailHtmlForFrame(
      '<p onclick="alert(1)">x</p><script>alert(2)</script>' +
        '<iframe src="https://evil"></iframe><form action="https://evil"></form>',
    );
    const low = out.toLowerCase();
    expect(low).not.toContain('<script');
    expect(low).not.toContain('onclick');
    expect(low).not.toContain('<iframe');
    expect(low).not.toContain('<form');
  });

  it('forces safe link targets', () => {
    const out = sanitizeEmailHtmlForFrame('<a href="https://example.com">go</a>');
    expect(out).toContain('target="_blank"');
    expect(out).toContain('rel="noopener noreferrer"');
  });
});
