/**
 * Captcha widget for login/bootstrap forms.
 *
 * Providers: Cloudflare Turnstile, hCaptcha, Google reCAPTCHA (v2 checkbox).
 * All three expose a render/reset API with `sitekey` + token callbacks.
 */

import { useEffect, useId, useRef, type RefObject } from 'react';

import type { CaptchaProvider } from '@/lib/captcha-providers';

interface CaptchaApi {
  render: (
    container: string | HTMLElement,
    options: {
      sitekey: string;
      callback?: (token: string) => void;
      'expired-callback'?: () => void;
      'error-callback'?: () => void;
    },
  ) => string | number;
  reset: (widgetId?: string | number) => void;
  remove?: (widgetId: string | number) => void;
}

/** reCAPTCHA v3 API surface (invisible, score-based). */
interface RecaptchaV3Api {
  ready: (cb: () => void) => void;
  execute: (siteKey: string, options: { action: string }) => Promise<string>;
}

declare global {
  interface Window {
    turnstile?: CaptchaApi;
    hcaptcha?: CaptchaApi;
    grecaptcha?: CaptchaApi & Partial<RecaptchaV3Api>;
    __lyraHcaptchaOnload?: () => void;
    __lyraRecaptchaOnload?: () => void;
  }
}

interface ProviderSpec {
  scriptSrc: string | ((siteKey: string) => string);
  /** Window global holding the render API. */
  globalName: 'turnstile' | 'hcaptcha' | 'grecaptcha';
  /** If set, the script signals readiness via this window callback. */
  onloadName?: '__lyraHcaptchaOnload' | '__lyraRecaptchaOnload';
  /** Invisible providers (reCAPTCHA v3) execute on demand, no checkbox UI. */
  invisible?: boolean;
}

const PROVIDERS: Record<CaptchaProvider, ProviderSpec> = {
  turnstile: {
    scriptSrc: 'https://challenges.cloudflare.com/turnstile/v0/api.js',
    globalName: 'turnstile',
  },
  hcaptcha: {
    scriptSrc: 'https://js.hcaptcha.com/1/api.js?render=explicit&onload=__lyraHcaptchaOnload',
    globalName: 'hcaptcha',
    onloadName: '__lyraHcaptchaOnload',
  },
  recaptcha: {
    scriptSrc:
      'https://www.google.com/recaptcha/api.js?render=explicit&onload=__lyraRecaptchaOnload',
    globalName: 'grecaptcha',
    onloadName: '__lyraRecaptchaOnload',
  },
  'recaptcha-v3': {
    scriptSrc: (siteKey) => `https://www.google.com/recaptcha/api.js?render=${siteKey}`,
    globalName: 'grecaptcha',
    invisible: true,
  },
};

const scriptPromises = new Map<CaptchaProvider, Promise<CaptchaApi & Partial<RecaptchaV3Api>>>();

function loadCaptchaScript(
  provider: CaptchaProvider,
  siteKey: string,
): Promise<CaptchaApi & Partial<RecaptchaV3Api>> {
  const spec = PROVIDERS[provider];
  const existing = window[spec.globalName];
  if (existing) return Promise.resolve(existing);
  const inflight = scriptPromises.get(provider);
  if (inflight) return inflight;

  const promise = new Promise<CaptchaApi & Partial<RecaptchaV3Api>>((resolve, reject) => {
    const ready = () => {
      const api = window[spec.globalName];
      if (api) {
        resolve(api);
      } else {
        reject(new Error(`${provider} script loaded without API global`));
      }
    };
    // Providers with explicit render signal readiness through a callback
    // instead of the script's load event.
    if (spec.onloadName) {
      window[spec.onloadName] = ready;
    }
    const script = document.createElement('script');
    script.src = typeof spec.scriptSrc === 'function' ? spec.scriptSrc(siteKey) : spec.scriptSrc;
    script.async = true;
    script.defer = true;
    if (!spec.onloadName) {
      script.onload = ready;
    }
    script.onerror = () => reject(new Error(`${provider} script failed`));
    document.head.appendChild(script);
  });
  scriptPromises.set(provider, promise);
  return promise;
}

interface CaptchaWidgetProps {
  provider: CaptchaProvider;
  siteKey: string;
  onToken: (token: string | null) => void;
  resetKey?: number;
  className?: string;
  /**
   * Invisible providers (reCAPTCHA v3) expose a submit-time token fetcher
   * through this ref — there is no user interaction to wait for.
   */
  fetchTokenRef?: RefObject<CaptchaTokenFetcher | null>;
}

/** Fetches a fresh captcha token on demand; `null` when verification fails. */
export type CaptchaTokenFetcher = () => Promise<string | null>;

export function CaptchaWidget({
  provider,
  siteKey,
  onToken,
  resetKey = 0,
  className,
  fetchTokenRef,
}: CaptchaWidgetProps) {
  const containerId = useId().replace(/:/g, '');
  const widgetIdRef = useRef<string | number | null>(null);
  const onTokenRef = useRef(onToken);
  useEffect(() => {
    onTokenRef.current = onToken;
  }, [onToken]);

  useEffect(() => {
    let cancelled = false;

    // Invisible providers (reCAPTCHA v3): no checkbox UI. The form fetches a
    // fresh token at submit time through fetchTokenRef; the script is
    // preloaded here so submit is fast.
    if (PROVIDERS[provider].invisible) {
      if (fetchTokenRef) {
        fetchTokenRef.current = async () => {
          try {
            const api = await loadCaptchaScript(provider, siteKey);
            if (!api.execute) return null;
            return await api.execute(siteKey, { action: 'login' });
          } catch {
            return null;
          }
        };
      }
      void loadCaptchaScript(provider, siteKey).catch(() => {});
      return () => {
        cancelled = true;
        if (fetchTokenRef) fetchTokenRef.current = null;
      };
    }

    void loadCaptchaScript(provider, siteKey)
      .then((api) => {
        if (cancelled) return;
        const container = document.getElementById(containerId);
        if (!container) return;
        container.innerHTML = '';
        widgetIdRef.current = api.render(container, {
          sitekey: siteKey,
          callback: (token) => onTokenRef.current(token),
          'expired-callback': () => onTokenRef.current(null),
          'error-callback': () => onTokenRef.current(null),
        });
      })
      .catch(() => onTokenRef.current(null));

    return () => {
      cancelled = true;
      const id = widgetIdRef.current;
      const api = window[PROVIDERS[provider].globalName];
      if (id !== null && api) {
        if (api.remove) {
          api.remove(id);
        } else {
          // reCAPTCHA has no remove(); resetting releases the token.
          api.reset(id);
          const container = document.getElementById(containerId);
          if (container) container.innerHTML = '';
        }
        widgetIdRef.current = null;
      }
    };
  }, [containerId, provider, siteKey, resetKey]);

  // Invisible providers render nothing (Google shows its own badge).
  if (PROVIDERS[provider].invisible) return null;

  return <div id={containerId} className={className ?? 'flex min-h-[65px] justify-center'} />;
}
