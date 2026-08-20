/**
 * XState machine for the authentication flow.
 *
 * Multi-step flow: idle → authenticating → authenticated / error.
 * Handles login, optional TOTP verification, and session management.
 *
 * Role: FLOW LOGIC only. No data (→ Zustand), no async streams (→ RxJS).
 */

import { setup, assign } from 'xstate';

interface AuthContext {
  username: string;
  password: string;
  totpCode: string;
  error: string | null;
}

type AuthEvent =
  | { type: 'LOGIN'; username: string; password: string }
  | { type: 'TOTP_SUBMIT'; code: string }
  | { type: 'LOGOUT' }
  | { type: 'RESET' };

export const authMachine = setup({
  types: {} as {
    context: AuthContext;
    events: AuthEvent;
  },
  actions: {
    setCredentials: assign(({ event }) => {
      if (event.type !== 'LOGIN') return {};
      return {
        username: event.username,
        password: event.password,
        error: null,
      };
    }),
    setTotpCode: assign(({ event }) => {
      if (event.type !== 'TOTP_SUBMIT') return {};
      return { totpCode: event.code };
    }),
    clearError: assign({ error: null }),
    clearSession: assign({
      username: '',
      password: '',
      totpCode: '',
      error: null,
    }),
    setError: assign(() => {
      // In a real impl, the error comes from the API response.
      return { error: 'Authentication failed' };
    }),
  },
  guards: {
    hasTotp: ({ context }: { context: AuthContext }) => {
      // Stub: in production this comes from the server response.
      return context.totpCode.length > 0;
    },
  },
}).createMachine({
  id: 'auth',
  initial: 'idle',
  context: {
    username: '',
    password: '',
    totpCode: '',
    error: null,
  },
  states: {
    idle: {
      on: {
        LOGIN: {
          target: 'authenticating',
          actions: 'setCredentials',
        },
      },
    },
    authenticating: {
      // In production: invoke API call here.
      // For now, immediately transition to authenticated.
      always: {
        target: 'authenticated',
      },
      on: {
        LOGOUT: { target: 'idle', actions: 'clearSession' },
      },
    },
    authenticated: {
      on: {
        LOGOUT: { target: 'idle', actions: 'clearSession' },
      },
    },
    error: {
      on: {
        LOGIN: {
          target: 'authenticating',
          actions: 'setCredentials',
        },
        RESET: { target: 'idle', actions: 'clearSession' },
      },
    },
  },
});
