/**
 * Settings page with account management.
 *
 * Provides CRUD operations for mail accounts with i18n support.
 */

import { useState, useEffect } from 'react';
import { t } from '../i18n';
import { useUIStore } from '../stores/ui';
import { useAuthStore } from '../stores/auth';

interface MailAccount {
  id: string;
  displayName: string;
  emailAddress: string;
  protocol: string;
  imapHost?: string;
  imapPort?: number;
  imapSecurity?: string;
  smtpHost?: string;
  smtpPort?: number;
  smtpSecurity?: string;
  isActive: boolean;
  syncEnabled: boolean;
  lastSyncAt?: string;
  createdAt: string;
  updatedAt: string;
}

interface ProbeResult {
  found: boolean;
  source?: string;
  protocol: string;
  imapHost?: string;
  imapPort?: number;
  imapSecurity?: string;
  smtpHost?: string;
  smtpPort?: number;
  smtpSecurity?: string;
}

export function SettingsPage() {
  const locale = useUIStore((s) => s.locale);
  const token = useAuthStore((s) => s.token);
  const [accounts, setAccounts] = useState<MailAccount[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [showAddForm, setShowAddForm] = useState(false);
  const [editingAccount, setEditingAccount] = useState<MailAccount | null>(null);

  // Form state
  const [formData, setFormData] = useState({
    displayName: '',
    emailAddress: '',
    password: '',
    protocol: 'imap',
    imapHost: '',
    imapPort: 993,
    imapSecurity: 'tls',
    smtpHost: '',
    smtpPort: 587,
    smtpSecurity: 'starttls',
  });

  const [probing, setProbing] = useState(false);
  const [probeResult, setProbeResult] = useState<ProbeResult | null>(null);

  useEffect(() => {
    fetchAccounts();
  }, []);

  async function fetchAccounts() {
    try {
      setLoading(true);
      const res = await fetch('/api/accounts', {
        headers: { Authorization: `Bearer ${token}` },
      });
      if (!res.ok) throw new Error('Failed to fetch accounts');
      const data = await res.json();
      setAccounts(data);
    } catch (err: any) {
      setError(err.message);
    } finally {
      setLoading(false);
    }
  }

  async function handleProbe() {
    if (!formData.emailAddress) return;
    try {
      setProbing(true);
      setProbeResult(null);
      const res = await fetch('/api/accounts/probe', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          Authorization: `Bearer ${token}`,
        },
        body: JSON.stringify({ emailAddress: formData.emailAddress }),
      });
      if (!res.ok) throw new Error('Probe failed');
      const data = await res.json();
      setProbeResult(data);
      if (data.found) {
        setFormData((prev) => ({
          ...prev,
          protocol: data.protocol,
          imapHost: data.imapHost || prev.imapHost,
          imapPort: data.imapPort || prev.imapPort,
          imapSecurity: data.imapSecurity || prev.imapSecurity,
          smtpHost: data.smtpHost || prev.smtpHost,
          smtpPort: data.smtpPort || prev.smtpPort,
          smtpSecurity: data.smtpSecurity || prev.smtpSecurity,
        }));
      }
    } catch (err: any) {
      setError(err.message);
    } finally {
      setProbing(false);
    }
  }

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    try {
      const url = editingAccount ? `/api/accounts/${editingAccount.id}` : '/api/accounts';
      const method = editingAccount ? 'PUT' : 'POST';
      const body: any = {
        displayName: formData.displayName,
        emailAddress: formData.emailAddress,
        protocol: formData.protocol,
        imapHost: formData.imapHost || null,
        imapPort: formData.imapPort || null,
        imapSecurity: formData.imapSecurity,
        smtpHost: formData.smtpHost || null,
        smtpPort: formData.smtpPort || null,
        smtpSecurity: formData.smtpSecurity,
      };
      if (formData.password) {
        body.password = formData.password;
      }
      const res = await fetch(url, {
        method,
        headers: {
          'Content-Type': 'application/json',
          Authorization: `Bearer ${token}`,
        },
        body: JSON.stringify(body),
      });
      if (!res.ok) throw new Error('Failed to save account');
      setShowAddForm(false);
      setEditingAccount(null);
      resetForm();
      fetchAccounts();
    } catch (err: any) {
      setError(err.message);
    }
  }

  async function handleDelete(id: string) {
    if (!confirm(t(locale, 'settings.accounts.confirmDelete'))) return;
    try {
      const res = await fetch(`/api/accounts/${id}`, {
        method: 'DELETE',
        headers: { Authorization: `Bearer ${token}` },
      });
      if (!res.ok) throw new Error('Failed to delete account');
      fetchAccounts();
    } catch (err: any) {
      setError(err.message);
    }
  }

  function handleEdit(account: MailAccount) {
    setEditingAccount(account);
    setFormData({
      displayName: account.displayName,
      emailAddress: account.emailAddress,
      password: '',
      protocol: account.protocol,
      imapHost: account.imapHost || '',
      imapPort: account.imapPort || 993,
      imapSecurity: account.imapSecurity || 'tls',
      smtpHost: account.smtpHost || '',
      smtpPort: account.smtpPort || 587,
      smtpSecurity: account.smtpSecurity || 'starttls',
    });
    setShowAddForm(true);
  }

  function resetForm() {
    setFormData({
      displayName: '',
      emailAddress: '',
      password: '',
      protocol: 'imap',
      imapHost: '',
      imapPort: 993,
      imapSecurity: 'tls',
      smtpHost: '',
      smtpPort: 587,
      smtpSecurity: 'starttls',
    });
    setProbeResult(null);
  }

  return (
    <div className="settings-page">
      <h1>{t(locale, 'settings.title')}</h1>

      <section className="settings-section">
        <div className="section-header">
          <h2>{t(locale, 'settings.accounts.title')}</h2>
          <button
            onClick={() => {
              resetForm();
              setEditingAccount(null);
              setShowAddForm(true);
            }}
          >
            {t(locale, 'settings.accounts.add')}
          </button>
        </div>

        {error && <div className="error-message">{error}</div>}

        {loading ? (
          <div>{t(locale, 'common.loading')}</div>
        ) : accounts.length === 0 ? (
          <div className="empty-state">
            <p>{t(locale, 'settings.accounts.empty')}</p>
          </div>
        ) : (
          <div className="accounts-list">
            {accounts.map((account) => (
              <div key={account.id} className="account-card">
                <div className="account-info">
                  <h3>{account.displayName}</h3>
                  <p>{account.emailAddress}</p>
                  <p className="account-protocol">
                    {account.protocol.toUpperCase()}
                    {account.imapHost && ` • ${account.imapHost}`}
                  </p>
                </div>
                <div className="account-actions">
                  <button onClick={() => handleEdit(account)}>{t(locale, 'common.edit')}</button>
                  <button onClick={() => handleDelete(account.id)} className="danger">
                    {t(locale, 'common.delete')}
                  </button>
                </div>
              </div>
            ))}
          </div>
        )}
      </section>

      {showAddForm && (
        <div className="modal-overlay">
          <div className="modal">
            <h2>
              {editingAccount
                ? t(locale, 'settings.accounts.edit')
                : t(locale, 'settings.accounts.add')}
            </h2>
            <form onSubmit={handleSubmit}>
              <div className="form-group">
                <label>{t(locale, 'settings.accounts.displayName')}</label>
                <input
                  type="text"
                  value={formData.displayName}
                  onChange={(e) =>
                    setFormData((prev) => ({
                      ...prev,
                      displayName: e.target.value,
                    }))
                  }
                  required
                />
              </div>

              <div className="form-group">
                <label>{t(locale, 'settings.accounts.email')}</label>
                <input
                  type="email"
                  value={formData.emailAddress}
                  onChange={(e) =>
                    setFormData((prev) => ({
                      ...prev,
                      emailAddress: e.target.value,
                    }))
                  }
                  required
                />
                <button type="button" onClick={handleProbe} disabled={probing}>
                  {probing
                    ? t(locale, 'settings.accounts.probing')
                    : t(locale, 'settings.accounts.autoDetect')}
                </button>
              </div>

              {probeResult?.found && (
                <div className="probe-result">
                  <p>
                    {t(locale, 'settings.accounts.probeFound', {
                      source: probeResult.source || 'unknown',
                    })}
                  </p>
                </div>
              )}

              <div className="form-group">
                <label>{t(locale, 'settings.accounts.password')}</label>
                <input
                  type="password"
                  value={formData.password}
                  onChange={(e) =>
                    setFormData((prev) => ({
                      ...prev,
                      password: e.target.value,
                    }))
                  }
                  required={!editingAccount}
                />
              </div>

              <fieldset>
                <legend>{t(locale, 'settings.accounts.imapSettings')}</legend>
                <div className="form-group">
                  <label>{t(locale, 'settings.accounts.host')}</label>
                  <input
                    type="text"
                    value={formData.imapHost}
                    onChange={(e) =>
                      setFormData((prev) => ({
                        ...prev,
                        imapHost: e.target.value,
                      }))
                    }
                  />
                </div>
                <div className="form-group">
                  <label>{t(locale, 'settings.accounts.port')}</label>
                  <input
                    type="number"
                    value={formData.imapPort}
                    onChange={(e) =>
                      setFormData((prev) => ({
                        ...prev,
                        imapPort: parseInt(e.target.value),
                      }))
                    }
                  />
                </div>
                <div className="form-group">
                  <label>{t(locale, 'settings.accounts.security')}</label>
                  <select
                    value={formData.imapSecurity}
                    onChange={(e) =>
                      setFormData((prev) => ({
                        ...prev,
                        imapSecurity: e.target.value,
                      }))
                    }
                  >
                    <option value="tls">TLS</option>
                    <option value="starttls">STARTTLS</option>
                    <option value="none">{t(locale, 'settings.accounts.none')}</option>
                  </select>
                </div>
              </fieldset>

              <fieldset>
                <legend>{t(locale, 'settings.accounts.smtpSettings')}</legend>
                <div className="form-group">
                  <label>{t(locale, 'settings.accounts.host')}</label>
                  <input
                    type="text"
                    value={formData.smtpHost}
                    onChange={(e) =>
                      setFormData((prev) => ({
                        ...prev,
                        smtpHost: e.target.value,
                      }))
                    }
                  />
                </div>
                <div className="form-group">
                  <label>{t(locale, 'settings.accounts.port')}</label>
                  <input
                    type="number"
                    value={formData.smtpPort}
                    onChange={(e) =>
                      setFormData((prev) => ({
                        ...prev,
                        smtpPort: parseInt(e.target.value),
                      }))
                    }
                  />
                </div>
                <div className="form-group">
                  <label>{t(locale, 'settings.accounts.security')}</label>
                  <select
                    value={formData.smtpSecurity}
                    onChange={(e) =>
                      setFormData((prev) => ({
                        ...prev,
                        smtpSecurity: e.target.value,
                      }))
                    }
                  >
                    <option value="tls">TLS</option>
                    <option value="starttls">STARTTLS</option>
                    <option value="none">{t(locale, 'settings.accounts.none')}</option>
                  </select>
                </div>
              </fieldset>

              <div className="form-actions">
                <button type="submit">
                  {editingAccount ? t(locale, 'common.save') : t(locale, 'common.add')}
                </button>
                <button
                  type="button"
                  onClick={() => {
                    setShowAddForm(false);
                    setEditingAccount(null);
                  }}
                >
                  {t(locale, 'common.cancel')}
                </button>
              </div>
            </form>
          </div>
        </div>
      )}
    </div>
  );
}
