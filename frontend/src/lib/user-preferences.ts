import { api } from '@/lib/api-client';
import type { AuthMeResponse } from '@/lib/api-client';
import { isMarkReadPolicy } from '@/lib/mark-read-policy';
import { useAuthStore } from '@/stores/auth';
import { useUIStore } from '@/stores/ui';
import type { MarkReadPolicy } from '@/types';

export function applyMarkReadPolicy(raw?: string): void {
  if (raw && isMarkReadPolicy(raw)) {
    useUIStore.getState().setMarkReadPolicy(raw);
  }
}

export async function saveMarkReadPolicy(policy: MarkReadPolicy): Promise<void> {
  const me = await api<AuthMeResponse>('/auth/preferences', {
    method: 'PATCH',
    body: JSON.stringify({ markReadPolicy: policy }),
  });
  const saved = isMarkReadPolicy(me.mark_read_policy) ? me.mark_read_policy : policy;
  useUIStore.getState().setMarkReadPolicy(saved);
  const auth = useAuthStore.getState();
  if (auth.user) {
    auth.setUser({ ...auth.user, markReadPolicy: saved });
  }
}

/** Persist the UI locale for the signed-in user and apply it locally. */
export async function saveLocale(locale: 'en' | 'zh'): Promise<void> {
  const me = await api<AuthMeResponse>('/auth/preferences', {
    method: 'PATCH',
    body: JSON.stringify({ locale }),
  });
  const saved = me.locale === 'en' || me.locale === 'zh' ? me.locale : locale;
  useUIStore.getState().setLocale(saved);
  const auth = useAuthStore.getState();
  if (auth.user) {
    auth.setUser({ ...auth.user, locale: saved });
  }
}
