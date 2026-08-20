/**
 * Hook to fetch mail data from the backend APIs.
 *
 * Loads accounts, folders, and messages on mount and after sync events.
 * Populates the Zustand mail store so the UI components can read from it.
 */

import { useEffect, useCallback } from 'react';
import { useMailStore } from '../stores/mail';
import { useUIStore } from '../stores/ui';
import { useAuthStore } from '../stores/auth';
import type { MailAccount, MailMessage, MailAddress } from '../types';

interface ApiAccount {
  id: string;
  displayName: string;
  emailAddress: string;
  protocol: string;
  isActive: boolean;
  syncEnabled: boolean;
  lastSyncAt?: string;
}

interface ApiMessage {
  id: string;
  accountId: string;
  folderId: string;
  subject?: string;
  fromAddress?: string;
  toAddresses?: string;
  ccAddresses?: string;
  date?: string;
  snippet?: string;
  bodyText?: string;
  bodyHtml?: string;
  isRead: boolean;
  isStarred: boolean;
  hasAttachments: boolean;
}

function parseAddress(json?: string): MailAddress {
  if (!json) return { email: 'unknown' };
  try {
    const parsed = JSON.parse(json);
    if (parsed.raw) {
      // Format: "Name <email>" or just "email"
      const match = parsed.raw.match(/^(.+?)\s*<(.+?)>$/);
      if (match) return { name: match[1].trim(), email: match[2].trim() };
      return { email: parsed.raw };
    }
    return { email: 'unknown' };
  } catch {
    return { email: 'unknown' };
  }
}

function parseAddresses(json?: string): MailAddress[] {
  if (!json) return [];
  try {
    const parsed = JSON.parse(json);
    if (Array.isArray(parsed)) {
      return parsed.map((item: string) => {
        const match = item.match(/^(.+?)\s*<(.+?)>$/);
        if (match) return { name: match[1].trim(), email: match[2].trim() };
        return { email: item };
      });
    }
    return [];
  } catch {
    return [];
  }
}

export function useMailData() {
  const token = useAuthStore((s) => s.token);
  const setAccounts = useMailStore((s) => s.setAccounts);
  const upsertMessage = useMailStore((s) => s.upsertMessage);
  const selectedAccountId = useUIStore((s) => s.selectedAccountId);
  const setSelectedAccount = useUIStore((s) => s.setSelectedAccount);

  const fetchAccounts = useCallback(async () => {
    if (!token) return;
    try {
      const res = await fetch('/api/accounts', {
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

      // Auto-select first account if none selected
      if (!selectedAccountId && accounts.length > 0) {
        setSelectedAccount(accounts[0].id);
      }
    } catch {
      // Network error - silently fail
    }
  }, [token, setAccounts, selectedAccountId, setSelectedAccount]);

  const fetchFolders = useCallback(
    async (accountId: string) => {
      if (!token || !accountId) return;
      try {
        // The backend doesn't have a direct /api/folders endpoint yet,
        // but folders are populated via sync. We'll query from the message endpoint.
        // For now, use a simple approach: the folders are already in the DB
        // after sync. We'll trigger a sync and then the data will be available.
        // In a real implementation, we'd have a /api/folders endpoint.
      } catch {
        // Silently fail
      }
    },
    [token],
  );

  const fetchMessages = useCallback(
    async (folderId: string) => {
      if (!token || !folderId) return;
      try {
        const res = await fetch(`/api/folders/${folderId}/messages`, {
          headers: { Authorization: `Bearer ${token}` },
        });
        if (!res.ok) return;
        const data: ApiMessage[] = await res.json();
        for (const msg of data) {
          const mailMsg: MailMessage = {
            id: msg.id,
            accountId: msg.accountId,
            folderId: msg.folderId,
            subject: msg.subject ?? '(no subject)',
            from: parseAddress(msg.fromAddress),
            to: parseAddresses(msg.toAddresses),
            cc: parseAddresses(msg.ccAddresses),
            date: msg.date ?? new Date().toISOString(),
            snippet: msg.snippet ?? '',
            bodyText: msg.bodyText,
            bodyHtml: msg.bodyHtml,
            isRead: msg.isRead,
            isStarred: msg.isStarred,
            isDraft: false,
            hasAttachments: msg.hasAttachments,
          };
          upsertMessage(mailMsg);
        }
      } catch {
        // Silently fail
      }
    },
    [token, upsertMessage],
  );

  // Fetch accounts on mount
  useEffect(() => {
    fetchAccounts();
  }, [fetchAccounts]);

  // Listen for sync-complete events
  useEffect(() => {
    const handler = () => {
      fetchAccounts();
    };
    window.addEventListener('lyra:sync-complete', handler);
    return () => window.removeEventListener('lyra:sync-complete', handler);
  }, [fetchAccounts]);

  return {
    fetchAccounts,
    fetchFolders,
    fetchMessages,
  };
}
