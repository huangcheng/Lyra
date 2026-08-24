/**
 * Settings page with account management.
 *
 * Standalone slim-nav shell; sections: General, Accounts, Spam & Filters,
 * Privacy. Provides CRUD operations for mail accounts with i18n support.
 */

import { useState, useEffect } from 'react';
import { useNavigate } from '@tanstack/react-router';
import { Flag, Plus, Shield, SlidersHorizontal, Users, X } from 'lucide-react';
import { t } from '../i18n';
import { SlimPageNav, type SlimNavItem } from '@/components/slim-page-nav';
import { TotpEnroll } from './totp-enroll';
import { useUIStore } from '../stores/ui';
import { useAuthStore } from '../stores/auth';
import { api, type AuthMeResponse } from '@/lib/api-client';
import { MARK_READ_POLICIES } from '@/lib/mark-read-policy';
import type { ThemeMode } from '@/lib/theme';
import { saveLocale, saveMarkReadPolicy, applyMarkReadPolicy } from '@/lib/user-preferences';
import {
  fetchPrivacySettings,
  updatePrivacySettings,
  type PrivacySettings,
} from '@/lib/privacy-api';
import { syncEvents$ } from '@/rxjs/sync-events';
import { Button } from '@/components/ui/button';
import { Badge } from '@/components/ui/badge';
import { Switch } from '@/components/ui/switch';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select';
import type { MarkReadPolicy } from '@/types';

const REMOTE_IMAGE_MODES = ['block', 'proxy'] as const;
type RemoteImageMode = (typeof REMOTE_IMAGE_MODES)[number];

type SettingsSection = 'general' | 'accounts' | 'spam' | 'privacy';

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

const SPAM_SENSITIVITY = ['lenient', 'standard', 'strict'] as const;
const EXAMPLE_BLOCKED_SENDERS = ['newsletter@example.com', 'promo@example.net'];

export function SettingsPage() {
  const locale = useUIStore((s) => s.locale);
  const setLocale = useUIStore((s) => s.setLocale);
  const theme = useUIStore((s) => s.theme);
  const setTheme = useUIStore((s) => s.setTheme);
  const markReadPolicy = useUIStore((s) => s.markReadPolicy);
  const token = useAuthStore((s) => s.token);
  const clearSession = useAuthStore((s) => s.clearSession);
  const navigate = useNavigate();
  const [section, setSection] = useState<SettingsSection>('general');
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
  const [syncingId, setSyncingId] = useState<string | null>(null);
  const [syncErrors, setSyncErrors] = useState<Record<string, string>>({});
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
  const [prefsError, setPrefsError] = useState<string | null>(null);
  const [prefsSaving, setPrefsSaving] = useState(false);
  const [privacySettings, setPrivacySettings] = useState<PrivacySettings | null>(null);
  const [privacySaving, setPrivacySaving] = useState(false);
  const [privacyError, setPrivacyError] = useState<string | null>(null);

  useEffect(() => {
    void fetchAccounts();
    void fetchPrivacySettings()
      .then(setPrivacySettings)
      .catch(() => {});
    void api<AuthMeResponse>('/auth/me')
      .then((me) => {
        setTotpEnabled(Boolean(me.totp_enabled));
        applyMarkReadPolicy(me.mark_read_policy);
      })
      .catch(() => {});
  }, []);

  useEffect(() => {
    const sub = syncEvents$.subscribe((ev) => {
      if (ev.type === 'sync_started') {
        setSyncingId(ev.accountId);
        setSyncErrors((prev) => {
          const next = { ...prev };
          delete next[ev.accountId];
          return next;
        });
      }
      if (ev.type === 'sync_complete') {
        setSyncingId((id) => (id === ev.accountId ? null : id));
        void fetchAccounts();
        setSyncMessage(t(locale, 'sync.syncComplete'));
      }
      if (ev.type === 'sync_error') {
        setSyncingId((id) => (id === ev.accountId ? null : id));
        setSyncErrors((prev) => ({ ...prev, [ev.accountId]: ev.error }));
      }
    });
    return () => sub.unsubscribe();
  }, [locale]);

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
      setSyncingId(id);
      setSyncMessage(null);
      setSyncErrors((prev) => {
        const next = { ...prev };
        delete next[id];
        return next;
      });
      await api(`/accounts/${id}/sync`, { method: 'POST' });
      await pollUntilSyncIdle();
      await fetchAccounts();
      setSyncMessage(t(locale, 'sync.syncComplete'));
    } catch (err: unknown) {
      const message = err instanceof Error ? err.message : String(err);
      setSyncErrors((prev) => ({ ...prev, [id]: message }));
    } finally {
      setSyncingId(null);
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

  async function handleMarkReadPolicyChange(value: string) {
    if (!MARK_READ_POLICIES.includes(value as MarkReadPolicy)) return;
    setPrefsError(null);
    setPrefsSaving(true);
    try {
      await saveMarkReadPolicy(value as MarkReadPolicy);
    } catch (err: unknown) {
      setPrefsError(err instanceof Error ? err.message : String(err));
    } finally {
      setPrefsSaving(false);
    }
  }

  async function handleRemoteImagesModeChange(value: string) {
    if (!REMOTE_IMAGE_MODES.includes(value as RemoteImageMode)) return;
    setPrivacyError(null);
    setPrivacySaving(true);
    try {
      const updated = await updatePrivacySettings({
        remoteImages: value as RemoteImageMode,
      });
      setPrivacySettings(updated);
    } catch (err: unknown) {
      setPrivacyError(err instanceof Error ? err.message : String(err));
    } finally {
      setPrivacySaving(false);
    }
  }

  const remoteImagesMode = privacySettings?.remoteImages ?? 'block';

  const navItems: SlimNavItem[] = [
    {
      key: 'general',
      label: t(locale, 'settings.general'),
      icon: SlidersHorizontal,
      active: section === 'general',
      onClick: () => setSection('general'),
    },
    {
      key: 'accounts',
      label: t(locale, 'settings.accountsNav'),
      icon: Users,
      active: section === 'accounts',
      onClick: () => setSection('accounts'),
    },
    {
      key: 'spam',
      label: t(locale, 'settings.spam.title'),
      icon: Flag,
      active: section === 'spam',
      onClick: () => setSection('spam'),
    },
    {
      key: 'privacy',
      label: t(locale, 'settings.privacy.title'),
      icon: Shield,
      active: section === 'privacy',
      onClick: () => setSection('privacy'),
    },
  ];

  const sectionMeta: Record<SettingsSection, { title: string; subtitle: string }> = {
    general: {
      title: t(locale, 'settings.general'),
      subtitle: t(locale, 'settings.generalSubtitle'),
    },
    accounts: {
      title: t(locale, 'settings.accounts.title'),
      subtitle: t(locale, 'settings.accountsSubtitle'),
    },
    spam: {
      title: t(locale, 'settings.spam.title'),
      subtitle: t(locale, 'settings.spam.subtitle'),
    },
    privacy: {
      title: t(locale, 'settings.privacy.title'),
      subtitle: t(locale, 'settings.privacySubtitle'),
    },
  };

  const soonBadge = (
    <Badge variant="outline" className="text-[10.5px] font-normal text-ter-foreground">
      {t(locale, 'common.soon')}
    </Badge>
  );

  return (
    <div className="flex h-svh">
      <SlimPageNav section={t(locale, 'settings.section')} items={navItems} />
      <main className="flex-1 overflow-auto bg-background">
        <header className="border-b border-border px-8 pb-5 pt-7">
          <h1 className="font-display text-xl font-medium">{sectionMeta[section].title}</h1>
          <p className="text-[12.5px] text-ter-foreground">{sectionMeta[section].subtitle}</p>
        </header>

        <div className="max-w-2xl space-y-4 px-8 py-6">
          {section === 'general' && (
            <>
              <section className="space-y-3 rounded-[10px] border border-border bg-card px-5 py-4">
                <div className="flex flex-wrap items-center justify-between gap-3">
                  <div>
                    <div className="text-[13px] font-medium">{t(locale, 'settings.language')}</div>
                  </div>
                  <div className="flex items-center gap-2">
                    <Button
                      variant={locale === 'en' ? 'secondary' : 'outline'}
                      size="sm"
                      onClick={() => void saveLocale('en').catch(() => setLocale('en'))}
                    >
                      {t(locale, 'settings.english')}
                    </Button>
                    <Button
                      variant={locale === 'zh' ? 'secondary' : 'outline'}
                      size="sm"
                      onClick={() => void saveLocale('zh').catch(() => setLocale('zh'))}
                    >
                      {t(locale, 'settings.chinese')}
                    </Button>
                  </div>
                </div>
                <div className="flex flex-wrap items-center justify-between gap-3 border-t border-border pt-3">
                  <div className="text-[13px] font-medium">{t(locale, 'settings.theme')}</div>
                  <Select value={theme} onValueChange={(v) => setTheme(v as ThemeMode)}>
                    <SelectTrigger size="sm" className="min-w-[140px]">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {(['light', 'dark', 'system'] as const).map((mode) => (
                        <SelectItem key={mode} value={mode}>
                          {t(locale, `settings.themeMode.${mode}`)}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </div>
              </section>

              <section className="space-y-3 rounded-[10px] border border-border bg-card px-5 py-4">
                <div className="flex flex-wrap items-center justify-between gap-3">
                  <div>
                    <div className="text-[13px] font-medium">
                      {t(locale, 'settings.preferences.markRead')}
                    </div>
                    <div className="text-xs text-ter-foreground">
                      {t(locale, 'settings.preferences.readingStatus')}
                    </div>
                  </div>
                  <Select
                    value={markReadPolicy}
                    onValueChange={(value) => void handleMarkReadPolicyChange(value)}
                    disabled={prefsSaving}
                  >
                    <SelectTrigger size="sm" className="min-w-[200px]">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {MARK_READ_POLICIES.map((policy) => (
                        <SelectItem key={policy} value={policy}>
                          {t(locale, `settings.preferences.markReadPolicy.${policy}`)}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </div>
                {prefsError ? <div className="text-sm text-destructive">{prefsError}</div> : null}
              </section>

              <section className="space-y-4 rounded-[10px] border border-border bg-card px-5 py-4">
                <h2 className="text-[13px] font-medium">{t(locale, 'settings.security.title')}</h2>
                {securityError && <div className="text-sm text-destructive">{securityError}</div>}
                {securityMessage && (
                  <div className="text-sm text-muted-foreground" role="status">
                    {securityMessage}
                  </div>
                )}
                <form onSubmit={handleChangePassword} className="space-y-3">
                  <h3 className="text-[13px] font-medium">
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
                    <h3 className="text-[13px] font-medium">
                      {t(locale, 'settings.security.disableTotpTitle')}
                    </h3>
                    <p className="text-xs text-ter-foreground">
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
                        <h3 className="text-[13px] font-medium">
                          {t(locale, 'settings.security.enableTotpTitle')}
                        </h3>
                        <p className="text-xs text-ter-foreground">
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

              <section className="flex items-center justify-between rounded-[10px] border border-border bg-card px-5 py-4">
                <div className="text-[13px] font-medium">{t(locale, 'settings.session')}</div>
                <Button
                  variant="outline"
                  size="sm"
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
              </section>
            </>
          )}

          {section === 'accounts' && (
            <>
              {error && <div className="text-sm text-destructive">{error}</div>}
              {syncMessage && (
                <div className="text-sm text-muted-foreground" role="status">
                  {syncMessage}
                </div>
              )}

              {loading ? (
                <div className="text-sm text-muted-foreground">{t(locale, 'common.loading')}</div>
              ) : (
                <>
                  {accounts.map((account) => (
                    <div
                      key={account.id}
                      className="flex items-center gap-3.5 rounded-[10px] border border-border bg-card px-5 py-4"
                    >
                      <div className="flex size-9 shrink-0 items-center justify-center rounded-[9px] bg-muted">
                        <span className="font-brand text-[15px] text-foreground">
                          {(account.displayName || account.emailAddress).charAt(0).toUpperCase()}
                        </span>
                      </div>
                      <div className="min-w-0 flex-1">
                        <div className="truncate text-[13.5px] font-medium">
                          {account.emailAddress}
                        </div>
                        <div className="flex flex-wrap items-center gap-x-2 gap-y-0.5 text-[11.5px] text-muted-foreground">
                          <span className="size-1.5 rounded-full bg-ok" />
                          {account.lastSyncAt && (
                            <span>
                              {t(locale, 'sync.lastSync')}: {formatLastSync(account.lastSyncAt)}
                            </span>
                          )}
                          <span>{account.protocol.toUpperCase()}</span>
                          {account.displayName && account.displayName !== account.emailAddress && (
                            <span>{account.displayName}</span>
                          )}
                        </div>
                        {syncErrors[account.id] && (
                          <p className="text-xs text-destructive">
                            {t(locale, 'sync.syncFailed')}: {syncErrors[account.id]}
                          </p>
                        )}
                      </div>
                      <div className="flex shrink-0 items-center gap-2">
                        <Button
                          variant="ghost"
                          size="sm"
                          disabled={syncingId === account.id}
                          onClick={() => void handleSync(account.id)}
                        >
                          {syncingId === account.id
                            ? t(locale, 'sync.syncing')
                            : t(locale, 'settings.syncNow')}
                        </Button>
                        <Button
                          variant="outline"
                          size="sm"
                          className="rounded-full"
                          onClick={() => handleEdit(account)}
                        >
                          {t(locale, 'settings.manage')}
                        </Button>
                        <Button
                          variant="ghost"
                          size="sm"
                          className="text-destructive"
                          onClick={() => handleDelete(account.id)}
                        >
                          {t(locale, 'common.delete')}
                        </Button>
                      </div>
                    </div>
                  ))}

                  <button
                    type="button"
                    className="flex w-full flex-col items-center justify-center gap-1.5 rounded-[10px] border border-input bg-secondary px-5 py-6 text-[13px] font-medium text-foreground hover:bg-accent"
                    onClick={() => {
                      resetForm();
                      setEditingAccount(null);
                      setShowAddForm(true);
                    }}
                  >
                    <Plus size={18} className="text-ter-foreground" />
                    {t(locale, 'settings.accounts.add')}
                  </button>
                  {accounts.length === 0 && (
                    <p className="text-center text-xs text-ter-foreground">
                      {t(locale, 'settings.accounts.empty')}
                    </p>
                  )}
                </>
              )}
            </>
          )}

          {section === 'spam' && (
            <>
              <section className="space-y-3 rounded-[10px] border border-border bg-card px-5 py-4">
                <div className="flex items-center gap-2">
                  <h2 className="text-[13px] font-medium">
                    {t(locale, 'settings.spam.filtering')}
                  </h2>
                  {soonBadge}
                </div>
                {(
                  [
                    ['enable', 'enableDesc'],
                    ['learn', 'learnDesc'],
                    ['autoDelete', 'autoDeleteDesc'],
                  ] as const
                ).map(([key, descKey], i) => (
                  <div
                    key={key}
                    className={
                      i > 0
                        ? 'flex items-center justify-between gap-3 border-t border-border pt-3'
                        : 'flex items-center justify-between gap-3'
                    }
                  >
                    <div>
                      <div className="text-[13px] font-medium">
                        {t(locale, `settings.spam.${key}`)}
                      </div>
                      <div className="text-xs text-ter-foreground">
                        {t(locale, `settings.spam.${descKey}`)}
                      </div>
                    </div>
                    <Switch disabled />
                  </div>
                ))}
              </section>

              <section className="space-y-3 rounded-[10px] border border-border bg-card px-5 py-4">
                <div className="flex items-center gap-2">
                  <h2 className="text-[13px] font-medium">
                    {t(locale, 'settings.spam.sensitivity')}
                  </h2>
                  {soonBadge}
                </div>
                <div className="flex items-center justify-between gap-3">
                  <div className="text-xs text-ter-foreground">
                    {t(locale, 'settings.spam.sensitivityDesc')}
                  </div>
                  <div className="flex h-8 items-center rounded-lg bg-accent p-0.5 text-muted-foreground">
                    {SPAM_SENSITIVITY.map((level, i) => (
                      <button
                        key={level}
                        type="button"
                        disabled
                        className={
                          i === 1
                            ? 'h-7 rounded-md bg-card px-3 text-sm font-medium text-foreground'
                            : 'h-7 rounded-md px-3 text-sm font-medium'
                        }
                      >
                        {t(locale, `settings.spam.${level}`)}
                      </button>
                    ))}
                  </div>
                </div>
              </section>

              <section className="space-y-3 rounded-[10px] border border-border bg-card px-5 py-4">
                <div className="flex items-center gap-2">
                  <h2 className="text-[13px] font-medium">{t(locale, 'settings.spam.blocked')}</h2>
                  {soonBadge}
                </div>
                <p className="text-xs text-ter-foreground">
                  {t(locale, 'settings.spam.blockedDesc')}
                </p>
                {EXAMPLE_BLOCKED_SENDERS.map((sender) => (
                  <div
                    key={sender}
                    className="flex items-center justify-between gap-3 border-t border-border pt-3"
                  >
                    <span className="text-[13px] text-muted-foreground">{sender}</span>
                    <Button
                      variant="ghost"
                      size="sm"
                      disabled
                      aria-label={t(locale, 'common.delete')}
                    >
                      <X size={14} />
                    </Button>
                  </div>
                ))}
                <div className="flex items-center gap-2 border-t border-border pt-3">
                  <input
                    type="email"
                    disabled
                    placeholder={t(locale, 'settings.spam.addSender')}
                    className="h-9 w-full rounded-md border border-input bg-transparent px-3 text-sm"
                  />
                  <Button variant="outline" size="sm" disabled>
                    {t(locale, 'common.add')}
                  </Button>
                </div>
              </section>
            </>
          )}

          {section === 'privacy' && (
            <>
              <section className="space-y-3 rounded-[10px] border border-border bg-card px-5 py-4">
                <h2 className="text-[13px] font-medium">
                  {t(locale, 'settings.privacy.remoteImages')}
                </h2>
                <p className="text-xs text-ter-foreground">
                  {t(locale, 'settings.privacy.remoteImagesHint')}
                </p>
                <div className="flex flex-wrap items-center justify-between gap-3">
                  <span className="text-xs text-ter-foreground">
                    {t(locale, `settings.privacy.remoteImagesModeHelp.${remoteImagesMode}`)}
                  </span>
                  <Select
                    value={remoteImagesMode}
                    onValueChange={(value) => void handleRemoteImagesModeChange(value)}
                    disabled={privacySaving || !privacySettings}
                  >
                    <SelectTrigger size="sm" className="min-w-[200px]">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {REMOTE_IMAGE_MODES.map((mode) => (
                        <SelectItem key={mode} value={mode}>
                          {t(locale, `settings.privacy.remoteImagesMode.${mode}`)}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </div>
                {privacyError ? (
                  <div className="text-sm text-destructive">{privacyError}</div>
                ) : null}
              </section>

              <section className="space-y-3 rounded-[10px] border border-border bg-card px-5 py-4">
                <div className="flex items-center gap-2">
                  <h2 className="text-[13px] font-medium">
                    {t(locale, 'settings.privacy.trackingTitle')}
                  </h2>
                  {soonBadge}
                </div>
                {(
                  [
                    ['stripPixels', 'stripPixelsDesc'],
                    ['warnLinks', 'warnLinksDesc'],
                  ] as const
                ).map(([key, descKey], i) => (
                  <div
                    key={key}
                    className={
                      i > 0
                        ? 'flex items-center justify-between gap-3 border-t border-border pt-3'
                        : 'flex items-center justify-between gap-3'
                    }
                  >
                    <div>
                      <div className="text-[13px] font-medium">
                        {t(locale, `settings.privacy.${key}`)}
                      </div>
                      <div className="text-xs text-ter-foreground">
                        {t(locale, `settings.privacy.${descKey}`)}
                      </div>
                    </div>
                    <Switch disabled />
                  </div>
                ))}
              </section>

              <section className="space-y-3 rounded-[10px] border border-border bg-card px-5 py-4">
                <div className="flex items-center gap-2">
                  <h2 className="text-[13px] font-medium">
                    {t(locale, 'settings.privacy.dataTitle')}
                  </h2>
                  {soonBadge}
                </div>
                <div className="flex items-center justify-between gap-3">
                  <div>
                    <div className="text-[13px] font-medium">
                      {t(locale, 'settings.privacy.exportData')}
                    </div>
                    <div className="text-xs text-ter-foreground">
                      {t(locale, 'settings.privacy.exportDataDesc')}
                    </div>
                  </div>
                  <Button variant="outline" size="sm" disabled>
                    {t(locale, 'settings.privacy.exportData')}
                  </Button>
                </div>
                <div className="flex items-center justify-between gap-3 border-t border-border pt-3">
                  <div>
                    <div className="text-[13px] font-medium text-destructive">
                      {t(locale, 'settings.privacy.deleteData')}
                    </div>
                    <div className="text-xs text-ter-foreground">
                      {t(locale, 'settings.privacy.deleteDataDesc')}
                    </div>
                  </div>
                  <Button variant="outline" size="sm" disabled className="text-destructive">
                    {t(locale, 'settings.privacy.deleteData')}
                  </Button>
                </div>
              </section>
            </>
          )}
        </div>

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
      </main>
    </div>
  );
}
