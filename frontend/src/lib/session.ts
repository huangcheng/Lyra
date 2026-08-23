/**
 * Restore a persisted Lyra session before the router mounts.
 */

import { api, userFromMe, type AuthMeResponse } from '@/lib/api-client';
import { useAuthStore } from '@/stores/auth';

export async function restoreSession(): Promise<void> {
  const token = localStorage.getItem('lyra_token');
  if (!token) return;

  try {
    const me = await api<AuthMeResponse>('/auth/me');
    const auth = useAuthStore.getState();
    auth.setToken(token);
    auth.setUser(userFromMe(me));
  } catch {
    localStorage.removeItem('lyra_token');
    useAuthStore.getState().clearSession();
  }
}
