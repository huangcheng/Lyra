/**
 * Settings page with account management.
 *
 * Provides CRUD operations for mail accounts with i18n support.
 */

import { useState, useEffect } from 'react';
import { useNavigate } from '@tanstack/react-router';
import { t } from '../i18n';
import { SecondaryPage } from './secondary-page';
import { TotpEnroll } from './totp-enroll';
import { useUIStore } from '../stores/ui';
import { useAuthStore } from '../stores/auth';
import { api, type AuthMeResponse } from '@/lib/api-client';
import { syncEvents$ } from '@/rxjs/sync-events';
import { Button } from '@/components/ui/button';

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
  const setLocale = useUIStore((s) => s.setLocale);
  const token = useAuthStore((s) => s.token);
  const clearSession = useAuthStore((s) => s.clearSession);
  const navigate = useNavigate();
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
  const [syncing, setSyncing] = useState(false);
  const [syncMessage, setSyncMessage] = useState<string | null>(null);

  // Security section state
  const [totpEnabled, setTotpEnabled] = useState(false);
  const [currentPassword, setCurrentPassword] = useState('');
  const [newPassword, setNewPassword] = useState('');
  const [securityMessage, setSecurityMessage] = useState<string | null>(null);
  const [securityError, setSecurityError] = useState<string | null>(null);
  const [totpPassword, setTotpPassword] = useState('');
  const [changingPassword, setChangingPassword] = useState(false);
  const [disablingTotp, setDisablingTotp] = useState(false);
  const [enrollingTotp, setEnrollingTotp] = useState(false);

  useEffect(() => {
    void fetchAccounts();
    void api<AuthMeResponse>('/auth/me')
      .then((me) => {
        setTotpEnabled(Boolean(me.totp_enabled));
      })
      .catch(() => {});
  }, []);

  useEffect(() => {
    const sub = syncEvents$.subscribe((ev) => {
      if (ev.type === 'sync_complete') void fetchAccounts();
    });
    return () => sub.unsubscribe();
  }, []);

  async function fetchAccounts() {
    try {
      setLoading(true);
      const data = await api<MailAccount[]>('/accounts');
      setAccounts(data);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }

  async function pollUntilSyncIdle() {
    const deadline = Date.now() + 90_000;
    for (;;) {
      if (Date.now() >= deadline) {
        throw new Error(t(locale, 'sync.syncError'));
      }
      await new Promise((r) => setTimeout(r, 2000));
      const status = await api<{ syncing: boolean }>('/sync/status');
      if (!status.syncing) return;
    }
  }

  async function handleSync(id: string) {
    try {
      setError(null);
      setSyncing(true);
      setSyncMessage(null);
      await api(`/accounts/${id}/sync`, { method: 'POST' });
      setSyncMessage(t(locale, 'settings.syncQueued'));
      await pollUntilSyncIdle();
      await fetchAccounts();
      setSyncMessage(t(locale, 'sync.syncComplete'));
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : String(err));
      setSyncMessage(null);
    } finally {
      setSyncing(false);
    }
  }

  function formatLastSync(iso?: string) {
    if (!iso) return null;
    try {
      return new Date(iso).toLocaleString(locale === 'zh' ? 'zh-CN' : 'en-US');
    } catch {
      return iso;
    }
  }

  async function handleProbe() {
    if (!formData.emailAddress) return;
    try {
      setProbing(true);
      setProbeResult(null);
      const data = await api<ProbeResult>('/accounts/probe', {
        method: 'POST',
        body: JSON.stringify({ emailAddress: formData.emailAddress }),
      });
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
      const url = editingAccount ? `/accounts/${editingAccount.id}` : '/accounts';
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
      await api(url, {
        method,
        body: JSON.stringify(body),
      });
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
      await api(`/accounts/${id}`, { method: 'DELETE' });
      fetchAccounts();
    } catch (err: any) {
      setError(err.message);
    }
  }

  async function handleChangePassword(e: React.FormEvent) {
    e.preventDefault();
    try {
      setChangingPassword(true);
      setSecurityError(null);
      setSecurityMessage(null);
      await api('/auth/change-password', {
        method: 'POST',
        body: JSON.stringify({
          current_password: currentPassword,
          new_password: newPassword,
        }),
      });
      // The backend invalidates every session on password change; log out
      // locally and send the user back to the login page.
      localStorage.removeItem('lyra_token');
      clearSession();
      void navigate({ to: '/login' });
    } catch (err: any) {
      setSecurityError(err.message);
      setChangingPassword(false);
    }
  }

  async function handleDisableTotp(e: React.FormEvent) {
    e.preventDefault();
    try {
      setDisablingTotp(true);
      setSecurityError(null);
      setSecurityMessage(null);
      await api('/auth/totp/disable', {
        method: 'POST',
        body: JSON.stringify({ password: totpPassword }),
      });
      setTotpEnabled(false);
      setTotpPassword('');
      setSecurityMessage(t(locale, 'settings.security.disableTotpSuccess'));
    } catch (err: any) {
      setSecurityError(err.message);
    } finally {
      setDisablingTotp(false);
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
      // Legacy 'none' values (removed insecure mode) coerce to 'tls' so the
      // select never shows a blank value and saving doesn't 400.
      imapSecurity:
        !account.imapSecurity || account.imapSecurity === 'none' ? 'tls' : account.imapSecurity,
      smtpHost: account.smtpHost || '',
      smtpPort: account.smtpPort || 587,
      smtpSecurity:
        !account.smtpSecurity || account.smtpSecurity === 'none'
          ? 'starttls'
          : account.smtpSecurity,
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
    <SecondaryPage title={t(locale, 'settings.title')}>
      <div className="mx-auto max-w-2xl space-y-6">
        <section className="space-y-4 rounded-lg border p-4">
          <h2 className="text-base font-semibold">{t(locale, 'settings.session')}</h2>
          <div className="flex flex-wrap items-center gap-2">
            <Button
              variant={locale === 'en' ? 'secondary' : 'outline'}
              size="sm"
              onClick={() => setLocale('en')}
            >
              {t(locale, 'settings.english')}
            </Button>
            <Button
              variant={locale === 'zh' ? 'secondary' : 'outline'}
              size="sm"
              onClick={() => setLocale('zh')}
            >
              {t(locale, 'settings.chinese')}
            </Button>
            <Button
              variant="outline"
              size="sm"
              className="ml-auto"
              onClick={() => {
                if (token) {
                  void api('/auth/logout', { method: 'POST' }).catch(() => {});
                }
                localStorage.removeItem('lyra_token');
                clearSession();
                void navigate({ to: '/login' });
              }}
            >
              {t(locale, 'auth.logout')}
            </Button>
          </div>
        </section>

        <section className="space-y-4 rounded-lg border p-4">
          <h2 className="text-base font-semibold">{t(locale, 'settings.security.title')}</h2>
          {securityError && <div className="error-message">{securityError}</div>}
          {securityMessage && (
            <div className="text-sm text-muted-foreground" role="status">
              {securityMessage}
            </div>
          )}
          <form onSubmit={handleChangePassword} className="space-y-3">
            <h3 className="text-sm font-medium">
              {t(locale, 'settings.security.changePasswordTitle')}
            </h3>
            <div className="space-y-2">
              <label className="text-sm" htmlFor="settings-current-password">
                {t(locale, 'settings.security.currentPassword')}
              </label>
              <input
                id="settings-current-password"
                type="password"
                className="h-9 w-full rounded-md border border-input bg-transparent px-3 text-sm"
                value={currentPassword}
                onChange={(e) => setCurrentPassword(e.target.value)}
                autoComplete="current-password"
                required
              />
            </div>
            <div className="space-y-2">
              <label className="text-sm" htmlFor="settings-new-password">
                {t(locale, 'settings.security.newPassword')}
              </label>
              <input
                id="settings-new-password"
                type="password"
                className="h-9 w-full rounded-md border border-input bg-transparent px-3 text-sm"
                value={newPassword}
                onChange={(e) => setNewPassword(e.target.value)}
                autoComplete="new-password"
                required
              />
            </div>
            <Button type="submit" variant="outline" size="sm" disabled={changingPassword}>
              {changingPassword
                ? t(locale, 'common.loading')
                : t(locale, 'settings.security.changePassword')}
            </Button>
          </form>
          {totpEnabled && (
            <form onSubmit={handleDisableTotp} className="space-y-3 border-t pt-4">
              <h3 className="text-sm font-medium">
                {t(locale, 'settings.security.disableTotpTitle')}
              </h3>
              <p className="text-sm text-muted-foreground">
                {t(locale, 'settings.security.disableTotpDescription')}
              </p>
              <div className="space-y-2">
                <label className="text-sm" htmlFor="settings-totp-password">
                  {t(locale, 'auth.password')}
                </label>
                <input
                  id="settings-totp-password"
                  type="password"
                  className="h-9 w-full rounded-md border border-input bg-transparent px-3 text-sm"
                  value={totpPassword}
                  onChange={(e) => setTotpPassword(e.target.value)}
                  autoComplete="current-password"
                  required
                />
              </div>
              <Button type="submit" variant="outline" size="sm" disabled={disablingTotp}>
                {disablingTotp
                  ? t(locale, 'common.loading')
                  : t(locale, 'settings.security.disableTotp')}
              </Button>
            </form>
          )}
          {!totpEnabled && (
            <div className="space-y-3 border-t pt-4">
              {enrollingTotp ? (
                <TotpEnroll
                  onComplete={() => {
                    setEnrollingTotp(false);
                    setTotpEnabled(true);
                    const user = useAuthStore.getState().user;
                    if (user) {
                      useAuthStore.getState().setUser({ ...user, totpEnabled: true });
                    }
                    setSecurityMessage(t(locale, 'auth.totpEnabled'));
                  }}
                  onCancel={() => setEnrollingTotp(false)}
                />
              ) : (
                <>
                  <h3 className="text-sm font-medium">
                    {t(locale, 'settings.security.enableTotpTitle')}
                  </h3>
                  <p className="text-sm text-muted-foreground">
                    {t(locale, 'settings.security.enableTotpDescription')}
                  </p>
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    onClick={() => {
                      setSecurityError(null);
                      setSecurityMessage(null);
                      setEnrollingTotp(true);
                    }}
                  >
                    {t(locale, 'settings.security.enableTotp')}
                  </Button>
                </>
              )}
            </div>
          )}
        </section>

        <section className="space-y-4">
          <div className="flex items-center justify-between">
            <h2 className="text-base font-semibold">{t(locale, 'settings.accounts.title')}</h2>
            <button
              type="button"
              className="rounded-md border bg-background px-3 py-1.5 text-sm hover:bg-accent"
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
          {syncMessage && (
            <div className="text-sm text-muted-foreground" role="status">
              {syncing ? t(locale, 'sync.syncing') : syncMessage}
            </div>
          )}

          {loading ? (
            <div>{t(locale, 'common.loading')}</div>
          ) : accounts.length === 0 ? (
            <div className="empty-state">
              <p>{t(locale, 'settings.accounts.empty')}</p>
            </div>
          ) : (
            <div className="space-y-3">
              {accounts.map((account) => (
                <div
                  key={account.id}
                  className="flex items-center justify-between rounded-lg border p-4"
                >
                  <div>
                    <h3 className="font-medium">{account.displayName}</h3>
                    <p className="text-sm text-muted-foreground">{account.emailAddress}</p>
                    <p className="text-xs text-muted-foreground">
                      {account.protocol.toUpperCase()}
                      {account.imapHost && ` • ${account.imapHost}`}
                    </p>
                    {account.lastSyncAt && (
                      <p className="text-xs text-muted-foreground">
                        {t(locale, 'sync.lastSync')}: {formatLastSync(account.lastSyncAt)}
                      </p>
                    )}
                  </div>
                  <div className="flex gap-2">
                    <button
                      type="button"
                      className="rounded-md border px-3 py-1.5 text-sm hover:bg-accent disabled:opacity-50"
                      disabled={syncing}
                      onClick={() => void handleSync(account.id)}
                    >
                      {syncing ? t(locale, 'sync.syncing') : t(locale, 'settings.syncNow')}
                    </button>
                    <button
                      type="button"
                      className="rounded-md border px-3 py-1.5 text-sm hover:bg-accent"
                      onClick={() => handleEdit(account)}
                    >
                      {t(locale, 'common.edit')}
                    </button>
                    <button
                      type="button"
                      className="rounded-md border px-3 py-1.5 text-sm text-destructive hover:bg-accent"
                      onClick={() => handleDelete(account.id)}
                    >
                      {t(locale, 'common.delete')}
                    </button>
                  </div>
                </div>
              ))}
            </div>
          )}
        </section>

        {showAddForm && (
          <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4">
            <div className="max-h-[90vh] w-full max-w-lg overflow-auto rounded-lg border bg-background p-6 shadow-lg">
              <h2>
                {editingAccount
                  ? t(locale, 'settings.accounts.edit')
                  : t(locale, 'settings.accounts.add')}
              </h2>
              <form
                onSubmit={handleSubmit}
                className="space-y-4 [&_input]:h-9 [&_input]:w-full [&_input]:rounded-md [&_input]:border [&_input]:border-input [&_input]:bg-transparent [&_input]:px-3 [&_input]:text-sm [&_select]:h-9 [&_select]:w-full [&_select]:rounded-md [&_select]:border [&_select]:bg-background [&_select]:px-3 [&_label]:text-sm [&_label]:font-medium [&_fieldset]:space-y-3 [&_fieldset]:rounded-md [&_fieldset]:border [&_fieldset]:p-3 [&_legend]:px-1 [&_legend]:text-sm [&_legend]:font-medium [&_button]:rounded-md [&_button]:border [&_button]:px-3 [&_button]:py-1.5 [&_button]:text-sm [&_button]:hover:bg-accent"
              >
                <div className="space-y-2">
                  <label className="text-sm font-medium" htmlFor="settings-display-name">
                    {t(locale, 'settings.accounts.displayName')}
                  </label>
                  <input
                    id="settings-display-name"
                    type="text"
                    className="h-9 w-full rounded-md border border-input bg-transparent px-3 text-sm"
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
    </SecondaryPage>
  );
}
