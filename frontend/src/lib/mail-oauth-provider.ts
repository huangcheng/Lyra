import { isMicrosoftMailDomain } from '@/lib/microsoft-mail';
import { isYandexMailDomain } from '@/lib/yandex-mail';

export type MailOAuthProvider = 'microsoft' | 'yandex';

export function resolveMailOAuthProvider(emailOrDomain: string): MailOAuthProvider | null {
  if (isMicrosoftMailDomain(emailOrDomain)) return 'microsoft';
  if (isYandexMailDomain(emailOrDomain)) return 'yandex';
  return null;
}

export function suggestsMailOAuth(emailOrDomain: string): boolean {
  return resolveMailOAuthProvider(emailOrDomain) !== null;
}
