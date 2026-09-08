import { describe, expect, it } from 'vitest';

import { suggestLabel, verdictTone } from '@/lib/spam-assist';

describe('verdictTone', () => {
  it('marks confident spam and confident ham', () => {
    expect(verdictTone({ isSpam: true, confidence: 80 })).toBe('spam');
    expect(verdictTone({ isSpam: false, confidence: 80 })).toBe('clean');
  });

  it('marks low-confidence verdicts unsure regardless of lean', () => {
    expect(verdictTone({ isSpam: true, confidence: 45 })).toBe('unsure');
    expect(verdictTone({ isSpam: false, confidence: 10 })).toBe('unsure');
  });
});

describe('suggestLabel', () => {
  it('localizes the three tones', () => {
    expect(suggestLabel('spam', 'zh')).toMatch(/垃圾/);
    expect(suggestLabel('clean', 'en')).toMatch(/not spam/i);
    expect(suggestLabel('unsure', 'zh')).toMatch(/不确定/);
  });
});
