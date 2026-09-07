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

export type CaptchaPublic = {
  provider: string;
  siteKey: string;
};

interface AuthContext {
  username: string;
  password: string;
  displayName: string;
  locale: string | undefined;
  captchaToken: string | null;
  captcha: CaptchaPublic | null;
  totpCode: string;
  pendingToken: string | null;
  token: string | null;
  error: string | null;
}

type AuthEvent =
  | { type: 'LOGIN'; username: string; password: string; captchaToken?: string | null }
  | { type: 'TOTP_SUBMIT'; code: string }
  | {
      type: 'BOOTSTRAP';
      username: string;
      password: string;
      displayName?: string;
      locale?: string;
      captchaToken?: string | null;
    }
  | { type: 'LOGOUT' }
  | { type: 'RESET' }
  | { type: 'RETRY' }
  | { type: 'CAPTCHA_TOKEN'; token: string | null };

interface LoginResponse {
  token: string;
  user: User;
  requires_totp: boolean;
}

interface StatusResponse {
  has_user: boolean;
  totp_enabled: boolean;
  captcha?: { provider: string; siteKey: string } | null;
}

function persistToken(token: string) {
  localStorage.setItem('lyra_token', token);
}

const KNOWN_CAPTCHA_PROVIDERS = new Set(['turnstile', 'hcaptcha', 'recaptcha', 'recaptcha-v3']);

function captchaFromStatus(status: StatusResponse): CaptchaPublic | null {
  const c = status.captcha;
  if (!c || !KNOWN_CAPTCHA_PROVIDERS.has(c.provider) || !c.siteKey) return null;
  return { provider: c.provider, siteKey: c.siteKey };
}

async function fetchStatus(): Promise<StatusResponse> {
  return api<StatusResponse>('/auth/status', { auth: false });
}

async function login(
  username: string,
  password: string,
  captchaToken?: string | null,
): Promise<LoginResponse> {
  return api<LoginResponse>('/auth/login', {
    method: 'POST',
    auth: false,
    body: JSON.stringify({
      username,
      password,
      ...(captchaToken ? { captchaToken } : {}),
    }),
  });
}

async function bootstrap(
  username: string,
  password: string,
  displayName?: string,
  locale?: string,
  captchaToken?: string | null,
): Promise<LoginResponse> {
  return api<LoginResponse>('/auth/bootstrap', {
    method: 'POST',
    auth: false,
    body: JSON.stringify({
      username,
      password,
      display_name: displayName,
      locale,
      ...(captchaToken ? { captchaToken } : {}),
    }),
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
    loginUser: fromPromise(
      async ({
        input,
      }: {
        input: { username: string; password: string; captchaToken: string | null };
      }) => login(input.username, input.password, input.captchaToken),
    ),
    bootstrapUser: fromPromise(
      async ({
        input,
      }: {
        input: {
          username: string;
          password: string;
          displayName?: string;
          locale?: string;
          captchaToken: string | null;
        };
      }) =>
        bootstrap(
          input.username,
          input.password,
          input.displayName,
          input.locale,
          input.captchaToken,
        ),
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
        captchaToken: event.captchaToken ?? null,
        error: null,
      };
    }),
    setBootstrapCredentials: assign(({ event }) => {
      if (event.type !== 'BOOTSTRAP') return {};
      return {
        username: event.username,
        password: event.password,
        displayName: event.displayName ?? '',
        locale: event.locale,
        captchaToken: event.captchaToken ?? null,
        error: null,
      };
    }),
    setStatusCaptcha: assign(({ event }) => {
      if (!('output' in event) || !event.output) return {};
      return { captcha: captchaFromStatus(event.output as StatusResponse) };
    }),
    setCaptchaToken: assign(({ event }) => {
      if (event.type !== 'CAPTCHA_TOKEN') return {};
      return { captchaToken: event.token };
    }),
    setTotpCode: assign(({ event }) => {
      if (event.type !== 'TOTP_SUBMIT') return {};
      return { totpCode: event.code };
    }),
    clearError: assign({ error: null }),
    clearSession: assign({
      username: '',
      password: '',
      displayName: '',
      locale: undefined,
      captchaToken: null,
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
        captchaToken: null,
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
        captchaToken: null,
        totpCode: '',
      };
    }),
    setPendingToken: assign(({ event }) => {
      if (!('output' in event) || !event.output) return {};
      const output = event.output as LoginResponse;
      return { pendingToken: output.token, captchaToken: null };
    }),
    setLoginError: assign(({ event }) => {
      if (!('error' in event) || !event.error) return {};
      const error = event.error as Error;
      return { error: error.message || 'Authentication failed', captchaToken: null };
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
    displayName: '',
    locale: undefined,
    captchaToken: null,
    captcha: null,
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
            actions: 'setStatusCaptcha',
          },
          {
            guard: 'userExists',
            target: 'idle',
            actions: 'setStatusCaptcha',
          },
        ],
        onError: {
          target: 'error',
          actions: assign({ error: 'status_check' }),
        },
      },
    },
    idle: {
      on: {
        LOGIN: {
          target: 'authenticating',
          actions: 'setCredentials',
        },
        CAPTCHA_TOKEN: {
          actions: 'setCaptchaToken',
        },
      },
    },
    authenticating: {
      invoke: {
        src: 'loginUser',
        input: ({ context }) => ({
          username: context.username,
          password: context.password,
          captchaToken: context.captchaToken,
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
        CAPTCHA_TOKEN: {
          actions: 'setCaptchaToken',
        },
      },
    },
    bootstrapping: {
      invoke: {
        src: 'bootstrapUser',
        input: ({ context }) => ({
          username: context.username,
          password: context.password,
          displayName: context.displayName || undefined,
          locale: context.locale,
          captchaToken: context.captchaToken,
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
          target: 'checkingStatus',
          actions: 'clearError',
        },
      },
    },
  },
});
