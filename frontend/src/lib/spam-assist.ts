/**
 * AI spam-assist (suggest mode) pure logic: verdict tone classification
 * and localized labels. The reader wires these to /ai/spam/suggest.
 */

import type { SupportedLocale } from '@/i18n';

export interface SpamSuggestion {
  isSpam: boolean;
  confidence: number;
  reason: string;
}

export type VerdictTone = 'spam' | 'clean' | 'unsure';

/** Confidence at or above this leans decisive; below it we show "unsure". */
export const SPAM_CONFIDENCE_FLOOR = 60;

export function verdictTone(v: Pick<SpamSuggestion, 'isSpam' | 'confidence'>): VerdictTone {
  if (v.confidence < SPAM_CONFIDENCE_FLOOR) return 'unsure';
  return v.isSpam ? 'spam' : 'clean';
}

export function suggestLabel(tone: VerdictTone, locale: SupportedLocale): string {
  if (locale === 'zh') {
    return tone === 'spam'
      ? 'AI 判定：垃圾邮件'
      : tone === 'clean'
        ? 'AI 判定：非垃圾邮件'
        : 'AI 不确定';
  }
  return tone === 'spam'
    ? 'AI verdict: spam'
    : tone === 'clean'
      ? 'AI verdict: not spam'
      : 'AI is unsure';
}
