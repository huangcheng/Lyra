import { api } from '@/lib/api-client';
import { useMailStore } from '@/stores/mail';

/** PATCH read + update local store. Returns false when already read or request failed. */
export async function markMessageReadOnServer(messageId: string): Promise<boolean> {
  const msg = useMailStore.getState().messages[messageId];
  if (msg?.isRead) return true;
  try {
    await api(`/messages/${messageId}`, {
      method: 'PATCH',
      body: JSON.stringify({ isRead: true }),
    });
    useMailStore.getState().markMessageRead(messageId);
    return true;
  } catch {
    return false;
  }
}
