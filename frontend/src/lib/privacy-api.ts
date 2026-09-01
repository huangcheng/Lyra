import { api } from '@/lib/api-client';

export interface PrivacySettings {
  remoteImages: 'block' | 'proxy';
  remoteContentAllowlist: string[];
  gravatarAvatars: boolean;
}

export async function fetchPrivacySettings(): Promise<PrivacySettings> {
  return api<PrivacySettings>('/settings/privacy');
}

export async function updatePrivacySettings(
  patch: Partial<Pick<PrivacySettings, 'remoteImages' | 'gravatarAvatars'>>,
): Promise<PrivacySettings> {
  return api<PrivacySettings>('/settings/privacy', {
    method: 'PATCH',
    body: JSON.stringify(patch),
  });
}

export async function allowSenderPrivacy(sender: string): Promise<PrivacySettings> {
  return api<PrivacySettings>('/settings/privacy/allow-sender', {
    method: 'POST',
    body: JSON.stringify({ sender }),
  });
}

export async function removeAllowSenderPrivacy(sender: string): Promise<PrivacySettings> {
  return api<PrivacySettings>(`/settings/privacy/allow-sender/${encodeURIComponent(sender)}`, {
    method: 'DELETE',
  });
}
