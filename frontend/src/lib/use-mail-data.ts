/**
 * Load accounts and folders into the mail store.
 */

import { useCallback, useEffect } from 'react';

import { api } from '@/lib/api-client';
import { mapApiFolder, mapApiMessage, type ApiFolder, type ApiMessage } from '@/lib/mail-api';
import { syncEvents$ } from '@/rxjs/sync-events';
import { useAuthStore } from '@/stores/auth';
import { useMailStore } from '@/stores/mail';
import type { MailAccount } from '@/types';

interface ApiAccount {
  id: string;
  displayName: string;
  emailAddress: string;
  protocol: string;
  isActive: boolean;
  syncEnabled: boolean;
  lastSyncAt?: string;
}

export function useMailData() {
  const token = useAuthStore((s) => s.token);
  const setAccounts = useMailStore((s) => s.setAccounts);
  const setFolders = useMailStore((s) => s.setFolders);
  const upsertMessage = useMailStore((s) => s.upsertMessage);

  const fetchAccounts = useCallback(async () => {
    if (!token) return;
    try {
      const data = await api<ApiAccount[]>('/accounts');
      const accounts: MailAccount[] = data.map((a) => ({
        id: a.id,
        displayName: a.displayName,
        emailAddress: a.emailAddress,
        protocol: a.protocol as 'jmap' | 'imap',
        isActive: a.isActive,
        syncEnabled: a.syncEnabled,
        lastSyncAt: a.lastSyncAt,
      }));
      setAccounts(accounts);
    } catch {
      /* network or HTTP error — keep last good snapshot */
    }
  }, [token, setAccounts]);

  const fetchFolders = useCallback(async () => {
    if (!token) return;
    try {
      const data = await api<ApiFolder[]>('/folders');
      setFolders(data.map(mapApiFolder));
    } catch {
      /* keep last good snapshot */
    }
  }, [token, setFolders]);

  const fetchMessages = useCallback(
    async (folderId: string) => {
      if (!token || !folderId) return;
      try {
        const data = await api<ApiMessage[]>(`/folders/${folderId}/messages`);
        for (const msg of data) upsertMessage(mapApiMessage(msg));
      } catch {
        /* keep last good snapshot */
      }
    },
    [token, upsertMessage],
  );

  useEffect(() => {
    void fetchAccounts();
    void fetchFolders();
  }, [fetchAccounts, fetchFolders]);

  useEffect(() => {
    const sub = syncEvents$.subscribe((ev) => {
      if (ev.type === 'sync_complete' || ev.type === 'sync_error') {
        void fetchAccounts();
        void fetchFolders();
      }
    });
    return () => sub.unsubscribe();
  }, [fetchAccounts, fetchFolders]);

  return { fetchAccounts, fetchFolders, fetchMessages };
}
