/**
 * Login / bootstrap / TOTP — shadcn login-01 card, adapted for Lyra.
 */

import type { ComponentProps, FormEvent } from 'react';
import { useState } from 'react';

import { Button } from '@/components/ui/button';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Field, FieldDescription, FieldError, FieldGroup, FieldLabel } from '@/components/ui/field';
import { Input } from '@/components/ui/input';
import { t } from '@/i18n';
import { cn } from '@/lib/utils';
import { useUIStore } from '@/stores/ui';

interface LoginFormProps {
  onLogin: (username: string, password: string) => void;
  onBootstrap: (username: string, password: string, displayName?: string, locale?: string) => void;
  onTotpVerify: (code: string) => void;
  onRetry?: () => void;
  error: string | null;
  hasUser: boolean | null;
  requiresTotp: boolean;
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

  const formError = error || validationError;

  const handleLoginSubmit = (e: FormEvent) => {
    e.preventDefault();
    setValidationError(null);
    if (!username || !password) {
      setValidationError(t(locale, 'common.error'));
      return;
    }
    onLogin(username, password);
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
    onBootstrap(username, password, displayName || undefined, locale);
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

  return (
    <div className={cn('flex flex-col gap-6', className)} {...props}>
      <Card>
        <CardHeader>
          <CardTitle>
            {hasUser === null
              ? formError
                ? t(locale, 'common.error')
                : t(locale, 'common.loading')
              : title}
          </CardTitle>
          {hasUser !== null ? <CardDescription>{description}</CardDescription> : null}
        </CardHeader>
        <CardContent>
          {hasUser === null ? (
            <FieldGroup>
              {formError ? (
                <FieldError>{formError}</FieldError>
              ) : (
                <p className="text-sm text-muted-foreground">{t(locale, 'common.loading')}</p>
              )}
              {formError && onRetry ? (
                <Field>
                  <Button type="button" onClick={onRetry}>
                    {t(locale, 'common.retry')}
                  </Button>
                </Field>
              ) : null}
            </FieldGroup>
          ) : requiresTotp ? (
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
                  <Button type="submit">{t(locale, 'auth.totpVerify')}</Button>
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
                {formError ? <FieldError>{formError}</FieldError> : null}
                <Field>
                  <Button type="submit">{t(locale, 'auth.createAccount')}</Button>
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
                    placeholder="admin"
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
                    required
                  />
                </Field>
                {formError ? <FieldError>{formError}</FieldError> : null}
                <Field>
                  <Button type="submit">{t(locale, 'auth.login')}</Button>
                  <FieldDescription className="text-center">
                    {t(locale, 'app.tagline')}
                  </FieldDescription>
                </Field>
              </FieldGroup>
            </form>
          )}
        </CardContent>
      </Card>
      <div className="flex justify-center gap-2">
        <Button
          type="button"
          variant={locale === 'en' ? 'secondary' : 'ghost'}
          size="sm"
          onClick={() => setLocale('en')}
        >
          EN
        </Button>
        <Button
          type="button"
          variant={locale === 'zh' ? 'secondary' : 'ghost'}
          size="sm"
          onClick={() => setLocale('zh')}
        >
          中文
        </Button>
      </div>
    </div>
  );
}
