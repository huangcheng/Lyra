/**
 * Microsoft mail-account OAuth (`/api/v1/oauth/microsoft/*`).
 *
 * App login stays username/password (+ TOTP). This only connects Outlook/M365
 * mailboxes via authorization code + PKCE.
 */

import { api } from '@/lib/api-client';

export interface MsOAuthStatus {
  configured: boolean;
}

export interface MsOAuthStart {
  authorizeUrl: string;
}

export async function fetchMsOAuthStatus(): Promise<MsOAuthStatus> {
  return api<MsOAuthStatus>('/oauth/microsoft/status');
}

export async function startMsOAuth(): Promise<MsOAuthStart> {
  return api<MsOAuthStart>('/oauth/microsoft/start');
}
