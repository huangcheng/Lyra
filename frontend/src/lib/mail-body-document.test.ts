import { describe, expect, it } from 'vitest';

import { buildMailBodyDocument } from './mail-body-document';

describe('buildMailBodyDocument', () => {
  it('wraps the body in a full document with the sheet defaults', () => {
    const doc = buildMailBodyDocument('<p>hi</p>');
    expect(doc).toContain('<!doctype html>');
    expect(doc).toContain('<base target="_blank">');
    expect(doc).toContain('background: #ffffff');
    expect(doc).toContain('</head><body><p>hi</p></body></html>');
  });

  it('keeps author markup after the shell styles so it wins ties', () => {
    const doc = buildMailBodyDocument('<style>.card{padding:32px}</style>');
    const shell = doc.indexOf('overflow-anchor: auto');
    const author = doc.indexOf('.card{padding:32px}');
    expect(shell).toBeGreaterThanOrEqual(0);
    expect(author).toBeGreaterThan(shell);
  });
});
