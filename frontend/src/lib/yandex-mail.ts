/**
 * Yandex mailbox domains that should use OAuth2 (XOAUTH2).
 * Keep in sync with `is_yandex_mail_domain` in `backend/src/accounts.rs`.
 */

const EXACT_DOMAINS = new Set([
  'yandex.ru',
  'yandex.com',
  'ya.ru',
  'yandex.by',
  'yandex.kz',
  'yandex.ua',
  'yandex.com.tr',
  'yandex.az',
  'yandex.co.il',
  'yandex.lv',
  'yandex.ee',
  'yandex.lt',
  'yandex.md',
  'yandex.tj',
  'yandex.tm',
  'narod.ru',
]);

export function extractMailDomain(emailOrDomain: string): string {
  const trimmed = emailOrDomain.trim().toLowerCase();
  const at = trimmed.lastIndexOf('@');
  return at >= 0 ? trimmed.slice(at + 1) : trimmed;
}

export function isYandexMailDomain(emailOrDomain: string): boolean {
  const domain = extractMailDomain(emailOrDomain);
  if (!domain) return false;
  if (EXACT_DOMAINS.has(domain)) return true;
  return domain.endsWith('.yandex.ru') || domain.endsWith('.yandex.com');
}

export function isYandexMailHost(host: string): boolean {
  const h = host.trim().toLowerCase();
  return h === 'imap.yandex.com' || h === 'smtp.yandex.com' || h.endsWith('.yandex.com');
}
