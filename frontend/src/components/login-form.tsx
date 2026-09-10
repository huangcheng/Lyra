/**
 * Login / bootstrap / TOTP — Lyra v2 login card.
 */

import type { ComponentProps, FormEvent } from 'react';
import { useEffect, useRef, useState } from 'react';

import { CaptchaWidget, type CaptchaTokenFetcher } from '@/components/captcha-widget';
import { StampLogo } from '@/components/stamp-logo';
import { Button } from '@/components/ui/button';
import { InlineOrb } from '@/components/ui/orb-state';
import { Field, FieldError, FieldGroup, FieldLabel } from '@/components/ui/field';
import { Input } from '@/components/ui/input';
import { t } from '@/i18n';
import {
  captchaProviderName,
  isInvisibleCaptchaProvider,
  type CaptchaProvider,
} from '@/lib/captcha-providers';
import { cn } from '@/lib/utils';
import type { CaptchaPublic } from '@/machines/auth';
import { useUIStore } from '@/stores/ui';

interface LoginFormProps {
  onLogin: (username: string, password: string, captchaToken?: string | null) => void;
  onBootstrap: (
    username: string,
    password: string,
    displayName?: string,
    locale?: string,
    captchaToken?: string | null,
  ) => void;
  onTotpVerify: (code: string) => void;
  onRetry?: () => void;
  error: string | null;
  hasUser: boolean | null;
  requiresTotp: boolean;
  captcha: CaptchaPublic | null;
}

export function LoginForm({
  className,
  onLogin,
  onBootstrap,
  onTotpVerify,
  onRetry,
  error,
  hasUser,
  requiresTotp,
  captcha,
  ...props
}: LoginFormProps & ComponentProps<'div'>) {
  const locale = useUIStore((s) => s.locale);
  const setLocale = useUIStore((s) => s.setLocale);
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [confirmPassword, setConfirmPassword] = useState('');
  const [displayName, setDisplayName] = useState('');
  const [totpCode, setTotpCode] = useState('');
  const [validationError, setValidationError] = useState<string | null>(null);
  const [captchaToken, setCaptchaToken] = useState<string | null>(null);
  const [captchaResetKey, setCaptchaResetKey] = useState(0);
  const [captchaVerifying, setCaptchaVerifying] = useState(false);
  const captchaFetchRef = useRef<CaptchaTokenFetcher | null>(null);

  const formError = error || validationError;
  const needsCaptcha = Boolean(captcha?.provider && captcha.siteKey);
  const invisibleCaptcha = captcha ? isInvisibleCaptchaProvider(captcha.provider) : false;

  useEffect(() => {
    if (error && needsCaptcha) {
      setCaptchaToken(null);
      setCaptchaResetKey((k) => k + 1);
    }
  }, [error, needsCaptcha]);

  /**
   * Resolve the token to submit with: `undefined` when no captcha is active,
   * a token on success, or `null` on failure. Interactive providers use the
   * token from the checkbox widget; invisible ones fetch a fresh token now.
   */
  const resolveCaptchaToken = async (): Promise<string | null | undefined> => {
    if (!needsCaptcha) return undefined;
    if (!invisibleCaptcha) return captchaToken;
    const fetcher = captchaFetchRef.current;
    if (!fetcher) return null;
    setCaptchaVerifying(true);
    try {
      return await fetcher();
    } finally {
      setCaptchaVerifying(false);
    }
  };

  const handleLoginSubmit = (e: FormEvent) => {
    e.preventDefault();
    setValidationError(null);
    if (!username || !password) {
      setValidationError(t(locale, 'common.error'));
      return;
    }
    void resolveCaptchaToken().then((token) => {
      if (token === null) {
        setValidationError(t(locale, 'auth.captchaFailed'));
        return;
      }
      if (needsCaptcha && !token) {
        setValidationError(t(locale, 'auth.captchaRequired'));
        return;
      }
      onLogin(username, password, token);
    });
  };

  const handleBootstrapSubmit = (e: FormEvent) => {
    e.preventDefault();
    setValidationError(null);
    if (!username || !password) {
      setValidationError(t(locale, 'common.error'));
      return;
    }
    if (password !== confirmPassword) {
      setValidationError(t(locale, 'auth.passwordMismatch'));
      return;
    }
    void resolveCaptchaToken().then((token) => {
      if (token === null) {
        setValidationError(t(locale, 'auth.captchaFailed'));
        return;
      }
      if (needsCaptcha && !token) {
        setValidationError(t(locale, 'auth.captchaRequired'));
        return;
      }
      onBootstrap(username, password, displayName || undefined, locale, token);
    });
  };

  const handleTotpSubmit = (e: FormEvent) => {
    e.preventDefault();
    setValidationError(null);
    if (!totpCode || totpCode.length !== 6) {
      setValidationError(t(locale, 'auth.totpCode'));
      return;
    }
    onTotpVerify(totpCode);
  };

  let title = t(locale, 'auth.loginTitle');
  let description = t(locale, 'auth.loginDescription');
  if (hasUser === false) {
    title = t(locale, 'auth.bootstrapTitle');
    description = t(locale, 'auth.bootstrapDescription');
  } else if (requiresTotp) {
    title = t(locale, 'auth.totpTitle');
    description = t(locale, 'auth.totpDescription');
  }

  const submitButtonClass = 'h-[42px] w-full rounded-lg font-medium';

  const captchaField =
    needsCaptcha && captcha ? (
      <Field>
        <CaptchaWidget
          provider={captcha.provider as CaptchaProvider}
          siteKey={captcha.siteKey}
          onToken={setCaptchaToken}
          resetKey={captchaResetKey}
          className="flex justify-center"
          fetchTokenRef={captchaFetchRef}
        />
        <p className="pt-1 text-[11px] text-muted-foreground">
          {t(locale, 'auth.captchaHint', { provider: captchaProviderName(captcha.provider) })}
        </p>
      </Field>
    ) : null;

  return (
    <div className={cn('flex flex-col', className)} {...props}>
      <div className="w-full max-w-[380px] animate-in fade-in slide-in-from-bottom-2 zoom-in-[0.98] duration-300 ease-out-quart rounded-xl border border-border bg-card px-9 pb-8 pt-10">
        <div className="flex items-center justify-center gap-3">
          <StampLogo size={40} className="rounded-[9px]" />
          <span className="font-brand text-[28px]">Lyra</span>
        </div>
        <p className="pb-7 pt-2 text-center text-[13px] text-muted-foreground">
          {t(locale, 'auth.tagline')}
        </p>

        {hasUser === null ? (
          <FieldGroup>
            {formError ? (
              <FieldError>{formError}</FieldError>
            ) : (
              <InlineOrb
                state="connecting"
                label={t(locale, 'common.loading')}
                className="text-sm text-muted-foreground"
              />
            )}
            {formError && onRetry ? (
              <Field>
                <Button
                  type="button"
                  variant="outline"
                  className={submitButtonClass}
                  onClick={onRetry}
                >
                  {t(locale, 'common.retry')}
                </Button>
              </Field>
            ) : null}
          </FieldGroup>
        ) : (
          <>
            {(hasUser === false || requiresTotp) && (
              <div className="pb-5 text-center">
                <p className="font-medium leading-none">{title}</p>
                <p className="pt-2 text-sm text-muted-foreground">{description}</p>
              </div>
            )}
            {requiresTotp ? (
              <form onSubmit={handleTotpSubmit}>
                <FieldGroup>
                  <Field>
                    <FieldLabel htmlFor="totp-code">{t(locale, 'auth.totpCode')}</FieldLabel>
                    <Input
                      id="totp-code"
                      type="text"
                      inputMode="numeric"
                      pattern="[0-9]*"
                      maxLength={6}
                      value={totpCode}
                      onChange={(e) => setTotpCode(e.target.value.replace(/\D/g, ''))}
                      placeholder="000000"
                      autoFocus
                      required
                    />
                  </Field>
                  {formError ? <FieldError>{formError}</FieldError> : null}
                  <Field>
                    <Button type="submit" variant="outline" className={submitButtonClass}>
                      {t(locale, 'auth.totpVerify')}
                    </Button>
                  </Field>
                </FieldGroup>
              </form>
            ) : !hasUser ? (
              <form onSubmit={handleBootstrapSubmit}>
                <FieldGroup>
                  <Field>
                    <FieldLabel htmlFor="username">{t(locale, 'auth.username')}</FieldLabel>
                    <Input
                      id="username"
                      value={username}
                      onChange={(e) => setUsername(e.target.value)}
                      autoComplete="username"
                      autoFocus
                      required
                    />
                  </Field>
                  <Field>
                    <FieldLabel htmlFor="display-name">{t(locale, 'auth.displayName')}</FieldLabel>
                    <Input
                      id="display-name"
                      value={displayName}
                      onChange={(e) => setDisplayName(e.target.value)}
                      autoComplete="name"
                    />
                  </Field>
                  <Field>
                    <FieldLabel htmlFor="password">{t(locale, 'auth.password')}</FieldLabel>
                    <Input
                      id="password"
                      type="password"
                      value={password}
                      onChange={(e) => setPassword(e.target.value)}
                      autoComplete="new-password"
                      required
                    />
                  </Field>
                  <Field>
                    <FieldLabel htmlFor="confirm-password">
                      {t(locale, 'auth.confirmPassword')}
                    </FieldLabel>
                    <Input
                      id="confirm-password"
                      type="password"
                      value={confirmPassword}
                      onChange={(e) => setConfirmPassword(e.target.value)}
                      autoComplete="new-password"
                      required
                    />
                  </Field>
                  {captchaField}
                  {formError ? <FieldError>{formError}</FieldError> : null}
                  <Field>
                    <Button
                      type="submit"
                      variant="outline"
                      className={submitButtonClass}
                      disabled={captchaVerifying}
                    >
                      {t(locale, 'auth.createAccount')}
                    </Button>
                  </Field>
                </FieldGroup>
              </form>
            ) : (
              <form onSubmit={handleLoginSubmit}>
                <FieldGroup>
                  <Field>
                    <FieldLabel htmlFor="username">{t(locale, 'auth.username')}</FieldLabel>
                    <Input
                      id="username"
                      value={username}
                      onChange={(e) => setUsername(e.target.value)}
                      autoComplete="username"
                      placeholder={t(locale, 'auth.username')}
                      autoFocus
                      required
                    />
                  </Field>
                  <Field>
                    <FieldLabel htmlFor="password">{t(locale, 'auth.password')}</FieldLabel>
                    <Input
                      id="password"
                      type="password"
                      value={password}
                      onChange={(e) => setPassword(e.target.value)}
                      autoComplete="current-password"
                      placeholder={t(locale, 'auth.password')}
                      required
                    />
                  </Field>
                  {captchaField}
                  {formError ? <FieldError>{formError}</FieldError> : null}
                  <Field>
                    <Button
                      type="submit"
                      variant="outline"
                      className={submitButtonClass}
                      disabled={captchaVerifying}
                    >
                      {t(locale, 'auth.login')}
                    </Button>
                  </Field>
                </FieldGroup>
              </form>
            )}
          </>
        )}

        <div className="flex items-center gap-1.5 pt-5 text-xs">
          <button
            type="button"
            className={cn(
              'rounded px-1 py-0.5 text-xs hover:text-foreground focus-visible:ring-1 focus-visible:ring-ring focus-visible:outline-none',
              locale === 'en' ? 'font-medium text-foreground' : 'text-muted-foreground',
            )}
            onClick={() => setLocale('en')}
          >
            EN
          </button>
          <button
            type="button"
            className={cn(
              'rounded px-1 py-0.5 text-xs hover:text-foreground focus-visible:ring-1 focus-visible:ring-ring focus-visible:outline-none',
              locale === 'zh' ? 'font-medium text-foreground' : 'text-muted-foreground',
            )}
            onClick={() => setLocale('zh')}
          >
            中文
          </button>
          <span className="flex-1" />
          <span className="text-[11px] text-muted-foreground">{t(locale, 'auth.selfHosted')}</span>
        </div>
      </div>
    </div>
  );
}
