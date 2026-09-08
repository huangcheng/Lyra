/**
 * Pending assistant actions: confirm-first proposal cards in the chat
 * panel. The assistant never mutates — the user's click executes via the
 * existing compose/action endpoints.
 */

import type { SupportedLocale } from '@/i18n';

export interface OpenDraftAction {
  type: 'openDraft';
  to: string;
  subject: string;
  body: string;
}

export type MoveActionName = 'spam' | 'archive' | 'trash' | 'notSpam';

export interface MoveMessageAction {
  type: 'moveMessage';
  messageId: string;
  action: MoveActionName;
}

export type PendingAction = OpenDraftAction | MoveMessageAction;

/** One-line card copy for a proposal. */
export function describePendingAction(a: PendingAction, locale: SupportedLocale): string {
  if (a.type === 'openDraft') {
    return locale === 'zh' ? `草稿 → ${a.to}` : `Draft to ${a.to}`;
  }
  const names: Record<MoveActionName, [string, string]> = {
    spam: ['移到垃圾邮件', 'Move to junk'],
    archive: ['归档', 'Archive'],
    trash: ['移到废纸篓', 'Move to trash'],
    notSpam: ['标为非垃圾邮件', 'Mark as not spam'],
  };
  const [zh, en] = names[a.action];
  return locale === 'zh' ? zh : en;
}
