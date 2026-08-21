/**
 * Login / bootstrap / TOTP — form-first polish (no watermark / globe).
 * Design: docs/superpowers/specs/2026-08-21-lyra-auth-editorial-design.md
 */

import { useState, type ReactNode } from 'react';
import { motion, useReducedMotion, type Variants } from 'motion/react';
import { t } from '../i18n';
import { useUIStore } from '../stores/ui';
import { StampMark } from './stamp-mark';

interface LoginFormProps {
  onLogin: (username: string, password: string) => void;
  onBootstrap: (username: string, password: string, displayName?: string, locale?: string) => void;
  onTotpVerify: (code: string) => void;
  error: string | null;
  hasUser: boolean | null;
  requiresTotp: boolean;
}

const easeOut = [0.22, 1, 0.36, 1] as const;

const stackVariants: Variants = {
  hidden: {},
  show: {
    transition: { staggerChildren: 0.055, delayChildren: 0.03 },
  },
};

const itemVariants: Variants = {
  hidden: { opacity: 0, y: 8 },
  show: {
    opacity: 1,
    y: 0,
    transition: { duration: 0.28, ease: easeOut },
  },
};

function AuthShell({ children }: { children: ReactNode }) {
  const locale = useUIStore((s) => s.locale);
  const reduceMotion = useReducedMotion();

  return (
    <div className="auth-page">
      <motion.div
        className="auth-stack"
        variants={stackVariants}
        initial={reduceMotion ? false : 'hidden'}
        animate="show"
      >
        {children}
        <motion.p className="auth-tagline" variants={itemVariants}>
          {t(locale, 'app.tagline')}
        </motion.p>
      </motion.div>
    </div>
  );
}

function BrandBlock() {
  return (
    <motion.header className="auth-brand-block" variants={itemVariants}>
      <StampMark size={44} className="auth-stamp" />
      <p className="auth-wordmark">Lyra</p>
    </motion.header>
  );
}

function AuthCopy({ title, description }: { title: string; description?: string }) {
  return (
    <motion.div className="auth-copy" variants={itemVariants}>
      <h1 className="auth-title">{title}</h1>
      {description ? <p className="auth-muted">{description}</p> : null}
    </motion.div>
  );
}

function AuthFormWrap({ children }: { children: ReactNode }) {
  return (
    <motion.div className="auth-form-wrap" variants={itemVariants}>
      {children}
    </motion.div>
  );
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

  if (hasUser === null) {
    return (
      <AuthShell>
        <motion.div className="auth-loading" variants={itemVariants}>
          <div className="auth-spinner" aria-hidden />
          <p className="auth-muted">{t(locale, 'common.loading')}</p>
        </motion.div>
      </AuthShell>
    );
  }

  if (requiresTotp) {
    return (
      <AuthShell>
        <BrandBlock />
        <AuthCopy
          title={t(locale, 'auth.totpTitle')}
          description={t(locale, 'auth.totpDescription')}
        />
        <AuthFormWrap>
          <form onSubmit={handleTotpSubmit} className="auth-form">
            <label className="auth-field" htmlFor="totp-code">
              <span className="auth-label">{t(locale, 'auth.totpCode')}</span>
              <input
                id="totp-code"
                type="text"
                inputMode="numeric"
                pattern="[0-9]*"
                maxLength={6}
                value={totpCode}
                onChange={(e) => setTotpCode(e.target.value.replace(/\D/g, ''))}
                className="auth-input auth-input--otp"
                placeholder="000000"
                autoFocus
              />
            </label>
            {(error || validationError) && (
              <p className="auth-error">{error || validationError}</p>
            )}
            <button type="submit" className="auth-submit">
              {t(locale, 'auth.totpVerify')}
            </button>
          </form>
        </AuthFormWrap>
      </AuthShell>
    );
  }

  if (!hasUser) {
    return (
      <AuthShell>
        <BrandBlock />
        <AuthCopy
          title={t(locale, 'auth.bootstrapTitle')}
          description={t(locale, 'auth.bootstrapDescription')}
        />
        <AuthFormWrap>
          <form onSubmit={handleBootstrapSubmit} className="auth-form">
            <label className="auth-field" htmlFor="username">
              <span className="auth-label">{t(locale, 'auth.username')}</span>
              <input
                id="username"
                type="text"
                value={username}
                onChange={(e) => setUsername(e.target.value)}
                className="auth-input"
                autoFocus
                autoComplete="username"
              />
            </label>
            <label className="auth-field" htmlFor="display-name">
              <span className="auth-label">{t(locale, 'auth.displayName')}</span>
              <input
                id="display-name"
                type="text"
                value={displayName}
                onChange={(e) => setDisplayName(e.target.value)}
                className="auth-input"
                autoComplete="name"
              />
            </label>
            <label className="auth-field" htmlFor="password">
              <span className="auth-label">{t(locale, 'auth.password')}</span>
              <input
                id="password"
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                className="auth-input"
                autoComplete="new-password"
              />
            </label>
            <label className="auth-field" htmlFor="confirm-password">
              <span className="auth-label">{t(locale, 'auth.confirmPassword')}</span>
              <input
                id="confirm-password"
                type="password"
                value={confirmPassword}
                onChange={(e) => setConfirmPassword(e.target.value)}
                className="auth-input"
                autoComplete="new-password"
              />
            </label>
            {(error || validationError) && (
              <p className="auth-error">{error || validationError}</p>
            )}
            <button type="submit" className="auth-submit">
              {t(locale, 'auth.createAccount')}
            </button>
          </form>
        </AuthFormWrap>
      </AuthShell>
    );
  }

  return (
    <AuthShell>
      <BrandBlock />
      <AuthCopy title={t(locale, 'auth.loginTitle')} />
      <AuthFormWrap>
        <form onSubmit={handleLoginSubmit} className="auth-form">
          <label className="auth-field" htmlFor="username">
            <span className="auth-label">{t(locale, 'auth.username')}</span>
            <input
              id="username"
              type="text"
              value={username}
              onChange={(e) => setUsername(e.target.value)}
              className="auth-input"
              autoFocus
              autoComplete="username"
            />
          </label>
          <label className="auth-field" htmlFor="password">
            <span className="auth-label">{t(locale, 'auth.password')}</span>
            <input
              id="password"
              type="password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              className="auth-input"
              autoComplete="current-password"
            />
          </label>
          {(error || validationError) && (
            <p className="auth-error">{error || validationError}</p>
          )}
          <button type="submit" className="auth-submit">
            {t(locale, 'auth.login')}
          </button>
        </form>
      </AuthFormWrap>
    </AuthShell>
  );
}
