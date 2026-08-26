/**
 * Mail-account OAuth (`/api/v1/oauth/*`).
 *
 * App login stays username/password (+ TOTP). Mail OAuth uses one shared callback
 * URL (`{LYRA_PUBLIC_URL}/api/v1/oauth/callback`); provider is inferred from email.
 */

import { api } from '@/lib/api-client';

export interface OAuthProviderInfo {
  id: string;
  displayName: string;
  configured: boolean;
}

export interface OAuthProvidersResponse {
  providers: OAuthProviderInfo[];
}

export interface OAuthStartResponse {
  authorizeUrl: string;
  provider: string;
}

export async function fetchOAuthProviders(): Promise<OAuthProvidersResponse> {
  return api<OAuthProvidersResponse>('/oauth/providers');
}

export async function startOAuth(email: string): Promise<OAuthStartResponse> {
  const q = new URLSearchParams({ email: email.trim() });
  return api<OAuthStartResponse>(`/oauth/start?${q}`);
}

/** True when a mail OAuth provider is configured on this server. */
export async function isMailOAuthConfigured(providerId: string): Promise<boolean> {
  const { providers } = await fetchOAuthProviders();
  return providers.some((p) => p.id === providerId && p.configured);
}

/** True when Microsoft mail OAuth is configured on this server. */
export async function isMicrosoftOAuthConfigured(): Promise<boolean> {
  return isMailOAuthConfigured('microsoft');
}

/** True when Yandex mail OAuth is configured on this server. */
export async function isYandexOAuthConfigured(): Promise<boolean> {
  return isMailOAuthConfigured('yandex');
}
