/**
 * Auth page — orchestrates the authentication flow.
 *
 * Renders the appropriate UI based on the auth state machine.
 */

import { useMachine } from '@xstate/react';
import { authMachine } from '../machines/auth';
import { useAuthStore } from '../stores/auth';
import { LoginForm } from './login-form';
import { useEffect } from 'react';

export function AuthPage() {
  const [state, send] = useMachine(authMachine);
  const authStore = useAuthStore();

  // Sync machine state to Zustand store
  useEffect(() => {
    if (state.matches('authenticated')) {
      // Machine is authenticated; fetch user info
      const token = authStore.token;
      if (token) {
        fetch('/api/auth/me', {
          headers: { Authorization: `Bearer ${token}` },
        })
          .then((res) => res.json())
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
            // Token may be invalid
            authStore.clearSession();
          });
      }
    }
  }, [state.value]);

  // Store the token when login/bootstrap succeeds
  useEffect(() => {
    // When transitioning to authenticated, we need to capture the token
    // from the machine's last event output
  }, [state.value]);

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

  // If authenticated, we should have a token
  // The parent component should handle rendering based on auth state
  if (state.matches('authenticated')) {
    return null; // Parent will re-render
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
