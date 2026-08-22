/**
 * Contacts page with CardDAV sync support.
 *
 * Displays contacts from all configured accounts.
 */

import { useState, useEffect } from 'react';
import { t } from '../i18n';
import { SecondaryPage } from './secondary-page';
import { useUIStore } from '../stores/ui';
import { useAuthStore } from '../stores/auth';

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

export function ContactsPage() {
  const locale = useUIStore((s) => s.locale);
  const token = useAuthStore((s) => s.token);
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
      const res = await fetch(`/api/v1/contacts?${params}`, {
        headers: { Authorization: `Bearer ${token}` },
      });
      if (!res.ok) throw new Error('Failed to fetch contacts');
      const data = await res.json();
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

  function getInitials(name?: string): string {
    if (!name) return '?';
    const parts = name.split(/\s+/);
    if (parts.length === 1) return parts[0].charAt(0).toUpperCase();
    return (parts[0].charAt(0) + parts[parts.length - 1].charAt(0)).toUpperCase();
  }

  return (
    <SecondaryPage title={t(locale, 'contacts.title')}>
      <div className="mx-auto flex max-w-4xl gap-6">
        <div className="w-72 shrink-0 space-y-3">
          <div className="contacts-header">
            <h1>{t(locale, 'contacts.title')}</h1>
            <form onSubmit={handleSearch} className="flex gap-2">
              <input
                type="text"
                className="h-9 flex-1 rounded-md border border-input bg-transparent px-3 text-sm"
                placeholder={t(locale, 'contacts.search')}
                value={searchQuery}
                onChange={(e) => setSearchQuery(e.target.value)}
              />
              <button
                type="submit"
                className="rounded-md border px-3 py-1.5 text-sm hover:bg-accent"
              >
                {t(locale, 'common.search')}
              </button>
            </form>
          </div>

          {loading ? (
            <div className="loading">{t(locale, 'common.loading')}</div>
          ) : error ? (
            <div className="error">{error}</div>
          ) : contacts.length === 0 ? (
            <div className="empty-state">
              <p>{t(locale, 'contacts.empty')}</p>
            </div>
          ) : (
            <div className="contacts-list">
              {contacts.map((contact) => (
                <div
                  key={contact.id}
                  className={`contact-item ${selectedContact?.id === contact.id ? 'selected' : ''}`}
                  onClick={() => setSelectedContact(contact)}
                >
                  <div className="contact-avatar">{getInitials(contact.displayName)}</div>
                  <div className="contact-info">
                    <div className="contact-name">
                      {contact.displayName || t(locale, 'contacts.noName')}
                    </div>
                    {contact.emailAddresses[0] && (
                      <div className="contact-email">{contact.emailAddresses[0]}</div>
                    )}
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>

        <div className="contacts-detail">
          {selectedContact ? (
            <div className="contact-detail-card">
              <div className="contact-avatar large">{getInitials(selectedContact.displayName)}</div>
              <h2>{selectedContact.displayName || t(locale, 'contacts.noName')}</h2>

              {selectedContact.emailAddresses.length > 0 && (
                <div className="detail-section">
                  <h3>{t(locale, 'contacts.email')}</h3>
                  {selectedContact.emailAddresses.map((email, i) => (
                    <div key={i} className="detail-item">
                      <a href={`mailto:${email}`}>{email}</a>
                    </div>
                  ))}
                </div>
              )}

              {selectedContact.phoneNumbers.length > 0 && (
                <div className="detail-section">
                  <h3>{t(locale, 'contacts.phone')}</h3>
                  {selectedContact.phoneNumbers.map((phone, i) => (
                    <div key={i} className="detail-item">
                      <a href={`tel:${phone}`}>{phone}</a>
                    </div>
                  ))}
                </div>
              )}

              {selectedContact.organisation && (
                <div className="detail-section">
                  <h3>{t(locale, 'contacts.organisation')}</h3>
                  <p>{selectedContact.organisation}</p>
                </div>
              )}
            </div>
          ) : (
            <div className="no-selection">
              <p>{t(locale, 'contacts.selectContact')}</p>
            </div>
          )}
        </div>
      </div>
    </SecondaryPage>
  );
}
