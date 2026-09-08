import { describe, expect, it } from 'vitest';

import { assistantAvailable, summarizePrompt } from '@/lib/assistant';

const base = {
  enabled: true,
  dialect: 'openai_chat' as const,
  baseUrl: 'https://x/v1',
  model: 'qwen3-max',
  hasKey: true,
  features: { draftReply: false, assistant: true },
};

describe('assistantAvailable', () => {
  it('is true only when enabled + feature flag + key are all set', () => {
    expect(assistantAvailable(base)).toBe(true);
    expect(assistantAvailable({ ...base, enabled: false })).toBe(false);
    expect(assistantAvailable({ ...base, hasKey: false })).toBe(false);
    expect(assistantAvailable({ ...base, features: { ...base.features, assistant: false } })).toBe(
      false,
    );
  });
});

describe('summarizePrompt', () => {
  it('is localized per locale', () => {
    expect(summarizePrompt('en')).toMatch(/summar/i);
    expect(summarizePrompt('zh')).toMatch(/总结/);
  });
});
