/**
 * Pure decision logic for the Add/Edit Account form.
 *
 * Extracted from `settings-page.tsx` so the state machine that guards the
 * auth-method/protocol pairing is unit-testable: the probe is asynchronous
 * and debounced, so its result can land long after the user has filled the
 * form — every rule here exists because a late probe (or an early paste)
 * once silently re-wrote a user's choices.
 */

/** Fastmail API tokens authenticate as Bearer; app passwords as Basic. */
const FASTMAIL_TOKEN_RE = /^fmu1-/i;

/**
 * Next auth method after the secret field changes on the given protocol.
 * Params are plain strings: the form state is stringly-typed and the
 * returned value feeds straight back into it.
 */
export function nextAuthTypeOnSecret(current: string, protocol: string, secret: string): string {
  return protocol === 'jmap' && current !== 'bearer' && FASTMAIL_TOKEN_RE.test(secret)
    ? 'bearer'
    : current;
}

export interface ProbePatchInput {
  /** True once the user clicked the protocol toggle by hand. */
  protocolTouched: boolean;
  protocol: string;
  authType: string;
  /** Current secret field value (may have been pasted before the probe). */
  secret: string;
}

export interface ProbeFormPatch {
  protocol: 'jmap';
  authType: string;
}

/**
 * Form patch a JMAP-capable probe may apply: default the protocol to JMAP
 * (Lyra prefers JMAP) while honoring a hand-picked protocol, never
 * downgrading an explicitly chosen auth method, and upgrading a pasted
 * Fastmail token that could not flip at paste time (the form was still on
 * the IMAP default then). Returns `null` when nothing should change.
 */
export function probeFormPatch(
  probe: { jmapSupported?: boolean },
  form: ProbePatchInput,
): ProbeFormPatch | null {
  if (!probe.jmapSupported || form.protocolTouched) return null;
  const authType =
    form.authType !== 'bearer' && FASTMAIL_TOKEN_RE.test(form.secret) ? 'bearer' : form.authType;
  return { protocol: 'jmap', authType };
}

/** Backend probe sources are internal slugs — never show them raw to users. */
export function probeSourceLabel(locale: 'en' | 'zh', source?: string | null): string {
  const labels: Record<string, { en: string; zh: string }> = {
    mozilla_ispdb: { en: 'the Mozilla ISP database', zh: 'Mozilla ISP 数据库' },
    common_patterns: { en: 'built-in provider settings', zh: '内置服务商配置' },
    microsoft_domain: { en: 'Microsoft domain settings', zh: 'Microsoft 域名配置' },
    yandex_domain: { en: 'Yandex domain settings', zh: 'Yandex 域名配置' },
  };
  if (!source) return locale === 'zh' ? '未知来源' : 'an unknown source';
  return labels[source] ? labels[source][locale] : source;
}

/** i18n key for the mail-OAuth callback banner, by backend detail code. */
export function oauthErrorKey(detail: string | null): string {
  if (detail === 'oauth_denied') return 'settings.accounts.oauthDenied';
  if (detail === 'token_exchange') return 'settings.accounts.oauthTokenExchange';
  return 'settings.accounts.oauthError';
}
