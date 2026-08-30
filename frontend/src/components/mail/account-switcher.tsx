/**
 * Account switcher — shadcn v3 mail example, plus unified "All inboxes".
 */

import { Inbox, Mail } from 'lucide-react';
import type { ReactNode } from 'react';

import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select';
import { t } from '@/i18n';
import { ALL_ACCOUNTS } from '@/lib/mail-api';
import { cn } from '@/lib/utils';
import { useMailStore } from '@/stores/mail';
import { useUIStore } from '@/stores/ui';

function AccountIcon({ kind, className }: { kind: 'all' | 'mail'; className?: string }) {
  const Icon = kind === 'all' ? Inbox : Mail;
  return <Icon className={cn('h-4 w-4 shrink-0', className)} />;
}

export function AccountSwitcher({ isCollapsed }: { isCollapsed: boolean }) {
  const locale = useUIStore((s) => s.locale);
  const accounts = useMailStore((s) => s.accounts);
  const selectedAccountId = useUIStore((s) => s.selectedAccountId);
  const setSelectedAccount = useUIStore((s) => s.setSelectedAccount);

  const options: { id: string; label: string; email: string; icon: ReactNode }[] = [
    {
      id: ALL_ACCOUNTS,
      label: t(locale, 'nav.allInboxes'),
      email: t(locale, 'nav.allInboxes'),
      icon: <AccountIcon kind="all" />,
    },
    ...accounts.map((account) => ({
      id: account.id,
      label: account.displayName || account.emailAddress,
      email: account.emailAddress,
      icon: <AccountIcon kind="mail" />,
    })),
  ];

  const selected = options.find((account) => account.id === selectedAccountId) ?? options[0];

  return (
    <Select value={selectedAccountId} onValueChange={setSelectedAccount}>
      <SelectTrigger
        className={cn(
          'flex w-full items-center gap-2 border border-input bg-card shadow-whisper [&>span]:line-clamp-1 [&>span]:flex [&>span]:w-full [&>span]:items-center [&>span]:gap-1 [&>span]:truncate [&_svg]:h-4 [&_svg]:w-4 [&_svg]:shrink-0',
          isCollapsed &&
            'flex h-9 w-9 shrink-0 items-center justify-center p-0 [&>span]:w-auto [&>svg]:hidden',
        )}
        aria-label={t(locale, 'nav.accounts')}
      >
        <SelectValue placeholder={t(locale, 'nav.accounts')}>
          {selected?.icon}
          <span className={cn('ml-2', isCollapsed && 'hidden')}>{selected?.label}</span>
        </SelectValue>
      </SelectTrigger>
      <SelectContent>
        {options.map((account) => (
          <SelectItem key={account.id} value={account.id}>
            <div className="flex items-center gap-3 [&_svg]:h-4 [&_svg]:w-4 [&_svg]:shrink-0 [&_svg]:text-foreground">
              {account.icon}
              {account.email}
            </div>
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}
