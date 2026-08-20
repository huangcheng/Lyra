/**
 * XState machine for adding a mail account.
 *
 * Multi-step flow:
 *   enter_email → probing → credentials → verifying → syncing → complete / error
 *
 * Role: FLOW LOGIC only. No data (→ Zustand), no async streams (→ RxJS).
 */

import { setup, assign } from 'xstate';

interface AccountSetupContext {
  emailAddress: string;
  displayName: string;
  password: string;
  autoConfigFound: boolean;
  error: string | null;
}

type AccountSetupEvent =
  | { type: 'SUBMIT_EMAIL'; email: string; displayName: string }
  | { type: 'SUBMIT_CREDENTIALS'; password: string }
  | { type: 'RETRY' }
  | { type: 'CANCEL' }
  | { type: 'RESET' };

export const accountSetupMachine = setup({
  types: {} as {
    context: AccountSetupContext;
    events: AccountSetupEvent;
  },
  actions: {
    setEmail: assign(({ event }) => {
      if (event.type !== 'SUBMIT_EMAIL') return {};
      return {
        emailAddress: event.email,
        displayName: event.displayName,
        error: null,
      };
    }),
    setCredentials: assign(({ event }) => {
      if (event.type !== 'SUBMIT_CREDENTIALS') return {};
      return { password: event.password };
    }),
    setError: assign(() => ({
      error: 'Account setup failed',
    })),
    clearAll: assign({
      emailAddress: '',
      displayName: '',
      password: '',
      autoConfigFound: false,
      error: null,
    }),
  },
}).createMachine({
  id: 'accountSetup',
  initial: 'enterEmail',
  context: {
    emailAddress: '',
    displayName: '',
    password: '',
    autoConfigFound: false,
    error: null,
  },
  states: {
    enterEmail: {
      on: {
        SUBMIT_EMAIL: {
          target: 'probing',
          actions: 'setEmail',
        },
        CANCEL: { target: 'idle' },
      },
    },
    probing: {
      // In production: invoke auto-config probe.
      // Stub: immediately move to credentials.
      always: {
        target: 'credentials',
      },
    },
    credentials: {
      on: {
        SUBMIT_CREDENTIALS: {
          target: 'verifying',
          actions: 'setCredentials',
        },
        CANCEL: { target: 'enterEmail' },
      },
    },
    verifying: {
      // In production: verify credentials with server.
      // Stub: immediately move to syncing.
      always: {
        target: 'syncing',
      },
    },
    syncing: {
      // In production: wait for sync to complete (via RxJS SSE stream).
      // Stub: immediately complete.
      always: {
        target: 'complete',
      },
    },
    complete: {
      on: {
        RESET: { target: 'enterEmail', actions: 'clearAll' },
      },
    },
    error: {
      on: {
        RETRY: { target: 'credentials' },
        RESET: { target: 'enterEmail', actions: 'clearAll' },
      },
    },
    idle: {
      // Terminal state for cancelled flows.
      on: {
        RESET: { target: 'enterEmail', actions: 'clearAll' },
      },
    },
  },
});
