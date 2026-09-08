/**
 * AI assist settings + draft assist (`/api/v1/settings/ai`, `/api/v1/ai/draft`).
 * The API key is write-only: reads get `hasKey`, never the key itself.
 */

import { api } from '@/lib/api-client';

export type AiDialect = 'openai_chat' | 'openai_responses' | 'anthropic';

export interface AiFeatures {
  draftReply: boolean;
  /** P3-lite: assistant chat bubble with the mail-search tool. */
  assistant: boolean;
}

export interface AiChatMessage {
  role: 'user' | 'assistant';
  content: string;
  createdAt: string;
}

export interface AiSettings {
  enabled: boolean;
  dialect: AiDialect;
  baseUrl: string;
  model: string;
  hasKey: boolean;
  features: AiFeatures;
}

export type AiSettingsUpdate = Partial<
  Omit<AiSettings, 'hasKey' | 'dialect' | 'features'> & {
    dialect: AiDialect;
    features: AiFeatures;
    /** Omitted ⇒ keep the stored key; empty string clears it. */
    apiKey: string;
  }
>;

export async function fetchAiSettings(): Promise<AiSettings> {
  return api<AiSettings>('/settings/ai');
}

export async function saveAiSettings(update: AiSettingsUpdate): Promise<AiSettings> {
  return api<AiSettings>('/settings/ai', {
    method: 'PUT',
    body: JSON.stringify(update),
  });
}

export async function testAiConnection(): Promise<{ ok: boolean; reply: string; model: string }> {
  return api<{ ok: boolean; reply: string; model: string }>('/settings/ai/test', {
    method: 'POST',
  });
}

export async function fetchAiChat(): Promise<AiChatMessage[]> {
  return api<AiChatMessage[]>('/ai/chat');
}

export async function sendAiChat(message: string, messageId?: string): Promise<string> {
  const res = await api<{ reply: string }>('/ai/chat', {
    method: 'POST',
    body: JSON.stringify({ message, messageId }),
  });
  return res.reply;
}

export async function clearAiChat(): Promise<void> {
  await api('/ai/chat', { method: 'DELETE' });
}

/** Suggest reply/forward text for one message (user always edits and sends). */
export async function requestAiDraft(
  messageId: string,
  mode: 'reply' | 'forward',
  instruction?: string,
): Promise<string> {
  const res = await api<{ text: string }>('/ai/draft', {
    method: 'POST',
    body: JSON.stringify({ messageId, mode, instruction }),
  });
  return res.text;
}
