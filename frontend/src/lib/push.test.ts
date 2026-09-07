import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@/lib/api-client', () => ({
  api: vi.fn(),
}));

import { api } from '@/lib/api-client';
import { pushStatus, subscribePush, syncPushPrefs, unsubscribePush } from './push';

const mockedApi = vi.mocked(api);

function mockRegistration(subscription: PushSubscription | null) {
  const reg = {
    pushManager: {
      getSubscription: vi.fn().mockResolvedValue(subscription),
      subscribe: vi.fn(),
    },
  };
  Object.defineProperty(navigator, 'serviceWorker', {
    value: { getRegistration: vi.fn().mockResolvedValue(reg), ready: Promise.resolve(reg) },
    configurable: true,
  });
  Object.defineProperty(window, 'PushManager', { value: class {}, configurable: true });
  return reg;
}

function fakeSubscription(endpoint = 'https://push.example/abc') {
  return {
    endpoint,
    toJSON: () => ({ endpoint, keys: { p256dh: 'p256dh', auth: 'auth' } }),
    unsubscribe: vi.fn().mockResolvedValue(true),
  } as unknown as PushSubscription;
}

describe('push', () => {
  beforeEach(() => {
    mockedApi.mockResolvedValue({ publicKey: 'BKey' } as never);
  });
  afterEach(() => {
    vi.restoreAllMocks();
    // @ts-expect-error cleanup
    delete navigator.serviceWorker;
    // @ts-expect-error cleanup
    delete window.PushManager;
  });

  it('reports unsupported without a service worker', async () => {
    // @ts-expect-error absence
    delete navigator.serviceWorker;
    expect(await pushStatus()).toBe('unsupported');
  });

  it('subscribes with the server VAPID key and PUTs the subscription', async () => {
    const sub = fakeSubscription();
    const reg = mockRegistration(null);
    // Real PushManager semantics: after subscribe() the new subscription is
    // what getSubscription() returns.
    reg.pushManager.subscribe.mockImplementation(async () => {
      reg.pushManager.getSubscription.mockResolvedValue(sub);
      return sub;
    });

    await subscribePush();

    expect(mockedApi).toHaveBeenCalledWith('/push/vapid-key');
    const [keyArg] = reg.pushManager.subscribe.mock.calls[0];
    expect(keyArg.userVisibleOnly).toBe(true);
    expect(keyArg.applicationServerKey).toBeInstanceOf(Uint8Array);
    expect(mockedApi).toHaveBeenCalledWith('/push/subscription', {
      method: 'PUT',
      body: JSON.stringify({
        endpoint: 'https://push.example/abc',
        keys: { p256dh: 'p256dh', auth: 'auth' },
      }),
    });
    expect(await pushStatus()).toBe('subscribed');
  });

  it('unsubscribes locally and on the server', async () => {
    const sub = fakeSubscription();
    mockRegistration(sub);
    await unsubscribePush();
    expect(mockedApi).toHaveBeenCalledWith('/push/subscription', {
      method: 'DELETE',
      body: JSON.stringify({ endpoint: 'https://push.example/abc' }),
    });
    expect(sub.unsubscribe).toHaveBeenCalled();
  });

  it('syncPushPrefs PUTs only when subscribed', async () => {
    mockRegistration(null);
    await syncPushPrefs({ mutedFolderIds: ['f1'], mutedThreadIds: [], locale: 'en' });
    expect(mockedApi).not.toHaveBeenCalledWith('/push/prefs', expect.anything());

    mockRegistration(fakeSubscription());
    await syncPushPrefs({ mutedFolderIds: ['f1'], mutedThreadIds: ['t1'], locale: 'zh' });
    expect(mockedApi).toHaveBeenCalledWith('/push/prefs', {
      method: 'PUT',
      body: JSON.stringify({ mutedFolderIds: ['f1'], mutedThreadIds: ['t1'], locale: 'zh' }),
    });
  });
});
