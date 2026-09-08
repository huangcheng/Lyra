import { describe, expect, it, vi } from 'vitest';

vi.mock('@/lib/api-client', () => ({ api: vi.fn().mockResolvedValue({}) }));

import { api } from '@/lib/api-client';
import { requestAiDraft, saveAiSettings } from '@/lib/ai-api';

const mockedApi = vi.mocked(api);

describe('saveAiSettings', () => {
  it('PATCHes only the provided fields; key omitted keeps the stored one', async () => {
    await saveAiSettings({ enabled: true, dialect: 'openai_chat' });
    expect(mockedApi).toHaveBeenCalledWith(
      '/settings/ai',
      expect.objectContaining({
        method: 'PUT',
        body: JSON.stringify({ enabled: true, dialect: 'openai_chat' }),
      }),
    );
  });
});

describe('requestAiDraft', () => {
  it('posts the message id and mode', async () => {
    await requestAiDraft('m-1', 'reply', 'keep it short');
    expect(mockedApi).toHaveBeenCalledWith(
      '/ai/draft',
      expect.objectContaining({
        method: 'POST',
        body: JSON.stringify({ messageId: 'm-1', mode: 'reply', instruction: 'keep it short' }),
      }),
    );
  });

  it('omits a blank instruction', async () => {
    await requestAiDraft('m-1', 'forward');
    expect(mockedApi).toHaveBeenLastCalledWith(
      '/ai/draft',
      expect.objectContaining({
        body: JSON.stringify({ messageId: 'm-1', mode: 'forward', instruction: undefined }),
      }),
    );
  });
});
