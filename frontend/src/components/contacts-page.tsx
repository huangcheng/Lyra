/**
 * Contacts — Fastmail-style three pane (books · A–Z list · detail).
 *
 * Peer destination to Mail: slim-nav shell, quiet hairline rails,
 * restrained detail typography. Color means status only — everything
 * else stays cool gray.
 */

import { useState, useEffect, useMemo, useRef, type FormEvent, type ReactNode } from 'react';
import { BookUser, Mail, Phone, Plus, Search, UserRound } from 'lucide-react';
import { t } from '../i18n';
import { api } from '../lib/api-client';
import { useAvatar } from '@/lib/avatar';
import {
  filterContacts,
  groupContactsByLetter,
  indexLettersFromGroups,
  uniqueAddressbooks,
  type BookFilter,
} from '@/lib/contacts-ui';
import { useNavigate } from '@tanstack/react-router';
import { EmptyState } from './empty-state';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import { Field, FieldDescription, FieldGroup, FieldLabel } from '@/components/ui/field';
import { SlimPanelHeader } from '@/components/slim-page-nav';
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
        'flex items-center justify-center rounded-full bg-muted font-medium text-muted-foreground',
        className,
      )}
    >
      {getInitials(name)}
    </span>
  );
}

function DetailField({
  icon: Icon,
  label,
  children,
}: {
  icon: typeof Mail;
  label: string;
  children: ReactNode;
}) {
  return (
    <div className="flex gap-3">
      <Icon className="mt-0.5 size-3.5 shrink-0 text-muted-foreground" />
      <div className="min-w-0 flex-1">
        <p className="text-[10.5px] font-medium tracking-wide text-muted-foreground uppercase">
          {label}
        </p>
        <div className="mt-1 space-y-1 text-[13.5px]">{children}</div>
      </div>
    </div>
  );
}

export function ContactsPage() {
  const locale = useUIStore((s) => s.locale);
  const [contacts, setContacts] = useState<Contact[]>([]);
  const navigate = useNavigate();
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [searchQuery, setSearchQuery] = useState('');
  const [bookFilter, setBookFilter] = useState<BookFilter>('all');
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [accountLabels, setAccountLabels] = useState<Record<string, string>>({});
  const [carddavAccountIds, setCarddavAccountIds] = useState<string[]>([]);
  const [addOpen, setAddOpen] = useState(false);

  // Register this page's commands for the ⌘K palette.
  useEffect(() => {
    const setPageCommands = useUIStore.getState().setPageCommands;
    setPageCommands([
      {
        id: 'new-contact',
        label: t(useUIStore.getState().locale, 'contacts.new'),
        icon: 'UserPlusIcon',
        keywords: ['new contact', 'contact', '新建联系人'],
        onSelect: () => setAddOpen(true),
      },
    ]);
    return () => setPageCommands([]);
  }, []);

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

  // Account email per book owner — the rail groups address books under
  // their account, which is the context a bare "Personal" label lacks.
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const accounts =
          await api<{ id: string; emailAddress: string; carddavUrl?: string | null }[]>(
            '/accounts',
          );
        if (!cancelled) {
          setAccountLabels(Object.fromEntries(accounts.map((a) => [a.id, a.emailAddress])));
          setCarddavAccountIds(accounts.filter((a) => a.carddavUrl).map((a) => a.id));
        }
      } catch {
        // rail falls back to book names without account headers
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
  const indexLetters = useMemo(() => indexLettersFromGroups(groups), [groups]);
  const listScrollRef = useRef<HTMLDivElement>(null);

  const jumpToLetter = (letter: string) => {
    const el = listScrollRef.current?.querySelector(`[data-letter="${CSS.escape(letter)}"]`);
    el?.scrollIntoView({ block: 'start' });
  };

  const letterFromPointer = (clientY: number, nav: HTMLElement): string | null => {
    const buttons = [...nav.querySelectorAll<HTMLElement>('[data-index-letter]')];
    if (buttons.length === 0) return null;
    for (const btn of buttons) {
      const r = btn.getBoundingClientRect();
      if (clientY >= r.top && clientY <= r.bottom) {
        return btn.dataset.indexLetter ?? null;
      }
    }
    // Clamp to nearest end when dragging past the rail.
    const first = buttons[0]!.getBoundingClientRect();
    const last = buttons[buttons.length - 1]!.getBoundingClientRect();
    if (clientY < first.top) return buttons[0]!.dataset.indexLetter ?? null;
    if (clientY > last.bottom) return buttons[buttons.length - 1]!.dataset.indexLetter ?? null;
    return null;
  };

  const booksByAccount = useMemo(() => {
    const m = new Map<string, typeof books>();
    for (const b of books) {
      const list = m.get(b.accountId) ?? [];
      list.push(b);
      m.set(b.accountId, list);
    }
    return [...m.entries()];
  }, [books]);
  const countByBook = useMemo(() => {
    const m = new Map<string, number>();
    for (const c of contacts) {
      if (!c.addressbookUrl) continue;
      const key = `${c.accountId}\0${c.addressbookUrl}`;
      m.set(key, (m.get(key) ?? 0) + 1);
    }
    return m;
  }, [contacts]);
  const selected = contacts.find((c) => c.id === selectedId) ?? null;

  return (
    <div className="flex h-svh">
      <aside className="flex w-[232px] shrink-0 flex-col overflow-y-auto border-r bg-secondary px-2 py-3">
        <SlimPanelHeader />
        <button
          type="button"
          className={cn(
            'mt-2 flex items-center justify-between rounded-[7px] px-2.5 py-1.5 text-left text-[13px] transition-colors hover:bg-accent',
            bookFilter === 'all' && 'bg-accent font-medium',
          )}
          onClick={() => setBookFilter('all')}
        >
          {t(locale, 'contacts.all')}
          {bookFilter === 'all' && contacts.length > 0 ? (
            <span className="text-[11px] text-muted-foreground tabular-nums">
              {contacts.length}
            </span>
          ) : null}
        </button>
        {booksByAccount.map(([accountId, accountBooks]) => (
          <div key={accountId} className="mt-3 space-y-0.5">
            <p
              className="truncate px-2.5 pb-1 text-[10.5px] font-medium tracking-wide text-muted-foreground/80"
              title={accountLabels[accountId] ?? accountId}
            >
              {accountLabels[accountId] ?? accountId}
            </p>
            {accountBooks.map((b) => {
              const active =
                bookFilter !== 'all' &&
                bookFilter.accountId === b.accountId &&
                bookFilter.addressbookUrl === b.addressbookUrl;
              const count = countByBook.get(`${b.accountId}\0${b.addressbookUrl}`) ?? 0;
              return (
                <button
                  key={`${b.accountId}:${b.addressbookUrl}`}
                  type="button"
                  className={cn(
                    'flex w-full items-center justify-between gap-2 rounded-[7px] px-2.5 py-1.5 text-left text-[13px] transition-colors hover:bg-accent',
                    active && 'bg-accent font-medium',
                  )}
                  onClick={() =>
                    setBookFilter({
                      accountId: b.accountId,
                      addressbookUrl: b.addressbookUrl,
                    })
                  }
                >
                  <span className="min-w-0 truncate">
                    {b.label === 'Personal'
                      ? t(locale, 'contacts.personal')
                      : b.label === 'Shared'
                        ? t(locale, 'contacts.shared')
                        : b.label}
                  </span>
                  <span className="shrink-0 text-[11px] text-muted-foreground tabular-nums">
                    {count}
                  </span>
                </button>
              );
            })}
          </div>
        ))}
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="mt-auto justify-start text-muted-foreground"
          disabled={carddavAccountIds.length === 0}
          title={
            carddavAccountIds.length === 0
              ? t(locale, 'contacts.newNoAccount')
              : t(locale, 'contacts.new')
          }
          onClick={() => setAddOpen(true)}
        >
          <Plus className="size-3.5" />
          {t(locale, 'contacts.new')}
        </Button>
      </aside>

      <AddContactDialog
        open={addOpen}
        onOpenChange={setAddOpen}
        accountIds={carddavAccountIds}
        accountLabels={accountLabels}
        defaultAccountId={booksByAccount[0]?.[0] ?? carddavAccountIds[0]}
        onCreated={(id) => {
          void (async () => {
            const data = await api<Contact[]>('/contacts?limit=500');
            setContacts(data);
            setSelectedId(id);
          })();
        }}
      />

      <main className="flex min-w-0 flex-1 flex-col">
        <header className="flex h-14 shrink-0 items-center gap-3 border-b px-5">
          <h1 className="font-display truncate text-xl font-medium">
            {t(locale, 'contacts.title')}
            {visible.length > 0 ? (
              <span className="ml-2 text-sm font-normal text-muted-foreground tabular-nums">
                {visible.length}
              </span>
            ) : null}
          </h1>
          <div className="relative ml-auto w-full max-w-xs">
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
          {/* A–Z list + Apple-style index rail */}
          <section className="relative flex w-80 shrink-0 flex-col border-r">
            <div ref={listScrollRef} className="min-h-0 flex-1 overflow-y-auto">
              {loading ? (
                <div className="p-4 text-sm text-muted-foreground">
                  {t(locale, 'common.loading')}
                </div>
              ) : error ? (
                <div className="p-4 text-sm text-destructive">{error}</div>
              ) : visible.length === 0 ? (
                <div className="p-4">
                  <EmptyState
                    icon={BookUser}
                    title={t(locale, 'contacts.empty')}
                    hint={t(locale, 'contacts.emptyHint')}
                  />
                  <Button
                    variant="outline"
                    size="sm"
                    className="mt-3"
                    onClick={() => navigate({ to: '/settings', search: { pim: true } })}
                  >
                    {t(locale, 'contacts.connectDav')}
                  </Button>
                </div>
              ) : (
                groups.map((g) => (
                  <div key={g.letter} data-letter={g.letter}>
                    <div className="sticky top-0 z-10 border-b border-border/60 bg-background/95 px-4 py-1 pr-7 text-[10.5px] font-medium text-muted-foreground backdrop-blur">
                      {g.letter}
                    </div>
                    {g.contacts.map((contact) => (
                      <button
                        key={contact.id}
                        type="button"
                        className={cn(
                          'flex w-full items-center gap-3 px-4 py-2 pr-7 text-left transition-colors hover:bg-accent',
                          selectedId === contact.id && 'bg-accent',
                        )}
                        onClick={() => setSelectedId(contact.id)}
                      >
                        <ContactAvatar
                          email={contact.emailAddresses[0]}
                          name={contact.displayName}
                          className="size-9 shrink-0 text-[13px]"
                        />
                        <span className="min-w-0">
                          <span
                            className={cn(
                              'block truncate text-[13.5px]',
                              selectedId === contact.id
                                ? 'font-medium'
                                : 'font-normal text-foreground',
                            )}
                          >
                            {contact.displayName || t(locale, 'contacts.noName')}
                          </span>
                          {contact.emailAddresses[0] ? (
                            <span className="block truncate text-[11.5px] text-muted-foreground">
                              {contact.emailAddresses[0]}
                            </span>
                          ) : null}
                        </span>
                      </button>
                    ))}
                  </div>
                ))
              )}
            </div>
            {indexLetters.length > 1 ? (
              <nav
                aria-label={t(locale, 'contacts.alphabetIndex')}
                className="absolute top-1/2 right-0 z-20 flex max-h-[calc(100%-0.5rem)] -translate-y-1/2 flex-col items-center justify-center px-0.5 py-1 select-none"
                onPointerDown={(e) => {
                  e.currentTarget.setPointerCapture(e.pointerId);
                  const letter = letterFromPointer(e.clientY, e.currentTarget);
                  if (letter) jumpToLetter(letter);
                }}
                onPointerMove={(e) => {
                  if (!e.currentTarget.hasPointerCapture(e.pointerId)) return;
                  const letter = letterFromPointer(e.clientY, e.currentTarget);
                  if (letter) jumpToLetter(letter);
                }}
              >
                {indexLetters.map((letter) => (
                  <button
                    key={letter}
                    type="button"
                    data-index-letter={letter}
                    className="flex h-[1.05em] w-4 items-center justify-center text-[9px] leading-none font-medium text-muted-foreground hover:text-foreground"
                    onClick={() => jumpToLetter(letter)}
                  >
                    {letter}
                  </button>
                ))}
              </nav>
            ) : null}
          </section>

          {/* Detail */}
          <section className="min-w-0 flex-1 overflow-y-auto">
            {selected ? (
              <div className="mx-auto max-w-lg space-y-8 p-10">
                <div className="flex flex-col items-start gap-5 sm:flex-row sm:items-center">
                  <ContactAvatar
                    email={selected.emailAddresses[0]}
                    name={selected.displayName}
                    className="size-20 text-2xl"
                  />
                  <div className="min-w-0">
                    <h2 className="font-display truncate text-2xl leading-tight font-medium">
                      {selected.displayName || t(locale, 'contacts.noName')}
                    </h2>
                    {selected.organisation ? (
                      <p className="mt-1 truncate text-sm text-muted-foreground">
                        {selected.organisation}
                      </p>
                    ) : null}
                    <div className="mt-3 flex flex-wrap gap-2">
                      {selected.emailAddresses[0] ? (
                        <Button variant="outline" size="sm" className="h-8" asChild>
                          <a href={`mailto:${selected.emailAddresses[0]}`}>
                            <Mail className="size-3.5" />
                            {t(locale, 'contacts.compose')}
                          </a>
                        </Button>
                      ) : null}
                      {selected.phoneNumbers[0] ? (
                        <Button variant="outline" size="sm" className="h-8" asChild>
                          <a href={`tel:${selected.phoneNumbers[0]}`}>
                            <Phone className="size-3.5" />
                            {t(locale, 'contacts.call')}
                          </a>
                        </Button>
                      ) : null}
                    </div>
                  </div>
                </div>

                {selected.emailAddresses.length > 0 ? (
                  <DetailField icon={Mail} label={t(locale, 'contacts.email')}>
                    {selected.emailAddresses.map((email) => (
                      <a key={email} href={`mailto:${email}`} className="block hover:underline">
                        {email}
                      </a>
                    ))}
                  </DetailField>
                ) : null}

                {selected.phoneNumbers.length > 0 ? (
                  <DetailField icon={Phone} label={t(locale, 'contacts.phone')}>
                    {selected.phoneNumbers.map((phone) => (
                      <a key={phone} href={`tel:${phone}`} className="block hover:underline">
                        {phone}
                      </a>
                    ))}
                  </DetailField>
                ) : null}

                {selected.organisation ? (
                  <DetailField icon={UserRound} label={t(locale, 'contacts.organisation')}>
                    <p>{selected.organisation}</p>
                  </DetailField>
                ) : null}
              </div>
            ) : (
              <div className="flex h-full items-center justify-center p-8">
                <EmptyState icon={UserRound} title={t(locale, 'contacts.selectContact')} />
              </div>
            )}
          </section>
        </div>
      </main>
    </div>
  );
}

/** Fastmail-style create dialog: VCARD fields, saved via CardDAV PUT. */
function AddContactDialog({
  open,
  onOpenChange,
  accountIds,
  accountLabels,
  defaultAccountId,
  onCreated,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  accountIds: string[];
  accountLabels: Record<string, string>;
  defaultAccountId?: string;
  onCreated: (newId: string) => void;
}) {
  const locale = useUIStore((s) => s.locale);
  const [accountId, setAccountId] = useState(defaultAccountId ?? accountIds[0] ?? '');
  const [displayName, setDisplayName] = useState('');
  const [email, setEmail] = useState('');
  const [phone, setPhone] = useState('');
  const [organisation, setOrganisation] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (open) {
      setAccountId(defaultAccountId ?? accountIds[0] ?? '');
      setDisplayName('');
      setEmail('');
      setPhone('');
      setOrganisation('');
      setError(null);
      setBusy(false);
    }
  }, [open, defaultAccountId, accountIds]);

  async function submit(e: FormEvent) {
    e.preventDefault();
    if (!accountId || !displayName.trim()) return;
    setBusy(true);
    setError(null);
    try {
      const created = await api<Contact>('/contacts', {
        method: 'POST',
        body: JSON.stringify({
          accountId,
          displayName: displayName.trim(),
          email: email.trim() || undefined,
          phone: phone.trim() || undefined,
          organisation: organisation.trim() || undefined,
        }),
      });
      onOpenChange(false);
      onCreated(created.id);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-sm">
        <DialogHeader>
          <DialogTitle>{t(locale, 'contacts.newTitle')}</DialogTitle>
          <DialogDescription>
            {t(locale, 'contacts.newHint', {
              book: accountLabels[accountId] ?? '',
            })}
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={(e) => void submit(e)}>
          <FieldGroup>
            <Field>
              <FieldLabel htmlFor="new-contact-name">{t(locale, 'contacts.name')}</FieldLabel>
              <Input
                id="new-contact-name"
                value={displayName}
                placeholder={t(locale, 'contacts.namePlaceholder')}
                onChange={(e) => setDisplayName(e.target.value)}
                autoFocus
                required
              />
            </Field>
            <Field>
              <FieldLabel htmlFor="new-contact-email">{t(locale, 'contacts.email')}</FieldLabel>
              <Input
                id="new-contact-email"
                type="email"
                value={email}
                placeholder="name@example.com"
                onChange={(e) => setEmail(e.target.value)}
              />
            </Field>
            <Field>
              <FieldLabel htmlFor="new-contact-phone">{t(locale, 'contacts.phone')}</FieldLabel>
              <Input
                id="new-contact-phone"
                value={phone}
                onChange={(e) => setPhone(e.target.value)}
              />
            </Field>
            <Field>
              <FieldLabel htmlFor="new-contact-org">{t(locale, 'contacts.org')}</FieldLabel>
              <Input
                id="new-contact-org"
                value={organisation}
                placeholder={t(locale, 'contacts.orgPlaceholder')}
                onChange={(e) => setOrganisation(e.target.value)}
              />
            </Field>
            {accountIds.length > 1 ? (
              <Field>
                <FieldLabel htmlFor="new-contact-account">
                  {t(locale, 'settings.accounts.title')}
                </FieldLabel>
                <select
                  id="new-contact-account"
                  className="h-9 w-full rounded-md border border-input bg-transparent px-3 text-sm outline-none focus-visible:border-foreground/35"
                  value={accountId}
                  onChange={(e) => setAccountId(e.target.value)}
                >
                  {accountIds.map((id) => (
                    <option key={id} value={id}>
                      {accountLabels[id] ?? id}
                    </option>
                  ))}
                </select>
              </Field>
            ) : null}
            {error ? (
              <FieldDescription className="text-destructive">{error}</FieldDescription>
            ) : null}
          </FieldGroup>
          <DialogFooter className="mt-4">
            <Button type="button" variant="ghost" onClick={() => onOpenChange(false)}>
              {t(locale, 'common.cancel')}
            </Button>
            <Button type="submit" disabled={busy || !displayName.trim() || !accountId}>
              {t(locale, 'contacts.create')}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
