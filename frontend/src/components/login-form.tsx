/**
 * Login screen for Lyra.
 *
 * Handles username/password authentication and TOTP challenge.
 */

import { useState } from 'react';
import { t } from '../i18n';
import { useUIStore } from '../stores/ui';

interface LoginFormProps {
  onLogin: (username: string, password: string) => void;
  onBootstrap: (username: string, password: string, displayName?: string, locale?: string) => void;
  onTotpVerify: (code: string) => void;
  error: string | null;
  hasUser: boolean | null;
  requiresTotp: boolean;
}

export function LoginForm({
  onLogin,
  onBootstrap,
  onTotpVerify,
  error,
  hasUser,
  requiresTotp,
}: LoginFormProps) {
  const locale = useUIStore((s) => s.locale);
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [confirmPassword, setConfirmPassword] = useState('');
  const [displayName, setDisplayName] = useState('');
  const [totpCode, setTotpCode] = useState('');
  const [validationError, setValidationError] = useState<string | null>(null);

  const handleLoginSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    setValidationError(null);
    if (!username || !password) {
      setValidationError(t(locale, 'common.error'));
      return;
    }
    onLogin(username, password);
  };

  const handleBootstrapSubmit = (e: React.FormEvent) => {
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

  const handleTotpSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    setValidationError(null);
    if (!totpCode || totpCode.length !== 6) {
      setValidationError(t(locale, 'auth.totpCode'));
      return;
    }
    onTotpVerify(totpCode);
  };

  // Loading state
  if (hasUser === null) {
    return (
      <div className="flex min-h-screen items-center justify-center">
        <div className="text-center">
          <div className="mx-auto h-8 w-8 animate-spin rounded-full border-4 border-primary border-t-transparent" />
          <p className="mt-2 text-sm text-muted-foreground">{t(locale, 'common.loading')}</p>
        </div>
      </div>
    );
  }

  // TOTP challenge
  if (requiresTotp) {
    return (
      <div className="flex min-h-screen items-center justify-center">
        <div className="w-full max-w-md space-y-6 p-6">
          <div className="text-center">
            <h1 className="text-2xl font-bold">{t(locale, 'auth.totpTitle')}</h1>
            <p className="mt-2 text-sm text-muted-foreground">
              {t(locale, 'auth.totpDescription')}
            </p>
          </div>

          <form onSubmit={handleTotpSubmit} className="space-y-4">
            <div>
              <label htmlFor="totp-code" className="block text-sm font-medium">
                {t(locale, 'auth.totpCode')}
              </label>
              <input
                id="totp-code"
                type="text"
                inputMode="numeric"
                pattern="[0-9]*"
                maxLength={6}
                value={totpCode}
                onChange={(e) => setTotpCode(e.target.value.replace(/\D/g, ''))}
                className="mt-1 block w-full rounded-md border border-input bg-background px-3 py-2 text-center text-lg tracking-widest"
                placeholder="000000"
                autoFocus
              />
            </div>

            {(error || validationError) && (
              <p className="text-sm text-destructive">{error || validationError}</p>
            )}

            <button
              type="submit"
              className="w-full rounded-md bg-primary px-4 py-2 text-primary-foreground hover:bg-primary/90"
            >
              {t(locale, 'auth.totpVerify')}
            </button>
          </form>
        </div>
      </div>
    );
  }

  // Bootstrap (first-run setup)
  if (!hasUser) {
    return (
      <div className="flex min-h-screen items-center justify-center">
        <div className="w-full max-w-md space-y-6 p-6">
          <div className="text-center">
            <h1 className="text-2xl font-bold">{t(locale, 'auth.bootstrapTitle')}</h1>
            <p className="mt-2 text-sm text-muted-foreground">
              {t(locale, 'auth.bootstrapDescription')}
            </p>
          </div>

          <form onSubmit={handleBootstrapSubmit} className="space-y-4">
            <div>
              <label htmlFor="username" className="block text-sm font-medium">
                {t(locale, 'auth.username')}
              </label>
              <input
                id="username"
                type="text"
                value={username}
                onChange={(e) => setUsername(e.target.value)}
                className="mt-1 block w-full rounded-md border border-input bg-background px-3 py-2"
                autoFocus
              />
            </div>

            <div>
              <label htmlFor="display-name" className="block text-sm font-medium">
                {t(locale, 'auth.displayName')}
              </label>
              <input
                id="display-name"
                type="text"
                value={displayName}
                onChange={(e) => setDisplayName(e.target.value)}
                className="mt-1 block w-full rounded-md border border-input bg-background px-3 py-2"
              />
            </div>

            <div>
              <label htmlFor="password" className="block text-sm font-medium">
                {t(locale, 'auth.password')}
              </label>
              <input
                id="password"
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                className="mt-1 block w-full rounded-md border border-input bg-background px-3 py-2"
              />
            </div>

            <div>
              <label htmlFor="confirm-password" className="block text-sm font-medium">
                {t(locale, 'auth.confirmPassword')}
              </label>
              <input
                id="confirm-password"
                type="password"
                value={confirmPassword}
                onChange={(e) => setConfirmPassword(e.target.value)}
                className="mt-1 block w-full rounded-md border border-input bg-background px-3 py-2"
              />
            </div>

            {(error || validationError) && (
              <p className="text-sm text-destructive">{error || validationError}</p>
            )}

            <button
              type="submit"
              className="w-full rounded-md bg-primary px-4 py-2 text-primary-foreground hover:bg-primary/90"
            >
              {t(locale, 'auth.createAccount')}
            </button>
          </form>
        </div>
      </div>
    );
  }

  // Normal login
  return (
    <div className="flex min-h-screen items-center justify-center">
      <div className="w-full max-w-md space-y-6 p-6">
        <div className="text-center">
          <h1 className="text-2xl font-bold">{t(locale, 'auth.loginTitle')}</h1>
        </div>

        <form onSubmit={handleLoginSubmit} className="space-y-4">
          <div>
            <label htmlFor="username" className="block text-sm font-medium">
              {t(locale, 'auth.username')}
            </label>
            <input
              id="username"
              type="text"
              value={username}
              onChange={(e) => setUsername(e.target.value)}
              className="mt-1 block w-full rounded-md border border-input bg-background px-3 py-2"
              autoFocus
            />
          </div>

          <div>
            <label htmlFor="password" className="block text-sm font-medium">
              {t(locale, 'auth.password')}
            </label>
            <input
              id="password"
              type="password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              className="mt-1 block w-full rounded-md border border-input bg-background px-3 py-2"
            />
          </div>

          {(error || validationError) && (
            <p className="text-sm text-destructive">{error || validationError}</p>
          )}

          <button
            type="submit"
            className="w-full rounded-md bg-primary px-4 py-2 text-primary-foreground hover:bg-primary/90"
          >
            {t(locale, 'auth.login')}
          </button>
        </form>
      </div>
    </div>
  );
}
