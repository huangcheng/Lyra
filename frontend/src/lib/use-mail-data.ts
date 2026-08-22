/**
 * Load accounts and folders into the mail store.
 */

import { useCallback, useEffect } from 'react';

import { mapApiFolder, mapApiMessage, type ApiFolder, type ApiMessage } from '@/lib/mail-api';
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
      const res = await fetch('/api/v1/accounts', {
        headers: { Authorization: `Bearer ${token}` },
      });
      if (!res.ok) return;
      const data: ApiAccount[] = await res.json();
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
      /* network error */
    }
  }, [token, setAccounts]);

  const fetchFolders = useCallback(async () => {
    if (!token) return;
    try {
      const res = await fetch('/api/v1/folders', {
        headers: { Authorization: `Bearer ${token}` },
      });
      if (!res.ok) return;
      const data: ApiFolder[] = await res.json();
      setFolders(data.map(mapApiFolder));
    } catch {
      /* network error */
    }
  }, [token, setFolders]);

  const fetchMessages = useCallback(
    async (folderId: string) => {
      if (!token || !folderId) return;
      try {
        const res = await fetch(`/api/v1/folders/${folderId}/messages`, {
          headers: { Authorization: `Bearer ${token}` },
        });
        if (!res.ok) return;
        const data: ApiMessage[] = await res.json();
        for (const msg of data) upsertMessage(mapApiMessage(msg));
      } catch {
        /* network error */
      }
    },
    [token, upsertMessage],
  );

  useEffect(() => {
    void fetchAccounts();
    void fetchFolders();
  }, [fetchAccounts, fetchFolders]);

  useEffect(() => {
    const handler = () => {
      void fetchAccounts();
      void fetchFolders();
    };
    window.addEventListener('lyra:sync-complete', handler);
    return () => window.removeEventListener('lyra:sync-complete', handler);
  }, [fetchAccounts, fetchFolders]);

  return { fetchAccounts, fetchFolders, fetchMessages };
}
