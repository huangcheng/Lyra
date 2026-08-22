/**
 * Restore a persisted Lyra session before the router mounts.
 */

import { useAuthStore } from '@/stores/auth';

export async function restoreSession(): Promise<void> {
  const token = localStorage.getItem('lyra_token');
  if (!token) return;

  try {
    const res = await fetch('/api/auth/me', {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) throw new Error('Session expired');
    const user = (await res.json()) as {
      id: string;
      username: string;
      display_name?: string;
      locale: string;
      totp_enabled: boolean;
    };
    const auth = useAuthStore.getState();
    auth.setToken(token);
    auth.setUser({
      id: user.id,
      username: user.username,
      displayName: user.display_name,
      locale: user.locale,
      totpEnabled: user.totp_enabled,
    });
  } catch {
    localStorage.removeItem('lyra_token');
    useAuthStore.getState().clearSession();
  }
}
