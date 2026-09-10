/**
 * Sentry for the SPA — opt-in via the backend's env config.
 *
 * The frontend DSN is served by `/version` (unauthenticated, unversioned):
 * it's a public client key, and this lets one env var (SENTRY_FRONTEND_DSN,
 * falling back to SENTRY_DSN) enable both sides without a rebuild. Init
 * happens before the first render so boot-time JS errors are captured.
 * Unset DSN ⇒ the SDK never initializes and nothing leaves the instance.
 */

import * as Sentry from '@sentry/react';

export interface VersionInfo {
  version: string;
  sentryDsn?: string | null;
}

/** Last-seen `/version` payload (settings reads it for the status row). */
let lastVersionInfo: VersionInfo | null = null;

export function getVersionInfo(): VersionInfo | null {
  return lastVersionInfo;
}

/** DSN shape guard — anything malformed keeps Sentry off. */
export function shouldInitSentry(dsn: string | null | undefined): boolean {
  return typeof dsn === 'string' && /^https:\/\/[^@]+@.+\..+/.test(dsn);
}

export async function fetchVersionInfo(): Promise<VersionInfo | null> {
  try {
    const res = await fetch('/version');
    if (!res.ok) return null;
    lastVersionInfo = (await res.json()) as VersionInfo;
    return lastVersionInfo;
  } catch {
    // Offline/dev without proxy — Sentry stays off, the app boots anyway.
    return null;
  }
}

export function initSentryFromVersion(info: VersionInfo | null): void {
  if (!info || !shouldInitSentry(info.sentryDsn)) return;
  Sentry.init({
    dsn: info.sentryDsn as string,
    release: `lyra-web@${info.version}`,
    // Errors only by default; performance tracing stays off unless asked
    // for later (matches the backend's default SENTRY_TRACES_SAMPLE_RATE=0).
    tracesSampleRate: 0,
    debug: new URLSearchParams(window.location.search).has('sentryDebug'),
  });
}

export { Sentry };
