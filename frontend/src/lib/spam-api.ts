/**
 * Anti-spam settings (`/api/v1/settings/spam`): filtering toggles,
 * sensitivity, and the learned/blocked sender lists.
 */

import { api } from '@/lib/api-client';

export type SpamSensitivity = 'lenient' | 'standard' | 'strict';
export type SenderList = 'blocked' | 'allowed';

export interface SpamSettings {
  enabled: boolean;
  learn: boolean;
  autoDelete: boolean;
  sensitivity: SpamSensitivity;
}

export interface SenderEntry {
  email: string;
  list: SenderList;
}

export interface SpamSettingsResponse extends SpamSettings {
  senders: SenderEntry[];
}

export async function fetchSpamSettings(): Promise<SpamSettingsResponse> {
  return api<SpamSettingsResponse>('/settings/spam');
}

export async function saveSpamSettings(settings: SpamSettings): Promise<SpamSettingsResponse> {
  return api<SpamSettingsResponse>('/settings/spam', {
    method: 'PUT',
    body: JSON.stringify(settings),
  });
}

export async function addSpamSender(
  email: string,
  list: SenderList,
): Promise<SpamSettingsResponse> {
  return api<SpamSettingsResponse>('/settings/spam/senders', {
    method: 'POST',
    body: JSON.stringify({ email, list }),
  });
}

export async function removeSpamSender(email: string): Promise<SpamSettingsResponse> {
  return api<SpamSettingsResponse>(`/settings/spam/senders/${encodeURIComponent(email)}`, {
    method: 'DELETE',
  });
}
