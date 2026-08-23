/**
 * Restore a persisted Lyra session before the router mounts.
 */

import { api, userFromMe, type AuthMeResponse } from '@/lib/api-client';
import { applyMarkReadPolicy } from '@/lib/user-preferences';
import { useAuthStore } from '@/stores/auth';
import { useUIStore } from '@/stores/ui';
import type { SupportedLocale } from '@/types';

function applyLocale(locale: string): void {
  if (locale === 'en' || locale === 'zh') {
    useUIStore.getState().setLocale(locale as SupportedLocale);
  }
}

export async function restoreSession(): Promise<void> {
  const token = localStorage.getItem('lyra_token');
  if (!token) return;

  try {
    const me = await api<AuthMeResponse>('/auth/me');
    const auth = useAuthStore.getState();
    auth.setToken(token);
    auth.setUser(userFromMe(me));
    applyLocale(me.locale);
    applyMarkReadPolicy(me.mark_read_policy);
  } catch {
    localStorage.removeItem('lyra_token');
    useAuthStore.getState().clearSession();
  }
}
