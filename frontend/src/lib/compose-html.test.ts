import { describe, expect, it } from 'vitest';

import { forwardHtml, quotedReplyHtml, signatureHtml, textToHtml } from '@/lib/compose-html';
import { htmlToText } from '@/lib/html-text';

describe('textToHtml', () => {
  it('escapes and wraps paragraphs', () => {
    expect(textToHtml('a < b\n\nc')).toBe('<p>a &lt; b</p><p>c</p>');
  });

  it('empty input gives empty output', () => {
    expect(textToHtml('   ')).toBe('');
  });
});

describe('signatureHtml', () => {
  it('plain text becomes a -- block', () => {
    expect(signatureHtml('Jane Doe\nAcme')).toBe('<p>--<br>Jane Doe<br>Acme</p>');
  });

  it('html signatures pass through; empty collapses', () => {
    expect(signatureHtml('<p>sig</p>')).toBe('<p>sig</p>');
    expect(signatureHtml(null)).toBe('');
  });
});

describe('quotedReplyHtml', () => {
  it('attributes and blockquotes the original', () => {
    const html = quotedReplyHtml(
      { fromName: 'Bob', fromEmail: 'bob@example.com', date: '2026-08-27', bodyHtml: '<p>hi</p>' },
      'Jane',
    );
    expect(html).toContain('On 2026-08-27, Bob wrote:');
    expect(html).toContain('<blockquote><p>hi</p></blockquote>');
    expect(html).toContain('Jane');
  });

  it('leads with a blank paragraph so the user can type above the quote', () => {
    const html = quotedReplyHtml(
      { fromName: 'Bob', fromEmail: 'bob@example.com', date: '2026-08-27', bodyHtml: '<p>hi</p>' },
      undefined,
    );
    expect(html.startsWith('<p><br></p>')).toBe(true);
    expect(html.indexOf('<p><br></p>')).toBeLessThan(html.indexOf('wrote:'));
  });
});

describe('forwardHtml', () => {
  it('includes forwarded header and original', () => {
    const html = forwardHtml(
      { fromName: 'Bob', fromEmail: 'bob@example.com', date: 'x', bodyText: 'yo' },
      undefined,
    );
    expect(html).toContain('-------- Forwarded message --------');
    expect(html).toContain('bob@example.com');
    expect(html).toContain('yo');
  });

  it('leads with a blank paragraph above the forward header', () => {
    const html = forwardHtml(
      { fromName: 'Bob', fromEmail: 'bob@example.com', date: 'x', bodyText: 'yo' },
      undefined,
    );
    expect(html.startsWith('<p><br></p>')).toBe(true);
  });
});
describe('htmlToText', () => {
  it('converts paragraphs, breaks, lists, links, quotes', () => {
    const html =
      '<p>hello</p><p>line1<br>line2</p><ul><li>one</li><li>two</li></ul>' +
      '<blockquote><p>quoted</p></blockquote><p><a href="https://x.example">site</a></p>';
    const text = htmlToText(html);
    expect(text).toContain('hello');
    expect(text).toContain('line1\nline2');
    expect(text).toContain('• one');
    expect(text).toContain('• two');
    expect(text).toContain('> quoted');
    expect(text).toContain('site <https://x.example>');
  });

  it('round-trips textToHtml output', () => {
    const src = 'first para\n\nsecond para';
    expect(htmlToText(textToHtml(src)).replace(/\n+/g, '\n')).toBe(src.replace(/\n+/g, '\n'));
  });
});
