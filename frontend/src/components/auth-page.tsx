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
import { authMachine } from '../machines/auth';
import { useAuthStore } from '../stores/auth';
import { useUIStore } from '../stores/ui';
import { LoginForm } from './login-form';

export function AuthPage() {
  const [state, send] = useMachine(authMachine);
  const navigate = useNavigate();
  const locale = useUIStore((s) => s.locale);
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

      fetch('/api/v1/auth/me', {
        headers: { Authorization: `Bearer ${token}` },
      })
        .then((res) => {
          if (!res.ok) throw new Error('Failed to fetch user');
          return res.json();
        })
        .then((user) => {
          authStore.setUser({
            id: user.id,
            username: user.username,
            displayName: user.display_name,
            locale: user.locale,
            totpEnabled: user.totp_enabled,
          });
        })
        .catch(() => {
          localStorage.removeItem('lyra_token');
          authStore.clearSession();
          syncedRef.current = false;
          send({ type: 'LOGOUT' });
        });
    }
  }, [state, state.context.token, authStore, send]);

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
          error={state.context.error}
          hasUser={
            state.matches('bootstrap') ? false : state.matches('checkingStatus') ? null : true
          }
          requiresTotp={state.matches('totpChallenge') || state.matches('verifyingTotp')}
        />
      </div>
    </div>
  );
}
