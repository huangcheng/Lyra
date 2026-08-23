/**
 * XState machine for the authentication flow.
 *
 * Multi-step flow: checkingStatus → idle/bootstrap/login/totpChallenge
 * Handles login, optional TOTP verification, and session management.
 *
 * Role: FLOW LOGIC only. No data (→ Zustand), no async streams (→ RxJS).
 */

import { setup, assign, fromPromise } from 'xstate';
import { api } from '../lib/api-client';
import type { User } from '../stores/auth';

interface AuthContext {
  username: string;
  password: string;
  totpCode: string;
  pendingToken: string | null;
  token: string | null;
  error: string | null;
}

type AuthEvent =
  | { type: 'LOGIN'; username: string; password: string }
  | { type: 'TOTP_SUBMIT'; code: string }
  | { type: 'BOOTSTRAP'; username: string; password: string; displayName?: string; locale?: string }
  | { type: 'LOGOUT' }
  | { type: 'RESET' }
  | { type: 'RETRY' };

interface LoginResponse {
  token: string;
  user: User;
  requires_totp: boolean;
}

interface StatusResponse {
  has_user: boolean;
  totp_enabled: boolean;
}

function persistToken(token: string) {
  localStorage.setItem('lyra_token', token);
}

async function fetchStatus(): Promise<StatusResponse> {
  return api<StatusResponse>('/auth/status', { auth: false });
}

async function login(username: string, password: string): Promise<LoginResponse> {
  return api<LoginResponse>('/auth/login', {
    method: 'POST',
    auth: false,
    body: JSON.stringify({ username, password }),
  });
}

async function bootstrap(
  username: string,
  password: string,
  displayName?: string,
  locale?: string,
): Promise<LoginResponse> {
  return api<LoginResponse>('/auth/bootstrap', {
    method: 'POST',
    auth: false,
    body: JSON.stringify({ username, password, display_name: displayName, locale }),
  });
}

async function verifyTotp(pendingToken: string, code: string): Promise<LoginResponse> {
  return api<LoginResponse>('/auth/totp/verify', {
    method: 'POST',
    auth: false,
    body: JSON.stringify({ pending_token: pendingToken, code }),
  });
}

export const authMachine = setup({
  types: {} as {
    context: AuthContext;
    events: AuthEvent;
  },
  actors: {
    checkStatus: fromPromise(async () => fetchStatus()),
    loginUser: fromPromise(async ({ input }: { input: { username: string; password: string } }) =>
      login(input.username, input.password),
    ),
    bootstrapUser: fromPromise(
      async ({
        input,
      }: {
        input: { username: string; password: string; displayName?: string; locale?: string };
      }) => bootstrap(input.username, input.password, input.displayName, input.locale),
    ),
    verifyTotpCode: fromPromise(
      async ({ input }: { input: { pendingToken: string; code: string } }) =>
        verifyTotp(input.pendingToken, input.code),
    ),
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
    setBootstrapCredentials: assign(({ event }) => {
      if (event.type !== 'BOOTSTRAP') return {};
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
      pendingToken: null,
      token: null,
      error: null,
    }),
    setLoginResult: assign(({ event }) => {
      if (!('output' in event) || !event.output) return {};
      const output = event.output as LoginResponse;
      persistToken(output.token);
      return {
        token: output.token,
        username: output.user.username,
        // Drop secrets from context once authentication succeeds.
        password: '',
        totpCode: '',
      };
    }),
    setTotpResult: assign(({ event }) => {
      if (!('output' in event) || !event.output) return {};
      const output = event.output as LoginResponse;
      persistToken(output.token);
      return {
        token: output.token,
        // Drop secrets from context once authentication succeeds.
        password: '',
        totpCode: '',
      };
    }),
    setPendingToken: assign(({ event }) => {
      if (!('output' in event) || !event.output) return {};
      const output = event.output as LoginResponse;
      return { pendingToken: output.token };
    }),
    setLoginError: assign(({ event }) => {
      if (!('error' in event) || !event.error) return {};
      const error = event.error as Error;
      return { error: error.message || 'Authentication failed' };
    }),
    setGenericError: assign(() => ({
      error: 'An unexpected error occurred',
    })),
  },
  guards: {
    requiresTotp: ({ event }) => {
      if (!('output' in event) || !event.output) return false;
      const output = event.output as LoginResponse;
      return output.requires_totp === true;
    },
    noUserExists: ({ event }) => {
      if (!('output' in event) || !event.output) return false;
      const output = event.output as StatusResponse;
      return !output.has_user;
    },
    userExists: ({ event }) => {
      if (!('output' in event) || !event.output) return false;
      const output = event.output as StatusResponse;
      return output.has_user;
    },
  },
}).createMachine({
  id: 'auth',
  initial: 'checkingStatus',
  context: {
    username: '',
    password: '',
    totpCode: '',
    pendingToken: null,
    token: null,
    error: null,
  },
  states: {
    checkingStatus: {
      invoke: {
        src: 'checkStatus',
        onDone: [
          {
            guard: 'noUserExists',
            target: 'bootstrap',
          },
          {
            guard: 'userExists',
            target: 'idle',
          },
        ],
        onError: {
          target: 'idle',
          actions: assign({ error: 'Failed to check status' }),
        },
      },
    },
    idle: {
      on: {
        LOGIN: {
          target: 'authenticating',
          actions: 'setCredentials',
        },
      },
    },
    authenticating: {
      invoke: {
        src: 'loginUser',
        input: ({ context }) => ({
          username: context.username,
          password: context.password,
        }),
        onDone: [
          {
            guard: 'requiresTotp',
            target: 'totpChallenge',
            actions: 'setPendingToken',
          },
          {
            target: 'authenticated',
            actions: 'setLoginResult',
          },
        ],
        onError: {
          target: 'idle',
          actions: 'setLoginError',
        },
      },
    },
    totpChallenge: {
      on: {
        TOTP_SUBMIT: {
          target: 'verifyingTotp',
          actions: 'setTotpCode',
        },
        RESET: {
          target: 'idle',
          actions: 'clearSession',
        },
      },
    },
    verifyingTotp: {
      invoke: {
        src: 'verifyTotpCode',
        input: ({ context }) => ({
          pendingToken: context.pendingToken!,
          code: context.totpCode,
        }),
        onDone: {
          target: 'authenticated',
          actions: 'setTotpResult',
        },
        onError: {
          target: 'totpChallenge',
          actions: 'setLoginError',
        },
      },
    },
    authenticated: {
      on: {
        LOGOUT: {
          target: 'idle',
          actions: 'clearSession',
        },
      },
    },
    bootstrap: {
      on: {
        BOOTSTRAP: {
          target: 'bootstrapping',
          actions: 'setBootstrapCredentials',
        },
      },
    },
    bootstrapping: {
      invoke: {
        src: 'bootstrapUser',
        input: ({ context }) => ({
          username: context.username,
          password: context.password,
        }),
        onDone: {
          target: 'authenticated',
          actions: 'setLoginResult',
        },
        onError: {
          target: 'bootstrap',
          actions: 'setLoginError',
        },
      },
    },
    error: {
      on: {
        RETRY: {
          target: 'idle',
          actions: 'clearError',
        },
        RESET: {
          target: 'idle',
          actions: 'clearSession',
        },
      },
    },
  },
});
