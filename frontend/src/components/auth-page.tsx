/**
 * Auth page — orchestrates the authentication flow.
 *
 * Renders the appropriate UI based on the auth state machine.
 * Syncs the session token from the XState machine to the Zustand store.
 */

import { useMachine } from '@xstate/react';
import { authMachine } from '../machines/auth';
import { useAuthStore } from '../stores/auth';
import { LoginForm } from './login-form';
import { useEffect, useRef } from 'react';

export function AuthPage() {
  const [state, send] = useMachine(authMachine);
  const authStore = useAuthStore();
  const syncedRef = useRef(false);

  // Sync machine state to Zustand store when authenticated
  useEffect(() => {
    if (state.matches('authenticated') && state.context.token && !syncedRef.current) {
      syncedRef.current = true;
      const token = state.context.token;

      // Store token in Zustand
      authStore.setToken(token);

      // Fetch user info with the token
      fetch('/api/auth/me', {
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
          authStore.clearSession();
          syncedRef.current = false;
        });
    }
  }, [state, state.context.token, authStore]);

  // Reset sync flag when leaving authenticated state
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

  // If authenticated, the router will redirect to /
  if (state.matches('authenticated')) {
    return null;
  }

  return (
    <LoginForm
      onLogin={handleLogin}
      onBootstrap={handleBootstrap}
      onTotpVerify={handleTotpVerify}
      error={state.context.error}
      hasUser={state.matches('bootstrap') ? false : state.matches('checkingStatus') ? null : true}
      requiresTotp={state.matches('totpChallenge') || state.matches('verifyingTotp')}
    />
  );
}
