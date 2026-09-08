/**
 * Assistant widget pure logic: availability rule and localized quick
 * prompts. UI state lives in the widget; these stay testable.
 */

import type { AiSettings } from '@/lib/ai-api';
import type { SupportedLocale } from '@/i18n';

/** The bubble shows only when the assistant can actually run. */
export function assistantAvailable(s: AiSettings | null): boolean {
  return Boolean(s?.enabled && s?.hasKey && s?.features.assistant);
}

/** Quick action when a message is open in the reader. */
export function summarizePrompt(locale: SupportedLocale): string {
  return locale === 'zh'
    ? '请总结这封邮件：关键信息、对方诉求、需要我做什么。'
    : 'Summarize this email: the key points, what the sender wants, and what I need to do.';
}
