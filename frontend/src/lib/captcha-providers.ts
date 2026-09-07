/** Captcha provider slugs shared by the login widget and the settings page. */

export type CaptchaProvider = 'turnstile' | 'hcaptcha' | 'recaptcha' | 'recaptcha-v3';

/** Invisible providers (reCAPTCHA v3) have no checkbox; tokens are fetched at submit time. */
export function isInvisibleCaptchaProvider(provider: string): boolean {
  return provider === 'recaptcha-v3';
}

/** Human-readable provider name for hints and labels. */
export function captchaProviderName(provider: string): string {
  switch (provider) {
    case 'turnstile':
      return 'Cloudflare Turnstile';
    case 'hcaptcha':
      return 'hCaptcha';
    case 'recaptcha':
      return 'Google reCAPTCHA';
    case 'recaptcha-v3':
      return 'Google reCAPTCHA v3';
    default:
      return provider;
  }
}
