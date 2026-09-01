/**
 * Contacts page with CardDAV sync support.
 *
 * Displays contacts from all configured accounts.
 */

import { useState, useEffect } from 'react';
import { UserRound, Users } from 'lucide-react';
import { t } from '../i18n';
import { api } from '../lib/api-client';
import { useAvatar } from '@/lib/avatar';
import { EmptyState } from './empty-state';
import { SecondaryPage } from './secondary-page';
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
  createdAt: string;
  updatedAt: string;
}

function getInitials(name?: string): string {
  if (!name) return '?';
  const parts = name.split(/\s+/);
  if (parts.length === 1) return parts[0].charAt(0).toUpperCase();
  return (parts[0].charAt(0) + parts[parts.length - 1].charAt(0)).toUpperCase();
}

/** Contact photo when the backend has one; monogram otherwise. */
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
  const [selectedContact, setSelectedContact] = useState<Contact | null>(null);

  useEffect(() => {
    fetchContacts();
  }, []);

  async function fetchContacts(query?: string) {
    try {
      setLoading(true);
      const params = new URLSearchParams();
      if (query) params.set('q', query);
      const data = await api<Contact[]>(`/contacts?${params}`);
      setContacts(data);
    } catch (err: any) {
      setError(err.message);
    } finally {
      setLoading(false);
    }
  }

  function handleSearch(e: React.FormEvent) {
    e.preventDefault();
    fetchContacts(searchQuery);
  }

  return (
    <SecondaryPage title={t(locale, 'contacts.title')}>
      <div className="mx-auto flex max-w-4xl gap-6">
        <div className="w-72 shrink-0 space-y-3">
          <form onSubmit={handleSearch} className="flex gap-2">
            <Input
              placeholder={t(locale, 'contacts.search')}
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
            />
            <Button type="submit" variant="outline" size="sm">
              {t(locale, 'common.search')}
            </Button>
          </form>

          {loading ? (
            <div className="p-4 text-sm text-muted-foreground">{t(locale, 'common.loading')}</div>
          ) : error ? (
            <div className="p-4 text-sm text-destructive">{error}</div>
          ) : contacts.length === 0 ? (
            <EmptyState
              icon={Users}
              title={t(locale, 'contacts.empty')}
              hint={t(locale, 'contacts.emptyHint')}
            />
          ) : (
            <div className="space-y-1">
              {contacts.map((contact) => (
                <button
                  key={contact.id}
                  type="button"
                  className={cn(
                    'flex w-full items-center gap-3 rounded-lg border p-3 text-left transition-colors hover:bg-accent',
                    selectedContact?.id === contact.id && 'bg-muted',
                  )}
                  onClick={() => setSelectedContact(contact)}
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
                    {contact.emailAddresses[0] && (
                      <span className="block truncate text-xs text-muted-foreground">
                        {contact.emailAddresses[0]}
                      </span>
                    )}
                  </span>
                </button>
              ))}
            </div>
          )}
        </div>

        <div className="min-w-0 flex-1">
          {selectedContact ? (
            <div className="space-y-6 rounded-lg border p-6">
              <div className="flex items-center gap-4">
                <ContactAvatar
                  email={selectedContact.emailAddresses[0]}
                  name={selectedContact.displayName}
                  className="h-14 w-14 text-xl"
                />
                <h2 className="text-lg font-semibold">
                  {selectedContact.displayName || t(locale, 'contacts.noName')}
                </h2>
              </div>

              {selectedContact.emailAddresses.length > 0 && (
                <div className="space-y-1">
                  <h3 className="text-sm font-medium text-muted-foreground">
                    {t(locale, 'contacts.email')}
                  </h3>
                  {selectedContact.emailAddresses.map((email, i) => (
                    <div key={i} className="text-sm">
                      <a href={`mailto:${email}`} className="text-primary hover:underline">
                        {email}
                      </a>
                    </div>
                  ))}
                </div>
              )}

              {selectedContact.phoneNumbers.length > 0 && (
                <div className="space-y-1">
                  <h3 className="text-sm font-medium text-muted-foreground">
                    {t(locale, 'contacts.phone')}
                  </h3>
                  {selectedContact.phoneNumbers.map((phone, i) => (
                    <div key={i} className="text-sm">
                      <a href={`tel:${phone}`} className="text-primary hover:underline">
                        {phone}
                      </a>
                    </div>
                  ))}
                </div>
              )}

              {selectedContact.organisation && (
                <div className="space-y-1">
                  <h3 className="text-sm font-medium text-muted-foreground">
                    {t(locale, 'contacts.organisation')}
                  </h3>
                  <p className="text-sm">{selectedContact.organisation}</p>
                </div>
              )}
            </div>
          ) : (
            <EmptyState icon={UserRound} title={t(locale, 'contacts.selectContact')} />
          )}
        </div>
      </div>
    </SecondaryPage>
  );
}
