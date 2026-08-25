/**
 * XState machine for the OpenGPG unlock dialog.
 *
 * Flow: closed → prompting → unlocking → success | prompting(error).
 * Role: FLOW LOGIC only — no Zustand mail data, no RxJS streams.
 */

import { assign, fromPromise, setup } from 'xstate';
import { unlockOpengpgKey, type CacheMode, type UnlockResult } from '@/lib/opengpg-api';
import { ApiError } from '@/lib/api-client';

interface UnlockContext {
  keyId: string | null;
  fingerprint: string | null;
  passphrase: string;
  cache: CacheMode;
  ttlMinutes: number;
  error: string | null;
  result: UnlockResult | null;
}

type UnlockEvent =
  | {
      type: 'OPEN';
      keyId: string;
      fingerprint: string;
      cache: CacheMode;
      ttlMinutes: number;
    }
  | { type: 'CLOSE' }
  | { type: 'SET_PASSPHRASE'; value: string }
  | { type: 'SET_CACHE'; value: CacheMode }
  | { type: 'SUBMIT' };

export const opengpgUnlockMachine = setup({
  types: {} as {
    context: UnlockContext;
    events: UnlockEvent;
  },
  actors: {
    unlock: fromPromise(
      async ({
        input,
      }: {
        input: {
          keyId: string;
          passphrase: string;
          cache: CacheMode;
          ttlMinutes: number;
        };
      }) => unlockOpengpgKey(input),
    ),
  },
}).createMachine({
  id: 'opengpgUnlock',
  initial: 'closed',
  context: {
    keyId: null,
    fingerprint: null,
    passphrase: '',
    cache: 'timed',
    ttlMinutes: 10,
    error: null,
    result: null,
  },
  states: {
    closed: {
      entry: assign({
        keyId: null,
        fingerprint: null,
        passphrase: '',
        error: null,
        result: null,
      }),
      on: {
        OPEN: {
          target: 'prompting',
          actions: assign(({ event }) => ({
            keyId: event.keyId,
            fingerprint: event.fingerprint,
            cache: event.cache,
            ttlMinutes: event.ttlMinutes,
            passphrase: '',
            error: null,
            result: null,
          })),
        },
      },
    },
    prompting: {
      on: {
        CLOSE: 'closed',
        SET_PASSPHRASE: {
          actions: assign(({ event }) => ({
            passphrase: event.value,
            error: null,
          })),
        },
        SET_CACHE: {
          actions: assign(({ event }) => ({ cache: event.value })),
        },
        SUBMIT: {
          guard: ({ context }) => Boolean(context.keyId && context.passphrase.length > 0),
          target: 'unlocking',
        },
      },
    },
    unlocking: {
      invoke: {
        src: 'unlock',
        input: ({ context }) => ({
          keyId: context.keyId!,
          passphrase: context.passphrase,
          cache: context.cache,
          ttlMinutes: context.ttlMinutes,
        }),
        onDone: {
          target: 'success',
          actions: assign(({ event }) => ({
            result: event.output,
            passphrase: '',
            error: null,
          })),
        },
        onError: {
          target: 'prompting',
          actions: assign(({ event }) => {
            const err = event.error;
            const message =
              err instanceof ApiError
                ? err.message
                : err instanceof Error
                  ? err.message
                  : 'Unlock failed';
            return { error: message, passphrase: '' };
          }),
        },
      },
    },
    success: {
      on: {
        CLOSE: 'closed',
      },
      after: {
        900: 'closed',
      },
    },
  },
});
