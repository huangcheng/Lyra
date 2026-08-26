/**
 * Microsoft consumer / 365 mailbox domains that should use OAuth2 (XOAUTH2).
 * Keep in sync with `is_microsoft_mail_domain` in `backend/src/accounts.rs`.
 */

const EXACT_DOMAINS = new Set([
  'outlook.com',
  'hotmail.com',
  'live.com',
  'live.in',
  'msn.com',
  'passport.com',
  'office365.com',
]);

const DOMAIN_SUFFIXES = [
  '.outlook.com',
  '.hotmail.com',
  '.live.com',
  '.msn.com',
  '.onmicrosoft.com',
] as const;

export function extractMailDomain(emailOrDomain: string): string {
  const trimmed = emailOrDomain.trim().toLowerCase();
  const at = trimmed.lastIndexOf('@');
  return at >= 0 ? trimmed.slice(at + 1) : trimmed;
}

export function isMicrosoftMailDomain(emailOrDomain: string): boolean {
  const domain = extractMailDomain(emailOrDomain);
  if (!domain) return false;
  if (EXACT_DOMAINS.has(domain)) return true;
  return DOMAIN_SUFFIXES.some((suffix) => domain.endsWith(suffix));
}

export function isMicrosoftMailHost(host: string): boolean {
  const h = host.trim().toLowerCase();
  return (
    h === 'outlook.office365.com' ||
    h === 'outlook.office.com' ||
    h === 'smtp-mail.outlook.com' ||
    h.endsWith('.office365.com') ||
    h.endsWith('.outlook.com')
  );
}
