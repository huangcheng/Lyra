/**
 * Contacts — Fastmail-style three pane (books · A–Z list · detail).
 */

import { useState, useEffect, useMemo } from 'react';
import { Link } from '@tanstack/react-router';
import { Mail, Phone, UserRound, Users, Building2, Search } from 'lucide-react';
import { t } from '../i18n';
import { api } from '../lib/api-client';
import { useAvatar } from '@/lib/avatar';
import {
  filterContacts,
  groupContactsByLetter,
  uniqueAddressbooks,
  type BookFilter,
} from '@/lib/contacts-ui';
import { EmptyState } from './empty-state';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { cn } from '@/lib/utils';
import { useUIStore } from '../stores/ui';

interface Contact {
  id: string;
  accountId: string;
  displayName?: string;
  emailAddresses: string[];
  phoneNumbers: string[];
  organisation?: string;
  photoPath?: string;
  addressbookUrl?: string;
  createdAt: string;
  updatedAt: string;
}

function getInitials(name?: string): string {
  if (!name) return '?';
  const parts = name.split(/\s+/);
  if (parts.length === 1) return parts[0].charAt(0).toUpperCase();
  return (parts[0].charAt(0) + parts[parts.length - 1].charAt(0)).toUpperCase();
}

function ContactAvatar({
  email,
  name,
  className,
}: {
  email?: string;
  name?: string;
  className: string;
}) {
  const avatarUrl = useAvatar(email);
  if (avatarUrl) {
    return (
      <img
        src={avatarUrl}
        alt={name ?? ''}
        className={cn('rounded-full object-cover', className)}
      />
    );
  }
  return (
    <span
      className={cn(
        'flex items-center justify-center rounded-full bg-primary/10 font-medium text-primary',
        className,
      )}
    >
      {getInitials(name)}
    </span>
  );
}

export function ContactsPage() {
  const locale = useUIStore((s) => s.locale);
  const [contacts, setContacts] = useState<Contact[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [searchQuery, setSearchQuery] = useState('');
  const [bookFilter, setBookFilter] = useState<BookFilter>('all');
  const [selectedId, setSelectedId] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        setLoading(true);
        setError(null);
        const data = await api<Contact[]>('/contacts?limit=500');
        if (cancelled) return;
        setContacts(data);
        setSelectedId((prev) => prev ?? data[0]?.id ?? null);
      } catch (err: unknown) {
        if (!cancelled) {
          setError(err instanceof Error ? err.message : String(err));
        }
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const books = useMemo(() => uniqueAddressbooks(contacts), [contacts]);
  const visible = useMemo(
    () => filterContacts(contacts, bookFilter, searchQuery),
    [contacts, bookFilter, searchQuery],
  );
  const groups = useMemo(() => groupContactsByLetter(visible), [visible]);
  const selected = contacts.find((c) => c.id === selectedId) ?? null;

  return (
    <div className="flex h-svh flex-col bg-background">
      <header className="flex h-14 shrink-0 items-center gap-3 border-b px-4">
        <Button variant="ghost" size="sm" asChild>
          <Link to="/">{t(locale, 'common.back')}</Link>
        </Button>
        <h1 className="text-lg font-semibold">{t(locale, 'contacts.title')}</h1>
        <div className="relative ml-auto w-full max-w-sm">
          <Search className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-muted-foreground" />
          <Input
            className="h-8 pl-8"
            placeholder={t(locale, 'contacts.search')}
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            aria-label={t(locale, 'contacts.search')}
          />
        </div>
      </header>

      <div className="flex min-h-0 flex-1">
        {/* Books rail */}
        <aside className="flex w-52 shrink-0 flex-col gap-1 overflow-y-auto border-r bg-muted/20 p-3">
          <button
            type="button"
            className={cn(
              'rounded-md px-2.5 py-1.5 text-left text-sm transition-colors hover:bg-accent',
              bookFilter === 'all' && 'bg-accent font-medium',
            )}
            onClick={() => setBookFilter('all')}
          >
            {t(locale, 'contacts.all')}
          </button>
          {books.length > 0 ? (
            <div className="mt-2 space-y-0.5">
              <p className="px-2.5 text-[10.5px] font-medium tracking-wide text-muted-foreground uppercase">
                {t(locale, 'contacts.addressBooks')}
              </p>
              {books.map((b) => {
                const active =
                  bookFilter !== 'all' &&
                  bookFilter.accountId === b.accountId &&
                  bookFilter.addressbookUrl === b.addressbookUrl;
                return (
                  <button
                    key={`${b.accountId}:${b.addressbookUrl}`}
                    type="button"
                    className={cn(
                      'w-full rounded-md px-2.5 py-1.5 text-left text-sm transition-colors hover:bg-accent',
                      active && 'bg-accent font-medium',
                    )}
                    onClick={() =>
                      setBookFilter({
                        accountId: b.accountId,
                        addressbookUrl: b.addressbookUrl,
                      })
                    }
                  >
                    {b.label === 'Personal'
                      ? t(locale, 'contacts.personal')
                      : b.label === 'Shared'
                        ? t(locale, 'contacts.shared')
                        : b.label}
                  </button>
                );
              })}
            </div>
          ) : null}
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="mt-auto h-8 justify-start text-xs"
            disabled
            title={t(locale, 'contacts.addBookSoon')}
          >
            + {t(locale, 'contacts.addBook')}
          </Button>
        </aside>

        {/* A–Z list */}
        <section className="flex w-80 shrink-0 flex-col overflow-y-auto border-r">
          {loading ? (
            <div className="p-4 text-sm text-muted-foreground">{t(locale, 'common.loading')}</div>
          ) : error ? (
            <div className="p-4 text-sm text-destructive">{error}</div>
          ) : visible.length === 0 ? (
            <div className="p-4">
              <EmptyState
                icon={Users}
                title={t(locale, 'contacts.empty')}
                hint={t(locale, 'contacts.emptyHint')}
              />
            </div>
          ) : (
            groups.map((g) => (
              <div key={g.letter}>
                <div className="sticky top-0 z-10 bg-background/95 px-3 py-1 text-[11px] font-medium text-muted-foreground backdrop-blur">
                  {g.letter}
                </div>
                {g.contacts.map((contact) => (
                  <button
                    key={contact.id}
                    type="button"
                    className={cn(
                      'flex w-full items-center gap-3 px-3 py-2 text-left transition-colors hover:bg-accent',
                      selectedId === contact.id && 'bg-muted',
                    )}
                    onClick={() => setSelectedId(contact.id)}
                  >
                    <ContactAvatar
                      email={contact.emailAddresses[0]}
                      name={contact.displayName}
                      className="h-9 w-9 shrink-0 text-sm"
                    />
                    <span className="min-w-0">
                      <span className="block truncate text-sm font-medium">
                        {contact.displayName || t(locale, 'contacts.noName')}
                      </span>
                      {contact.emailAddresses[0] ? (
                        <span className="block truncate text-xs text-muted-foreground">
                          {contact.emailAddresses[0]}
                        </span>
                      ) : null}
                    </span>
                  </button>
                ))}
              </div>
            ))
          )}
        </section>

        {/* Detail */}
        <section className="min-w-0 flex-1 overflow-y-auto p-8">
          {selected ? (
            <div className="mx-auto max-w-lg space-y-8">
              <div className="flex flex-col items-start gap-4 sm:flex-row sm:items-center">
                <ContactAvatar
                  email={selected.emailAddresses[0]}
                  name={selected.displayName}
                  className="h-16 w-16 text-2xl"
                />
                <div className="min-w-0">
                  <h2 className="truncate text-xl font-semibold">
                    {selected.displayName || t(locale, 'contacts.noName')}
                  </h2>
                  {selected.organisation ? (
                    <p className="text-sm text-muted-foreground">{selected.organisation}</p>
                  ) : null}
                </div>
              </div>

              <div className="flex flex-wrap gap-2">
                {selected.emailAddresses[0] ? (
                  <Button variant="secondary" size="sm" asChild>
                    <a href={`mailto:${selected.emailAddresses[0]}`}>
                      <Mail className="size-3.5" />
                      {t(locale, 'contacts.compose')}
                    </a>
                  </Button>
                ) : null}
                {selected.phoneNumbers[0] ? (
                  <Button variant="secondary" size="sm" asChild>
                    <a href={`tel:${selected.phoneNumbers[0]}`}>
                      <Phone className="size-3.5" />
                      {t(locale, 'contacts.call')}
                    </a>
                  </Button>
                ) : null}
              </div>

              {selected.emailAddresses.length > 0 ? (
                <div className="space-y-2">
                  <h3 className="flex items-center gap-1.5 text-xs font-medium tracking-wide text-muted-foreground uppercase">
                    <Mail className="size-3" />
                    {t(locale, 'contacts.email')}
                  </h3>
                  {selected.emailAddresses.map((email) => (
                    <a
                      key={email}
                      href={`mailto:${email}`}
                      className="block text-sm text-primary hover:underline"
                    >
                      {email}
                    </a>
                  ))}
                </div>
              ) : null}

              {selected.phoneNumbers.length > 0 ? (
                <div className="space-y-2">
                  <h3 className="flex items-center gap-1.5 text-xs font-medium tracking-wide text-muted-foreground uppercase">
                    <Phone className="size-3" />
                    {t(locale, 'contacts.phone')}
                  </h3>
                  {selected.phoneNumbers.map((phone) => (
                    <a
                      key={phone}
                      href={`tel:${phone}`}
                      className="block text-sm text-primary hover:underline"
                    >
                      {phone}
                    </a>
                  ))}
                </div>
              ) : null}

              {selected.organisation ? (
                <div className="space-y-2">
                  <h3 className="flex items-center gap-1.5 text-xs font-medium tracking-wide text-muted-foreground uppercase">
                    <Building2 className="size-3" />
                    {t(locale, 'contacts.organisation')}
                  </h3>
                  <p className="text-sm">{selected.organisation}</p>
                </div>
              ) : null}
            </div>
          ) : (
            <EmptyState icon={UserRound} title={t(locale, 'contacts.selectContact')} />
          )}
        </section>
      </div>
    </div>
  );
}
