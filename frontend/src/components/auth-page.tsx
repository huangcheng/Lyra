/**
 * Auth page — orchestrates the authentication flow.
 *
 * Renders the appropriate UI based on the auth state machine.
 * Syncs the session token from the XState machine to the Zustand store,
 * then navigates to the mail shell (TanStack beforeLoad does not re-run
 * when Zustand updates).
 */

import { useNavigate } from '@tanstack/react-router';
import { useMachine } from '@xstate/react';
import { useEffect, useRef } from 'react';

import { t } from '@/i18n';
import { api, userFromMe, type AuthMeResponse } from '@/lib/api-client';
import { applyMarkReadPolicy } from '@/lib/user-preferences';
import { authMachine } from '../machines/auth';
import { useAuthStore } from '../stores/auth';
import { useUIStore } from '../stores/ui';
import { LoginForm } from './login-form';

export function AuthPage() {
  const [state, send] = useMachine(authMachine);
  const navigate = useNavigate();
  const locale = useUIStore((s) => s.locale);
  const setLocale = useUIStore((s) => s.setLocale);
  const isAuthenticated = useAuthStore((s) => s.isAuthenticated);
  const authStore = useAuthStore();
  const syncedRef = useRef(false);

  useEffect(() => {
    if (isAuthenticated) {
      void navigate({ to: '/' });
    }
  }, [isAuthenticated, navigate]);

  useEffect(() => {
    if (state.matches('authenticated') && state.context.token && !syncedRef.current) {
      syncedRef.current = true;
      const token = state.context.token;
      authStore.setToken(token);

      void api<AuthMeResponse>('/auth/me')
        .then((user) => {
          authStore.setUser(userFromMe(user));
          if (user.locale === 'en' || user.locale === 'zh') {
            setLocale(user.locale);
          }
          applyMarkReadPolicy(user.mark_read_policy);
        })
        .catch(() => {
          localStorage.removeItem('lyra_token');
          authStore.clearSession();
          syncedRef.current = false;
          send({ type: 'LOGOUT' });
        });
    }
  }, [state, state.context.token, authStore, send, setLocale]);

  useEffect(() => {
    if (!state.matches('authenticated')) {
      syncedRef.current = false;
    }
  }, [state]);

  const handleLogin = (username: string, password: string) => {
    send({ type: 'LOGIN', username, password });
  };

  const handleBootstrap = (
    username: string,
    password: string,
    displayName?: string,
    locale?: string,
  ) => {
    send({ type: 'BOOTSTRAP', username, password, displayName, locale });
  };

  const handleTotpVerify = (code: string) => {
    send({ type: 'TOTP_SUBMIT', code });
  };

  const handleRetry = () => {
    send({ type: 'RETRY' });
  };

  if (state.matches('authenticated')) {
    return (
      <div className="flex min-h-svh items-center justify-center p-6 text-sm text-muted-foreground">
        {t(locale, 'common.loading')}
      </div>
    );
  }

  return (
    <div className="flex min-h-svh w-full items-center justify-center p-6 md:p-10">
      <div className="w-full max-w-sm">
        <LoginForm
          onLogin={handleLogin}
          onBootstrap={handleBootstrap}
          onTotpVerify={handleTotpVerify}
          onRetry={handleRetry}
          error={state.matches('error') ? t(locale, 'auth.statusCheckError') : state.context.error}
          hasUser={
            state.matches('bootstrap') || state.matches('bootstrapping')
              ? false
              : state.matches('checkingStatus') || state.matches('error')
                ? null
                : true
          }
          requiresTotp={state.matches('totpChallenge') || state.matches('verifyingTotp')}
        />
      </div>
    </div>
  );
}
