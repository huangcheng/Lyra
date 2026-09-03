if (typeof URL.createObjectURL !== 'function') {
  let n = 0;
  URL.createObjectURL = () => `blob:mock-${(n += 1)}`;
}

import { describe, expect, it } from 'vitest';

import {
  extractInlineImages,
  fileToBase64,
  newContentId,
  resolveInlineSources,
} from '@/lib/inline-images';

const png = new File([new Uint8Array([1, 2, 3])], 'photo.png', { type: 'image/png' });

describe('newContentId', () => {
  it('is a msg-id-style value without angle brackets', () => {
    const cid = newContentId();
    expect(cid).toMatch(/^[0-9a-f-]+@lyra$/);
  });
});

describe('extractInlineImages', () => {
  it('rewrites tracked blob URLs to cid and collects parts once', () => {
    const map = new Map([['blob:x', { file: png, contentId: 'c1@lyra' }]]);
    const { html, parts } = extractInlineImages(
      '<p>a</p><img src="blob:x"><img src="blob:x">',
      map,
    );
    expect(html).toBe('<p>a</p><img src="cid:c1@lyra"><img src="cid:c1@lyra">');
    expect(parts).toHaveLength(1);
    expect(parts[0]).toMatchObject({
      filename: 'photo.png',
      contentType: 'image/png',
      contentId: 'c1@lyra',
    });
  });

  it('leaves unknown blob URLs and remote URLs untouched', () => {
    const { html, parts } = extractInlineImages(
      '<img src="blob:unknown"><img src="https://example.com/x.png">',
      new Map(),
    );
    expect(html).toContain('src="blob:unknown"');
    expect(html).toContain('src="https://example.com/x.png"');
    expect(parts).toHaveLength(0);
  });

  it('no blob URLs at all → unchanged input, no parts', () => {
    const { html, parts } = extractInlineImages('<p>plain</p>', new Map());
    expect(html).toBe('<p>plain</p>');
    expect(parts).toHaveLength(0);
  });
});

describe('resolveInlineSources', () => {
  const source = { id: 'att1', filename: 'a.png', contentType: 'image/png', contentId: 'c1@lyra' };
  const fetchBlob = async () => new Blob([new Uint8Array([9])], { type: 'image/png' });

  it('rewrites matching cid refs to object URLs and maps them back', async () => {
    const { html, urlToImage } = await resolveInlineSources(
      '<img src="cid:c1@lyra">',
      [source],
      fetchBlob,
    );
    expect(html).not.toContain('cid:');
    const [url, entry] = [...urlToImage.entries()][0];
    expect(html).toContain(`src="${url}"`);
    expect(entry.contentId).toBe('c1@lyra');
    expect(entry.file.type).toBe('image/png');
    expect(entry.file.name).toBe('a.png');
  });

  it('skips sources whose cid is not referenced', async () => {
    const { html, urlToImage } = await resolveInlineSources('<p>none</p>', [source], fetchBlob);
    expect(html).toBe('<p>none</p>');
    expect(urlToImage.size).toBe(0);
  });

  it('degrades when a fetch fails: cid stays, other images still resolve', async () => {
    const sources = [source, { id: 'att2', filename: 'b.png', contentId: 'c2@lyra' }];
    const flaky = async (id: string) => {
      if (id === 'att1') throw new Error('gone');
      return new Blob([new Uint8Array([1])]);
    };
    const { html, urlToImage } = await resolveInlineSources(
      '<img src="cid:c1@lyra"><img src="cid:c2@lyra">',
      sources,
      flaky,
    );
    expect(html).toContain('src="cid:c1@lyra"');
    expect(urlToImage.size).toBe(1);
  });
});

describe('fileToBase64', () => {
  it('round-trips bytes', async () => {
    const b64 = await fileToBase64(png);
    expect(b64).toBe('AQID');
  });
});
