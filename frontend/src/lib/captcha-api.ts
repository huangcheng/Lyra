import { api } from '@/lib/api-client';

export type CaptchaProviderSetting =
  'none' | 'turnstile' | 'hcaptcha' | 'recaptcha' | 'recaptcha-v3';

export interface CaptchaProviderPair {
  siteKey: string;
  /** The secret is write-only; the server only reports whether one is set. */
  hasSecret: boolean;
}

export interface CaptchaSettings {
  /** The provider that currently protects login, or "none". */
  active: CaptchaProviderSetting;
  /** Known key pairs keyed by provider; switching providers keeps them all. */
  providers: Partial<Record<CaptchaProviderSetting, CaptchaProviderPair>>;
  /** Where the config came from: a saved setting or env vars. */
  source: 'settings' | 'env';
}

export interface CaptchaPairUpdate {
  siteKey?: string;
  /** Omit/empty to keep the stored secret for that provider. */
  secret?: string;
}

export async function fetchCaptchaSettings(): Promise<CaptchaSettings> {
  return api<CaptchaSettings>('/settings/captcha');
}

export async function saveCaptchaSettings(input: {
  active: CaptchaProviderSetting;
  /** Per-provider updates; a pair is upserted, omitted providers stay as-is. */
  providers?: Partial<Record<CaptchaProviderSetting, CaptchaPairUpdate | null>>;
}): Promise<CaptchaSettings> {
  return api<CaptchaSettings>('/settings/captcha', {
    method: 'PUT',
    body: JSON.stringify(input),
  });
}
