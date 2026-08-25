/**
 * Per-account folder role override pickers (CHE-128).
 *
 * Settings → Accounts → Edit: remap Archive / Sent / Drafts / Spam / Trash
 * to any synced folder. Uses PATCH /api/v1/folders/{id}.
 */

import { useEffect, useState } from 'react';

import { api } from '@/lib/api-client';
import { mapApiFolder, type ApiFolder } from '@/lib/mail-api';
import { t } from '@/i18n';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select';
import type { MailFolder, SupportedLocale } from '@/types';

const MAPPABLE_ROLES = ['archive', 'sent', 'drafts', 'spam', 'trash'] as const;
type MappableRole = (typeof MAPPABLE_ROLES)[number];

const DETECTED = '__detected__';

interface FolderRoleMappingProps {
  accountId: string;
  locale: SupportedLocale;
}

export function FolderRoleMapping({ accountId, locale }: FolderRoleMappingProps) {
  const [folders, setFolders] = useState<MailFolder[]>([]);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const data = await api<ApiFolder[]>('/folders');
        if (cancelled) return;
        setFolders(
          data
            .filter((f) => f.accountId === accountId)
            .map(mapApiFolder)
            .sort((a, b) => a.name.localeCompare(b.name)),
        );
      } catch (err: unknown) {
        if (!cancelled) {
          setError(err instanceof Error ? err.message : String(err));
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [accountId]);

  function folderIdForRole(role: MappableRole): string {
    const overridden = folders.find((f) => f.roleOverride === role);
    if (overridden) return overridden.id;
    return DETECTED;
  }

  async function handleChange(role: MappableRole, folderId: string) {
    setSaving(true);
    setError(null);
    try {
      const current = folders.find((f) => f.roleOverride === role);
      if (folderId === DETECTED) {
        if (current) {
          await api(`/folders/${current.id}`, {
            method: 'PATCH',
            body: JSON.stringify({ clearRoleOverride: true }),
          });
        }
      } else {
        await api(`/folders/${folderId}`, {
          method: 'PATCH',
          body: JSON.stringify({ roleOverride: role }),
        });
      }
      const data = await api<ApiFolder[]>('/folders');
      setFolders(
        data
          .filter((f) => f.accountId === accountId)
          .map(mapApiFolder)
          .sort((a, b) => a.name.localeCompare(b.name)),
      );
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  }

  if (folders.length === 0 && !error) {
    return (
      <p className="text-xs text-ter-foreground">
        {t(locale, 'settings.accounts.folderRolesEmpty')}
      </p>
    );
  }

  return (
    <fieldset className="space-y-3">
      <legend>{t(locale, 'settings.accounts.folderRoles')}</legend>
      <p className="text-xs text-ter-foreground">
        {t(locale, 'settings.accounts.folderRolesHint')}
      </p>
      {error ? <div className="text-sm text-destructive">{error}</div> : null}
      {MAPPABLE_ROLES.map((role) => (
        <div key={role} className="flex items-center justify-between gap-3">
          <label className="text-sm font-medium" htmlFor={`folder-role-${role}`}>
            {t(locale, `mail.folder.${role}`)}
          </label>
          <Select
            value={folderIdForRole(role)}
            onValueChange={(value) => void handleChange(role, value)}
            disabled={saving}
          >
            <SelectTrigger id={`folder-role-${role}`} size="sm" className="min-w-[180px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={DETECTED}>
                {t(locale, 'settings.accounts.folderRoleDetected')}
              </SelectItem>
              {folders.map((folder) => (
                <SelectItem key={folder.id} value={folder.id}>
                  {folder.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
      ))}
    </fieldset>
  );
}
