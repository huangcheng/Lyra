/**
 * OpenGPG keys + unlock/settings client (`/api/v1/opengpg`, `/settings/opengpg`).
 */

import { api } from '@/lib/api-client';

export type CacheMode = 'once' | 'timed' | 'session';

export interface OpengpgKey {
  id: string;
  fingerprint: string;
  primaryEmail: string;
  emails: string[];
  isSecret: boolean;
  isPrimary: boolean;
  revoked: boolean;
  createdAt?: string;
  updatedAt?: string;
}

export interface OpengpgSettings {
  passphraseCache: {
    mode: CacheMode | string;
    ttlMinutes: number;
  };
}

export interface UnlockResult {
  keyId: string;
  cache: string;
  unlocked: boolean;
  cached: boolean;
}

export async function listOpengpgKeys(): Promise<OpengpgKey[]> {
  return api<OpengpgKey[]>('/opengpg/keys');
}

export async function importOpengpgKey(armored: string, isPrimary = false): Promise<OpengpgKey> {
  return api<OpengpgKey>('/opengpg/keys', {
    method: 'POST',
    body: JSON.stringify({ armored, isPrimary }),
  });
}

export async function generateOpengpgKey(input: {
  email: string;
  name: string;
  passphrase: string;
  algorithm?: 'rsa4096' | 'ed25519';
}): Promise<OpengpgKey> {
  return api<OpengpgKey>('/opengpg/keys/generate', {
    method: 'POST',
    body: JSON.stringify(input),
  });
}

export async function setPrimaryOpengpgKey(id: string): Promise<OpengpgKey> {
  return api<OpengpgKey>(`/opengpg/keys/${id}`, {
    method: 'PATCH',
    body: JSON.stringify({ isPrimary: true }),
  });
}

export async function deleteOpengpgKey(id: string): Promise<void> {
  await api<void>(`/opengpg/keys/${id}`, { method: 'DELETE' });
}

export async function exportOpengpgKey(
  id: string,
  opts: { includeSecret?: boolean; currentPassword?: string } = {},
): Promise<{ armored: string; isSecret: boolean }> {
  const q = opts.includeSecret ? '?includeSecret=true' : '';
  const headers: HeadersInit = {};
  if (opts.includeSecret && opts.currentPassword) {
    headers['X-Lyra-Current-Password'] = opts.currentPassword;
  }
  return api<{ armored: string; isSecret: boolean }>(`/opengpg/keys/${id}/export${q}`, {
    headers,
  });
}

export async function unlockOpengpgKey(input: {
  keyId: string;
  passphrase: string;
  cache: CacheMode;
  ttlMinutes?: number;
}): Promise<UnlockResult> {
  return api<UnlockResult>('/opengpg/unlock', {
    method: 'POST',
    body: JSON.stringify({
      keyId: input.keyId,
      passphrase: input.passphrase,
      cache: input.cache,
      ttlMinutes: input.ttlMinutes,
    }),
  });
}

export async function lockOpengpgKeys(keyId?: string): Promise<{ unlockedIds: string[] }> {
  return api<{ unlockedIds: string[] }>('/opengpg/lock', {
    method: 'POST',
    body: JSON.stringify(keyId ? { keyId } : {}),
  });
}

export async function fetchOpengpgSettings(): Promise<OpengpgSettings> {
  return api<OpengpgSettings>('/settings/opengpg');
}

export async function updateOpengpgSettings(
  passphraseCache: OpengpgSettings['passphraseCache'],
): Promise<OpengpgSettings> {
  return api<OpengpgSettings>('/settings/opengpg', {
    method: 'PATCH',
    body: JSON.stringify({ passphraseCache }),
  });
}

export interface RecipientKeyMatch {
  id: string;
  fingerprint: string;
  primaryEmail: string;
}

export interface RecipientKeyLookup {
  email: string;
  keys: RecipientKeyMatch[];
  ambiguous: boolean;
  selectedKeyId?: string;
}

/** Lookup imported public keys for compose-time encrypt indicators. */
export async function lookupRecipientKeys(emails: string[]): Promise<RecipientKeyLookup[]> {
  if (emails.length === 0) return [];
  const q = encodeURIComponent(emails.join(','));
  return api<RecipientKeyLookup[]>(`/opengpg/recipient-keys?emails=${q}`);
}

export interface OpengpgSendOptions {
  sign?: boolean;
  encrypt?: boolean;
  attachPublicKey?: boolean;
  signingKeyId?: string;
  recipientKeyIds?: Record<string, string>;
}

/** Download armored text as a file (browser). */
export function downloadArmored(filename: string, armored: string): void {
  const blob = new Blob([armored], { type: 'application/pgp-keys' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  a.click();
  URL.revokeObjectURL(url);
}
