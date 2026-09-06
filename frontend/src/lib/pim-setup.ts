/**
 * Pure decision logic for the per-account Calendar & Contacts (CardDAV/CalDAV)
 * setup dialog.
 *
 * Extracted so the provider presets, connection-status derivation, and URL
 * rules are unit-testable without rendering the dialog — same pattern as
 * `account-form.ts`.
 */

/** Providers with known CalDAV/CardDAV behavior or app-password flows. */
export type PimProviderId = 'fastmail' | 'yandex' | 'icloud' | 'google' | 'microsoft' | 'generic';

/** Provider domains (matched as host-suffixes, so subdomains count). */
const PROVIDER_DOMAINS: Array<[PimProviderId, string[]]> = [
  ['fastmail', ['fastmail.com', 'fastmailusercontent.com']],
  ['yandex', ['yandex.ru', 'yandex.com', 'yandex.by', 'ya.ru']],
  ['icloud', ['icloud.com', 'me.com', 'mac.com']],
  ['google', ['gmail.com', 'googlemail.com']],
  ['microsoft', ['outlook.com', 'hotmail.com', 'live.com', 'live.in', 'msn.com']],
];

/** Detect the DAV provider preset from the account email address. */
export function pimProviderFor(email: string): PimProviderId {
  const domain = email.toLowerCase().split('@').pop() ?? '';
  if (!domain || !email.includes('@')) return 'generic';
  for (const [provider, suffixes] of PROVIDER_DOMAINS) {
    if (suffixes.some((s) => domain === s || domain.endsWith(`.${s}`))) {
      return provider;
    }
  }
  return 'generic';
}

export type PimSetupStatus =
  'off' | 'credentialOnly' | 'contactsOnly' | 'calendarsOnly' | 'connected';

/** Derive the dialog's connection status from account fields. */
export function pimSetupStatus(account: {
  hasCredential?: boolean;
  carddavUrl?: string | null;
  caldavUrl?: string | null;
}): PimSetupStatus {
  const carddav = account.carddavUrl?.trim() ?? '';
  const caldav = account.caldavUrl?.trim() ?? '';
  if (carddav && caldav) return 'connected';
  if (carddav) return 'contactsOnly';
  if (caldav) return 'calendarsOnly';
  if (account.hasCredential) return 'credentialOnly';
  return 'off';
}

/** i18n key with the provider-specific app-password hint. */
export function providerHintKey(provider: PimProviderId): string {
  return `settings.pim.hint${provider[0].toUpperCase()}${provider.slice(1)}`;
}

export type DavUrlResult = { ok: true; value: string } | { ok: false; errorKey: string };

/**
 * Validate a manually entered DAV URL. Empty input is an explicit clear
 * (`''`), mirroring the backend's update semantics. Only the URL *shape* is
 * checked here — scheme policy for public vs LAN hosts is enforced by the
 * server (`validate_server_url`).
 */
export function normalizeDavUrlInput(input: string): DavUrlResult {
  const value = input.trim();
  if (!value) return { ok: true, value: '' };
  try {
    const url = new URL(value);
    const schemeOk = url.protocol === 'https:' || url.protocol === 'http:';
    if (schemeOk && url.hostname) return { ok: true, value };
  } catch {
    // fall through to the error
  }
  return { ok: false, errorKey: 'settings.pim.urlInvalid' };
}
