/**
 * Settings page with account management.
 *
 * Standalone slim-nav shell; sections: General, Accounts, Spam & Filters,
 * Privacy, Encryption. Provides CRUD operations for mail accounts with i18n support.
 */

import { useEffect, useRef, useState } from 'react';
import { useNavigate } from '@tanstack/react-router';
import {
  Flag,
  KeyRound,
  Pencil,
  Plus,
  RefreshCw,
  Shield,
  SlidersHorizontal,
  Star,
  Trash2,
  Users,
  X,
} from 'lucide-react';
import { t } from '../i18n';
import { SlimPageNav, type SlimNavItem } from '@/components/slim-page-nav';
import { FolderRoleMapping } from './folder-role-mapping';
import { EncryptionSettings } from './encryption-settings';
import { NotificationSettings } from './notification-settings';
import { TotpEnroll } from './totp-enroll';
import { useUIStore } from '../stores/ui';
import { useAuthStore } from '../stores/auth';
import { api, type AuthMeResponse } from '@/lib/api-client';
import { MARK_READ_POLICIES } from '@/lib/mark-read-policy';
import type { ThemeMode } from '@/lib/theme';
import { saveLocale, saveMarkReadPolicy, applyMarkReadPolicy } from '@/lib/user-preferences';
import {
  fetchPrivacySettings,
  removeAllowSenderPrivacy,
  updatePrivacySettings,
  type PrivacySettings,
} from '@/lib/privacy-api';
import { fetchOAuthProviders, startOAuth } from '@/lib/oauth-api';
import {
  addSpamSender,
  fetchSpamSettings,
  removeSpamSender,
  saveSpamSettings,
  type SpamSettings,
  type SpamSettingsResponse,
  type SpamSensitivity,
} from '@/lib/spam-api';
import {
  nextAuthTypeOnSecret,
  oauthErrorKey,
  probeFormPatch,
  probeSourceLabel,
} from '@/lib/account-form';
import { isMicrosoftMailHost } from '@/lib/microsoft-mail';
import { isYandexMailHost } from '@/lib/yandex-mail';
import {
  resolveMailOAuthProvider,
  suggestsMailOAuth,
  type MailOAuthProvider,
} from '@/lib/mail-oauth-provider';
import { syncEvents$ } from '@/rxjs/sync-events';
import { Button } from '@/components/ui/button';
import { Badge } from '@/components/ui/badge';
import { Dialog, DialogContent, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Field, FieldGroup, FieldLabel } from '@/components/ui/field';
import { Input } from '@/components/ui/input';
import { Textarea } from '@/components/ui/textarea';
import { Switch } from '@/components/ui/switch';
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from '@/components/ui/tooltip';
import { cn } from '@/lib/utils';
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

type SettingsSection = 'general' | 'accounts' | 'spam' | 'privacy' | 'encryption';

interface MailAccount {
  id: string;
  displayName: string;
  emailAddress: string;
  protocol: string;
  imapHost?: string;
  imapPort?: number;
  imapSecurity?: string;
  authType?: string | null;
  jmapBaseUrl?: string | null;
  smtpHost?: string;
  smtpPort?: number;
  smtpSecurity?: string;
  signature?: string | null;
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
  /** The provider answers `/.well-known/jmap` — account can use JMAP. */
  jmapSupported?: boolean;
  /** `"oauth2"` when Microsoft Outlook/365 — use Sign in with Microsoft */
  authMethod?: string;
  imapHost?: string;
  imapPort?: number;
  imapSecurity?: string;
  smtpHost?: string;
  smtpPort?: number;
  smtpSecurity?: string;
}

const SPAM_SENSITIVITY = ['lenient', 'standard', 'strict'] as const;

export function SettingsPage() {
  const locale = useUIStore((s) => s.locale);
  const setLocale = useUIStore((s) => s.setLocale);
  const theme = useUIStore((s) => s.theme);
  const setTheme = useUIStore((s) => s.setTheme);
  const markReadPolicy = useUIStore((s) => s.markReadPolicy);
  const defaultAccountId = useUIStore((s) => s.defaultAccountId);
  const setDefaultAccount = useUIStore((s) => s.setDefaultAccount);
  const token = useAuthStore((s) => s.token);
  const clearSession = useAuthStore((s) => s.clearSession);
  const navigate = useNavigate();
  const [section, setSection] = useState<SettingsSection>('general');
  const [accounts, setAccounts] = useState<MailAccount[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [showAddForm, setShowAddForm] = useState(false);
  // True once the user clicks the protocol toggle — probe results must not
  // silently re-write a protocol the user chose while filling the form.
  const protocolTouchedRef = useRef(false);
  const [editingAccount, setEditingAccount] = useState<MailAccount | null>(null);

  // Form state
  const [formData, setFormData] = useState({
    displayName: '',
    signature: '',
    emailAddress: '',
    password: '',
    protocol: 'imap',
    authType: 'password',
    jmapBaseUrl: '',
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
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<{ ok: boolean; detail: string } | null>(null);
  const [totpPassword, setTotpPassword] = useState('');
  const [changingPassword, setChangingPassword] = useState(false);
  const [disablingTotp, setDisablingTotp] = useState(false);
  const [enrollingTotp, setEnrollingTotp] = useState(false);
  const [prefsError, setPrefsError] = useState<string | null>(null);
  const [prefsSaving, setPrefsSaving] = useState(false);
  const [privacySettings, setPrivacySettings] = useState<PrivacySettings | null>(null);
  const [spamSettings, setSpamSettings] = useState<SpamSettingsResponse | null>(null);
  const [spamSaving, setSpamSaving] = useState(false);
  const [newBlockedSender, setNewBlockedSender] = useState('');
  const [privacySaving, setPrivacySaving] = useState(false);
  const [privacyError, setPrivacyError] = useState<string | null>(null);
  const [removingAllowSender, setRemovingAllowSender] = useState<string | null>(null);
  const [oauthConfigured, setOauthConfigured] = useState<Record<MailOAuthProvider, boolean>>({
    microsoft: false,
    yandex: false,
  });
  const [oauthStarting, setOauthStarting] = useState(false);
  const [oauthMessage, setOauthMessage] = useState<string | null>(null);
  const [oauthError, setOauthError] = useState<string | null>(null);

  const suggestedOAuthProvider =
    probeResult?.authMethod === 'oauth2'
      ? resolveMailOAuthProvider(formData.emailAddress)
      : resolveMailOAuthProvider(formData.emailAddress);
  const suggestedAuthMethod =
    probeResult?.authMethod ?? (suggestedOAuthProvider ? 'oauth2' : undefined);
  const preferMailOAuth =
    suggestedAuthMethod === 'oauth2' &&
    !editingAccount &&
    suggestedOAuthProvider !== null &&
    oauthConfigured[suggestedOAuthProvider];

  useEffect(() => {
    const params = new URLSearchParams(window.location.search);
    const sectionParam = params.get('section');
    if (
      sectionParam === 'general' ||
      sectionParam === 'accounts' ||
      sectionParam === 'spam' ||
      sectionParam === 'privacy' ||
      sectionParam === 'encryption'
    ) {
      setSection(sectionParam);
    }
    const oauth = params.get('oauth');
    const detail = params.get('detail');
    if (oauth === 'ok') {
      setOauthMessage(
        detail === 'reconnected'
          ? t(locale, 'settings.accounts.oauthReconnected')
          : t(locale, 'settings.accounts.oauthOk'),
      );
      setSection('accounts');
    } else if (oauth === 'error') {
      setOauthError(t(locale, oauthErrorKey(detail)));
      setSection('accounts');
    }
    if (oauth) {
      params.delete('oauth');
      params.delete('detail');
      const next = params.toString();
      const path = `${window.location.pathname}${next ? `?${next}` : ''}`;
      window.history.replaceState({}, '', path);
    }

    void fetchAccounts();
    void fetchPrivacySettings()
      .then(setPrivacySettings)
      .catch(() => {});
    void fetchSpamSettings()
      .then(setSpamSettings)
      .catch(() => {});
    void fetchOAuthProviders()
      .then(({ providers }) => {
        setOauthConfigured({
          microsoft: providers.some((p) => p.id === 'microsoft' && p.configured),
          yandex: providers.some((p) => p.id === 'yandex' && p.configured),
        });
      })
      .catch(() =>
        setOauthConfigured({
          microsoft: false,
          yandex: false,
        }),
      );
    void api<AuthMeResponse>('/auth/me')
      .then((me) => {
        setTotpEnabled(Boolean(me.totp_enabled));
        applyMarkReadPolicy(me.mark_read_policy);
      })
      .catch(() => {});
    // locale only for oauth flash on first mount
    // eslint-disable-next-line react-hooks/exhaustive-deps -- one-shot URL + bootstrap
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

  async function handleMailOAuthSignIn() {
    setOauthError(null);
    setOauthMessage(null);
    const provider = suggestedOAuthProvider;
    if (!provider) {
      setOauthError(t(locale, 'settings.accounts.oauthEmailRequired'));
      return;
    }
    if (!oauthConfigured[provider]) {
      setOauthError(
        t(
          locale,
          provider === 'yandex'
            ? 'settings.accounts.yandexUnavailable'
            : 'settings.accounts.microsoftUnavailable',
        ),
      );
      return;
    }
    const email = formData.emailAddress.trim();
    if (!email.includes('@')) {
      setOauthError(t(locale, 'settings.accounts.oauthEmailRequired'));
      return;
    }
    try {
      setOauthStarting(true);
      const { authorizeUrl } = await startOAuth(email);
      window.location.assign(authorizeUrl);
    } catch (err: unknown) {
      setOauthError(err instanceof Error ? err.message : String(err));
      setOauthStarting(false);
    }
  }

  async function applySpamSettings(patch: Partial<SpamSettings>): Promise<void> {
    if (!spamSettings) return;
    const next = {
      enabled: spamSettings.enabled,
      learn: spamSettings.learn,
      autoDelete: spamSettings.autoDelete,
      sensitivity: spamSettings.sensitivity,
      ...patch,
    };
    setSpamSaving(true);
    try {
      setSpamSettings(await saveSpamSettings(next));
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSpamSaving(false);
    }
  }

  async function handleAddBlockedSender(): Promise<void> {
    const email = newBlockedSender.trim();
    if (!email) return;
    setSpamSaving(true);
    try {
      setSpamSettings(await addSpamSender(email, 'blocked'));
      setNewBlockedSender('');
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSpamSaving(false);
    }
  }

  async function handleRemoveSender(email: string): Promise<void> {
    setSpamSaving(true);
    try {
      setSpamSettings(await removeSpamSender(email));
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSpamSaving(false);
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
      const authMethod =
        data.authMethod ??
        (suggestsMailOAuth(formData.emailAddress) ||
        isMicrosoftMailHost(data.imapHost ?? '') ||
        isMicrosoftMailHost(data.smtpHost ?? '') ||
        isYandexMailHost(data.imapHost ?? '') ||
        isYandexMailHost(data.smtpHost ?? '')
          ? 'oauth2'
          : undefined);
      const enriched = authMethod ? { ...data, authMethod } : data;
      setProbeResult(enriched);
      if (enriched.found) {
        setFormData((prev) => ({
          ...prev,
          protocol: protocolTouchedRef.current ? prev.protocol : enriched.protocol,
          imapHost: enriched.imapHost || prev.imapHost,
          imapPort: enriched.imapPort || prev.imapPort,
          imapSecurity: enriched.imapSecurity || prev.imapSecurity,
          smtpHost: enriched.smtpHost || prev.smtpHost,
          smtpPort: enriched.smtpPort || prev.smtpPort,
          smtpSecurity: enriched.smtpSecurity || prev.smtpSecurity,
        }));
      }
      // JMAP-capable providers default to JMAP (Lyra prefers JMAP). Leave
      // authType untouched — a probe landing after the user pasted a token
      // must not silently flip the method back to Password — and honor a
      // protocol the user already picked by hand. A pasted Fastmail token
      // still upgrades to Bearer here: at paste time the form may still be
      // on the IMAP default, so the password-field flip alone can miss it.
      if (!editingAccount) {
        setFormData((prev) => {
          const patch = probeFormPatch(enriched, {
            protocolTouched: protocolTouchedRef.current,
            protocol: prev.protocol,
            authType: prev.authType,
            secret: prev.password,
          });
          return patch ? { ...prev, ...patch } : prev;
        });
      }
    } catch (err: any) {
      setError(err.message);
    } finally {
      setProbing(false);
    }
  }

  useEffect(() => {
    if (!showAddForm || editingAccount) return;
    const email = formData.emailAddress.trim();
    // Auto-probe any complete address: the probe answers IMAP autoconfig,
    // OAuth providers, and JMAP support (/.well-known/jmap).
    const domain = email.split('@')[1] ?? '';
    if (!email.includes('@') || !domain.includes('.')) return;
    const timer = window.setTimeout(() => {
      void handleProbe();
    }, 400);
    return () => window.clearTimeout(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- debounced probe on any complete address
  }, [formData.emailAddress, showAddForm, editingAccount]);

  /** Dial the mailbox with the form's current values; nothing is saved. */
  async function handleTestConnection() {
    if (!formData.emailAddress.trim()) return;
    setTesting(true);
    setTestResult(null);
    try {
      const isJmap = formData.protocol === 'jmap';
      const res = await api<{ ok: boolean; detail: string; folderCount: number | null }>(
        '/accounts/test-connection',
        {
          method: 'POST',
          body: JSON.stringify({
            emailAddress: formData.emailAddress,
            protocol: formData.protocol,
            password: formData.password,
            authType: isJmap ? formData.authType : undefined,
            jmapBaseUrl: isJmap && formData.jmapBaseUrl ? formData.jmapBaseUrl : undefined,
            imapHost: isJmap ? null : formData.imapHost || null,
            imapPort: isJmap ? null : formData.imapPort || null,
            imapSecurity: isJmap ? null : formData.imapSecurity,
          }),
        },
      );
      setTestResult({ ok: res.ok, detail: res.detail });
    } catch (err: unknown) {
      setTestResult({
        ok: false,
        detail: err instanceof Error ? err.message : String(err),
      });
    } finally {
      setTesting(false);
    }
  }

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    try {
      const url = editingAccount ? `/accounts/${editingAccount.id}` : '/accounts';
      const method = editingAccount ? 'PUT' : 'POST';
      const isJmap = formData.protocol === 'jmap';
      const body: any = {
        displayName: formData.displayName,
        emailAddress: formData.emailAddress,
        protocol: formData.protocol,
        authType: isJmap ? formData.authType : undefined,
        jmapBaseUrl: isJmap && formData.jmapBaseUrl ? formData.jmapBaseUrl : undefined,
        imapHost: isJmap ? null : formData.imapHost || null,
        imapPort: isJmap ? null : formData.imapPort || null,
        imapSecurity: isJmap ? null : formData.imapSecurity,
        smtpHost: isJmap ? null : formData.smtpHost || null,
        smtpPort: isJmap ? null : formData.smtpPort || null,
        smtpSecurity: isJmap ? null : formData.smtpSecurity,
      };
      if (formData.password) {
        body.password = formData.password;
      }
      if (editingAccount) {
        body.signature = formData.signature || null;
        body.authType = isJmap ? formData.authType : undefined;
        body.jmapBaseUrl = isJmap && formData.jmapBaseUrl ? formData.jmapBaseUrl : null;
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
      signature: account.signature ?? '',
      emailAddress: account.emailAddress,
      password: '',
      protocol: account.protocol,
      imapHost: account.imapHost || '',
      imapPort: account.imapPort || 993,
      // Legacy 'none' values (removed insecure mode) coerce to 'tls' so the
      // select never shows a blank value and saving doesn't 400.
      imapSecurity:
        !account.imapSecurity || account.imapSecurity === 'none' ? 'tls' : account.imapSecurity,
      authType: account.authType === 'bearer' ? 'bearer' : 'password',
      jmapBaseUrl: account.jmapBaseUrl || '',
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
    protocolTouchedRef.current = false;
    setFormData({
      displayName: '',
      signature: '',
      emailAddress: '',
      password: '',
      protocol: 'imap',
      authType: 'password',
      jmapBaseUrl: '',
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

  async function handleGravatarAvatarsChange(checked: boolean) {
    setPrivacyError(null);
    setPrivacySaving(true);
    try {
      const updated = await updatePrivacySettings({ gravatarAvatars: checked });
      setPrivacySettings(updated);
    } catch (err: unknown) {
      setPrivacyError(err instanceof Error ? err.message : String(err));
    } finally {
      setPrivacySaving(false);
    }
  }

  async function handleRemoveAllowSender(sender: string) {
    setPrivacyError(null);
    setRemovingAllowSender(sender);
    try {
      const updated = await removeAllowSenderPrivacy(sender);
      setPrivacySettings(updated);
    } catch (err: unknown) {
      setPrivacyError(err instanceof Error ? err.message : String(err));
    } finally {
      setRemovingAllowSender(null);
    }
  }

  const remoteImagesMode = privacySettings?.remoteImages ?? 'block';
  const remoteContentAllowlist = privacySettings?.remoteContentAllowlist ?? [];

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
    {
      key: 'encryption',
      label: t(locale, 'settings.encryption.title'),
      icon: KeyRound,
      active: section === 'encryption',
      onClick: () => setSection('encryption'),
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
    encryption: {
      title: t(locale, 'settings.encryption.title'),
      subtitle: t(locale, 'settings.encryption.subtitle'),
    },
  };

  const soonBadge = (
    <Badge variant="outline" className="text-[10.5px] font-normal text-muted-foreground">
      {t(locale, 'common.soon')}
    </Badge>
  );

  return (
    <div className="flex h-svh">
      <SlimPageNav section={t(locale, 'settings.section')} items={navItems} />
      <main className="flex-1 overflow-auto bg-background">
        <header className="border-b border-border px-8 pb-5 pt-7">
          {/* Title shares the content measure so it aligns with the cards
              below, and the whole column centers on wide screens. */}
          <div className="mx-auto w-full max-w-2xl">
            <h1 className="font-display text-xl font-medium">{sectionMeta[section].title}</h1>
            <p className="text-[12.5px] text-muted-foreground">{sectionMeta[section].subtitle}</p>
          </div>
        </header>

        <div className="stagger-in mx-auto w-full max-w-2xl space-y-4 px-8 py-6">
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
                    <div className="text-xs text-muted-foreground">
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

              <NotificationSettings />

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
                    <p className="text-xs text-muted-foreground">
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
                        <p className="text-xs text-muted-foreground">
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
              {oauthError && <div className="text-sm text-destructive">{oauthError}</div>}
              {oauthMessage && (
                <div className="text-sm text-muted-foreground" role="status">
                  {oauthMessage}
                </div>
              )}
              {syncMessage && (
                <div className="text-sm text-muted-foreground" role="status">
                  {syncMessage}
                </div>
              )}

              {loading ? (
                <div className="text-sm text-muted-foreground">{t(locale, 'common.loading')}</div>
              ) : (
                <TooltipProvider delayDuration={300}>
                  {accounts.length > 0 ? (
                    <section className="overflow-hidden rounded-[10px] border border-border/70 bg-card">
                      {accounts.map((account, index) => {
                        const isDefault = defaultAccountId === account.id;
                        const setDefaultLabel = t(locale, 'settings.accounts.setDefault');
                        const syncLabel =
                          syncingId === account.id
                            ? t(locale, 'sync.syncing')
                            : t(locale, 'settings.syncNow');
                        const manageLabel = t(locale, 'settings.manage');
                        const deleteLabel = t(locale, 'common.delete');
                        return (
                          <div
                            key={account.id}
                            className={cn(
                              'flex items-center gap-3 px-4 py-3',
                              index > 0 && 'border-t border-border/60',
                            )}
                          >
                            <div className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-accent text-[13px] font-medium text-foreground">
                              {(account.displayName || account.emailAddress)
                                .charAt(0)
                                .toUpperCase()}
                            </div>
                            <div className="min-w-0 flex-1">
                              <div className="flex min-w-0 items-baseline gap-2">
                                <span className="truncate text-[13.5px] font-medium">
                                  {account.emailAddress}
                                </span>
                                {isDefault ? (
                                  <span className="shrink-0 text-[11px] text-muted-foreground">
                                    {t(locale, 'settings.accounts.defaultBadge')}
                                  </span>
                                ) : null}
                              </div>
                              <div className="mt-0.5 flex flex-wrap items-center gap-x-2 gap-y-0.5 text-[11.5px] text-muted-foreground">
                                <span
                                  className={cn(
                                    'size-1.5 rounded-full',
                                    syncErrors[account.id] ? 'bg-destructive' : 'bg-ok',
                                  )}
                                />
                                {account.lastSyncAt ? (
                                  <span>
                                    {t(locale, 'sync.lastSync')}:{' '}
                                    {formatLastSync(account.lastSyncAt)}
                                  </span>
                                ) : null}
                                <span>{account.protocol.toUpperCase()}</span>
                                {account.displayName &&
                                account.displayName !== account.emailAddress ? (
                                  <span>{account.displayName}</span>
                                ) : null}
                              </div>
                              {syncErrors[account.id] ? (
                                <p className="mt-1 text-xs text-destructive">
                                  {t(locale, 'sync.syncFailed')}: {syncErrors[account.id]}
                                </p>
                              ) : null}
                            </div>
                            <div className="flex shrink-0 items-center gap-0.5">
                              {!isDefault ? (
                                <Tooltip>
                                  <TooltipTrigger asChild>
                                    <Button
                                      variant="ghost"
                                      size="icon-sm"
                                      aria-label={setDefaultLabel}
                                      onClick={() => setDefaultAccount(account.id)}
                                    >
                                      <Star className="size-3.5" />
                                    </Button>
                                  </TooltipTrigger>
                                  <TooltipContent>{setDefaultLabel}</TooltipContent>
                                </Tooltip>
                              ) : (
                                <span
                                  className="flex size-8 items-center justify-center text-muted-foreground"
                                  title={t(locale, 'settings.accounts.defaultBadge')}
                                >
                                  <Star className="size-3.5 fill-current" />
                                </span>
                              )}
                              <Tooltip>
                                <TooltipTrigger asChild>
                                  <Button
                                    variant="ghost"
                                    size="icon-sm"
                                    aria-label={syncLabel}
                                    disabled={syncingId === account.id}
                                    onClick={() => void handleSync(account.id)}
                                  >
                                    <RefreshCw
                                      className={cn(
                                        'size-3.5',
                                        syncingId === account.id && 'animate-spin',
                                      )}
                                    />
                                  </Button>
                                </TooltipTrigger>
                                <TooltipContent>{syncLabel}</TooltipContent>
                              </Tooltip>
                              <Tooltip>
                                <TooltipTrigger asChild>
                                  <Button
                                    variant="ghost"
                                    size="icon-sm"
                                    aria-label={manageLabel}
                                    onClick={() => handleEdit(account)}
                                  >
                                    <Pencil className="size-3.5" />
                                  </Button>
                                </TooltipTrigger>
                                <TooltipContent>{manageLabel}</TooltipContent>
                              </Tooltip>
                              <Tooltip>
                                <TooltipTrigger asChild>
                                  <Button
                                    variant="ghost"
                                    size="icon-sm"
                                    className="text-destructive hover:text-destructive"
                                    aria-label={deleteLabel}
                                    onClick={() => handleDelete(account.id)}
                                  >
                                    <Trash2 className="size-3.5" />
                                  </Button>
                                </TooltipTrigger>
                                <TooltipContent>{deleteLabel}</TooltipContent>
                              </Tooltip>
                            </div>
                          </div>
                        );
                      })}
                    </section>
                  ) : null}

                  <button
                    type="button"
                    className="flex w-full items-center justify-center gap-2 rounded-[10px] border border-dashed border-border/80 px-5 py-3.5 text-[13px] text-muted-foreground transition-colors hover:bg-accent/50 hover:text-foreground"
                    onClick={() => {
                      resetForm();
                      setEditingAccount(null);
                      setShowAddForm(true);
                    }}
                  >
                    <Plus size={16} />
                    {t(locale, 'settings.accounts.add')}
                  </button>
                  {accounts.length === 0 ? (
                    <p className="text-center text-xs text-muted-foreground">
                      {t(locale, 'settings.accounts.empty')}
                    </p>
                  ) : null}
                </TooltipProvider>
              )}
            </>
          )}

          {section === 'spam' && (
            <div className="space-y-4">
              <section className="space-y-3 rounded-[10px] border border-border bg-card px-5 py-4">
                <div className="flex items-center gap-2">
                  <h2 className="text-[13px] font-medium">
                    {t(locale, 'settings.spam.filtering')}
                  </h2>
                </div>
                {(
                  [
                    ['enable', 'enableDesc', 'enabled'],
                    ['learn', 'learnDesc', 'learn'],
                    ['autoDelete', 'autoDeleteDesc', 'autoDelete'],
                  ] as const
                ).map(([key, descKey, field], i) => (
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
                      <div className="text-xs text-muted-foreground">
                        {t(locale, `settings.spam.${descKey}`)}
                      </div>
                    </div>
                    <Switch
                      checked={spamSettings ? spamSettings[field] : false}
                      onCheckedChange={(checked) => void applySpamSettings({ [field]: checked })}
                      disabled={spamSaving || !spamSettings}
                    />
                  </div>
                ))}
              </section>

              <section className="space-y-3 rounded-[10px] border border-border bg-card px-5 py-4">
                <div className="flex items-center gap-2">
                  <h2 className="text-[13px] font-medium">
                    {t(locale, 'settings.spam.sensitivity')}
                  </h2>
                </div>
                <div className="flex items-center justify-between gap-3">
                  <div className="text-xs text-muted-foreground">
                    {t(locale, 'settings.spam.sensitivityDesc')}
                  </div>
                  <div className="flex h-8 items-center rounded-lg bg-accent p-0.5 text-muted-foreground">
                    {SPAM_SENSITIVITY.map((level) => (
                      <button
                        key={level}
                        type="button"
                        disabled={spamSaving || !spamSettings}
                        className={
                          spamSettings?.sensitivity === level
                            ? 'h-7 rounded-md bg-card px-3 text-sm font-medium text-foreground'
                            : 'h-7 rounded-md px-3 text-sm font-medium'
                        }
                        onClick={() =>
                          void applySpamSettings({ sensitivity: level as SpamSensitivity })
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
                </div>
                <p className="text-xs text-muted-foreground">
                  {t(locale, 'settings.spam.blockedDesc')}
                </p>
                {(spamSettings?.senders ?? [])
                  .filter((sender) => sender.list === 'blocked')
                  .map((sender) => (
                    <div
                      key={`${sender.list}:${sender.email}`}
                      className="flex items-center justify-between gap-3 border-t border-border pt-3"
                    >
                      <span className="text-[13px]">{sender.email}</span>
                      <Button
                        variant="ghost"
                        size="sm"
                        disabled={spamSaving}
                        aria-label={t(locale, 'common.delete')}
                        onClick={() => void handleRemoveSender(sender.email)}
                      >
                        <X size={14} />
                      </Button>
                    </div>
                  ))}
                {spamSettings &&
                  spamSettings.senders.filter((s) => s.list === 'blocked').length === 0 && (
                    <p className="border-t border-border pt-3 text-xs text-muted-foreground">
                      {t(locale, 'settings.spam.noBlocked')}
                    </p>
                  )}
                <div className="flex items-center gap-2 border-t border-border pt-3">
                  <input
                    type="text"
                    value={newBlockedSender}
                    onChange={(e) => setNewBlockedSender(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === 'Enter') void handleAddBlockedSender();
                    }}
                    placeholder={t(locale, 'settings.spam.addSender')}
                    disabled={spamSaving || !spamSettings}
                    className="h-9 w-full rounded-md border border-input bg-transparent px-3 text-sm"
                  />
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={spamSaving || !newBlockedSender.trim()}
                    onClick={() => void handleAddBlockedSender()}
                  >
                    {t(locale, 'common.add')}
                  </Button>
                </div>
              </section>
            </div>
          )}

          {section === 'privacy' && (
            <>
              <section className="space-y-3 rounded-[10px] border border-border bg-card px-5 py-4">
                <h2 className="text-[13px] font-medium">
                  {t(locale, 'settings.privacy.remoteImages')}
                </h2>
                <p className="text-xs text-muted-foreground">
                  {t(locale, 'settings.privacy.remoteImagesHint')}
                </p>
                <div className="flex flex-wrap items-center justify-between gap-3">
                  <span className="text-xs text-muted-foreground">
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
                <div className="flex items-center justify-between gap-3">
                  <div>
                    <h2 className="text-[13px] font-medium">
                      {t(locale, 'settings.privacy.gravatarAvatars')}
                    </h2>
                    <p className="text-xs text-muted-foreground">
                      {t(locale, 'settings.privacy.gravatarAvatarsHint')}
                    </p>
                  </div>
                  <Switch
                    checked={privacySettings?.gravatarAvatars ?? false}
                    onCheckedChange={(checked) => void handleGravatarAvatarsChange(checked)}
                    disabled={privacySaving || !privacySettings}
                  />
                </div>
                {privacyError ? (
                  <div className="text-sm text-destructive">{privacyError}</div>
                ) : null}
              </section>

              <section className="space-y-3 rounded-[10px] border border-border bg-card px-5 py-4">
                <h2 className="text-[13px] font-medium">
                  {t(locale, 'settings.privacy.allowlistTitle')}
                </h2>
                <p className="text-xs text-muted-foreground">
                  {t(locale, 'settings.privacy.allowlistHint')}
                </p>
                {remoteContentAllowlist.length === 0 ? (
                  <p className="text-xs text-muted-foreground">
                    {t(locale, 'settings.privacy.allowlistEmpty')}
                  </p>
                ) : (
                  remoteContentAllowlist.map((sender) => (
                    <div
                      key={sender}
                      className="flex items-center justify-between gap-3 border-t border-border pt-3 first:border-t-0 first:pt-0"
                    >
                      <span className="min-w-0 truncate text-[13px] text-foreground">{sender}</span>
                      <Button
                        variant="ghost"
                        size="sm"
                        disabled={removingAllowSender === sender}
                        aria-label={t(locale, 'settings.privacy.removeAllowSender')}
                        onClick={() => void handleRemoveAllowSender(sender)}
                      >
                        <X size={14} />
                      </Button>
                    </div>
                  ))
                )}
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
                      <div className="text-xs text-muted-foreground">
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
                    <div className="text-xs text-muted-foreground">
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
                    <div className="text-xs text-muted-foreground">
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

          {section === 'encryption' && <EncryptionSettings />}
        </div>

        <Dialog
          open={showAddForm}
          onOpenChange={(open) => {
            if (!open) {
              setShowAddForm(false);
              setEditingAccount(null);
            }
          }}
        >
          <DialogContent
            className="max-h-[calc(100dvh-2rem)] grid-rows-[auto_minmax(0,1fr)] overflow-hidden sm:max-w-xl"
            showCloseButton
          >
            <DialogHeader>
              <DialogTitle>
                {editingAccount
                  ? t(locale, 'settings.accounts.edit')
                  : t(locale, 'settings.accounts.add')}
              </DialogTitle>
            </DialogHeader>
            <form
              onSubmit={handleSubmit}
              className="grid min-h-0 grid-rows-[minmax(0,1fr)_auto] gap-4"
            >
              <FieldGroup className="min-h-0 overflow-y-auto">
                <Field>
                  <FieldLabel htmlFor="settings-display-name">
                    {t(locale, 'settings.accounts.displayName')}
                  </FieldLabel>
                  <Input
                    id="settings-display-name"
                    value={formData.displayName}
                    onChange={(e) =>
                      setFormData((prev) => ({
                        ...prev,
                        displayName: e.target.value,
                      }))
                    }
                    required
                  />
                </Field>

                {/* Signature is an identity concern, not a connection one —
                    it belongs to editing an existing account only. */}
                {editingAccount && (
                  <Field>
                    <FieldLabel htmlFor="settings-signature">
                      {t(locale, 'settings.accounts.signature')}
                    </FieldLabel>
                    <Textarea
                      id="settings-signature"
                      className="min-h-24"
                      value={formData.signature}
                      onChange={(e) =>
                        setFormData((prev) => ({
                          ...prev,
                          signature: e.target.value,
                        }))
                      }
                      placeholder={t(locale, 'settings.accounts.signatureHint')}
                    />
                  </Field>
                )}

                <Field>
                  <FieldLabel htmlFor="settings-email">
                    {t(locale, 'settings.accounts.email')}
                  </FieldLabel>
                  <div className="flex gap-2">
                    <Input
                      id="settings-email"
                      type="email"
                      className="flex-1"
                      value={formData.emailAddress}
                      onChange={(e) =>
                        setFormData((prev) => ({
                          ...prev,
                          emailAddress: e.target.value,
                        }))
                      }
                      required
                    />
                    <Button
                      type="button"
                      variant="outline"
                      onClick={handleProbe}
                      disabled={probing}
                    >
                      {probing
                        ? t(locale, 'settings.accounts.probing')
                        : t(locale, 'settings.accounts.autoDetect')}
                    </Button>
                  </div>
                </Field>

                {!editingAccount && !preferMailOAuth && (
                  <Field>
                    <FieldLabel>{t(locale, 'settings.accounts.protocol')}</FieldLabel>
                    <div className="flex gap-0.5 rounded-lg bg-accent p-0.5">
                      <button
                        type="button"
                        className={cn(
                          'flex-1 rounded-md px-3 py-1.5 text-xs font-medium transition-colors',
                          formData.protocol === 'jmap'
                            ? 'bg-card text-foreground'
                            : 'text-muted-foreground hover:text-foreground',
                        )}
                        onClick={() => {
                          protocolTouchedRef.current = true;
                          setFormData((prev) => ({ ...prev, protocol: 'jmap' }));
                        }}
                      >
                        {t(locale, 'settings.accounts.protocolJmap')}
                      </button>
                      <button
                        type="button"
                        className={cn(
                          'flex-1 rounded-md px-3 py-1.5 text-xs font-medium transition-colors',
                          formData.protocol !== 'jmap'
                            ? 'bg-card text-foreground'
                            : 'text-muted-foreground hover:text-foreground',
                        )}
                        onClick={() => {
                          protocolTouchedRef.current = true;
                          setFormData((prev) => ({ ...prev, protocol: 'imap' }));
                        }}
                      >
                        {t(locale, 'settings.accounts.protocolImap')}
                      </button>
                    </div>
                  </Field>
                )}

                {(probeResult?.found ||
                  probeResult?.jmapSupported ||
                  suggestedAuthMethod === 'oauth2') && (
                  <div className="probe-result space-y-2">
                    {(probeResult?.found || suggestedAuthMethod === 'oauth2') && (
                      <p>
                        {suggestedAuthMethod === 'oauth2'
                          ? suggestedOAuthProvider === 'yandex'
                            ? t(locale, 'settings.accounts.probeYandexOAuth')
                            : t(locale, 'settings.accounts.probeMicrosoftOAuth')
                          : t(locale, 'settings.accounts.probeFound', {
                              source: probeSourceLabel(locale, probeResult?.source),
                            })}
                      </p>
                    )}
                    {probeResult?.jmapSupported && (
                      <p className="text-xs text-muted-foreground">
                        {t(locale, 'settings.accounts.probeJmap')}
                      </p>
                    )}
                    {suggestedAuthMethod === 'oauth2' &&
                      !editingAccount &&
                      suggestedOAuthProvider && (
                        <div className="space-y-2 rounded-md border border-border bg-muted/40 p-3">
                          <p className="text-xs text-muted-foreground">
                            {oauthConfigured[suggestedOAuthProvider]
                              ? t(
                                  locale,
                                  suggestedOAuthProvider === 'yandex'
                                    ? 'settings.accounts.yandexHint'
                                    : 'settings.accounts.microsoftHint',
                                )
                              : t(
                                  locale,
                                  suggestedOAuthProvider === 'yandex'
                                    ? 'settings.accounts.yandexUnavailable'
                                    : 'settings.accounts.microsoftUnavailable',
                                )}
                          </p>
                          {oauthConfigured[suggestedOAuthProvider] && (
                            <Button
                              type="button"
                              className="w-full"
                              disabled={oauthStarting}
                              onClick={() => void handleMailOAuthSignIn()}
                            >
                              {oauthStarting
                                ? t(
                                    locale,
                                    suggestedOAuthProvider === 'yandex'
                                      ? 'settings.accounts.yandexStarting'
                                      : 'settings.accounts.microsoftStarting',
                                  )
                                : t(
                                    locale,
                                    suggestedOAuthProvider === 'yandex'
                                      ? 'settings.accounts.yandex'
                                      : 'settings.accounts.microsoft',
                                  )}
                            </Button>
                          )}
                        </div>
                      )}
                  </div>
                )}

                {!preferMailOAuth && formData.protocol === 'jmap' && (
                  <Field>
                    <FieldLabel>{t(locale, 'settings.accounts.authMethod')}</FieldLabel>
                    <div className="flex gap-0.5 rounded-lg bg-accent p-0.5">
                      <button
                        type="button"
                        className={cn(
                          'flex-1 rounded-md px-3 py-1.5 text-xs font-medium transition-colors',
                          formData.authType === 'bearer'
                            ? 'bg-card text-foreground'
                            : 'text-muted-foreground hover:text-foreground',
                        )}
                        onClick={() => setFormData((prev) => ({ ...prev, authType: 'bearer' }))}
                      >
                        {t(locale, 'settings.accounts.authApiToken')}
                      </button>
                      <button
                        type="button"
                        className={cn(
                          'flex-1 rounded-md px-3 py-1.5 text-xs font-medium transition-colors',
                          formData.authType !== 'bearer'
                            ? 'bg-card text-foreground'
                            : 'text-muted-foreground hover:text-foreground',
                        )}
                        onClick={() => setFormData((prev) => ({ ...prev, authType: 'password' }))}
                      >
                        {t(locale, 'settings.accounts.authPassword')}
                      </button>
                    </div>
                  </Field>
                )}

                {!preferMailOAuth && (
                  <Field>
                    <FieldLabel htmlFor="settings-password">
                      {formData.protocol === 'jmap' && formData.authType === 'bearer'
                        ? t(locale, 'settings.accounts.apiToken')
                        : t(locale, 'settings.accounts.password')}
                    </FieldLabel>
                    <Input
                      id="settings-password"
                      type="password"
                      value={formData.password}
                      onChange={(e) => {
                        const v = e.target.value;
                        setFormData((prev) => ({
                          ...prev,
                          password: v,
                          // Fastmail API tokens (fmu1-…) only authenticate
                          // as Bearer; flip the method on paste so Password
                          // + token can't reach the server and 401.
                          authType: nextAuthTypeOnSecret(prev.authType, prev.protocol, v),
                        }));
                      }}
                      required={!editingAccount && !preferMailOAuth}
                    />
                    {formData.protocol === 'jmap' && formData.authType === 'bearer' && (
                      <p className="text-xs text-muted-foreground">
                        {t(locale, 'settings.accounts.apiTokenHint')}
                      </p>
                    )}
                  </Field>
                )}

                {!preferMailOAuth && formData.protocol === 'jmap' && (
                  <Field>
                    <FieldLabel htmlFor="settings-jmap-url">
                      {t(locale, 'settings.accounts.jmapServerUrl')}
                    </FieldLabel>
                    <Input
                      id="settings-jmap-url"
                      value={formData.jmapBaseUrl}
                      onChange={(e) =>
                        setFormData((prev) => ({ ...prev, jmapBaseUrl: e.target.value }))
                      }
                      placeholder="https://api.fastmail.com"
                    />
                    <p className="text-xs text-muted-foreground">
                      {t(locale, 'settings.accounts.jmapServerUrlHint')}
                    </p>
                  </Field>
                )}

                {!preferMailOAuth && formData.protocol !== 'jmap' && (
                  <>
                    <Field className="gap-3 rounded-md border border-border/60 p-3">
                      <p className="text-sm font-medium">
                        {t(locale, 'settings.accounts.imapSettings')}
                      </p>
                      <div className="grid grid-cols-[1fr_6rem] gap-3">
                        <Field>
                          <FieldLabel>{t(locale, 'settings.accounts.host')}</FieldLabel>
                          <Input
                            value={formData.imapHost}
                            onChange={(e) =>
                              setFormData((prev) => ({
                                ...prev,
                                imapHost: e.target.value,
                              }))
                            }
                          />
                        </Field>
                        <Field>
                          <FieldLabel>{t(locale, 'settings.accounts.port')}</FieldLabel>
                          <Input
                            type="number"
                            value={formData.imapPort}
                            onChange={(e) =>
                              setFormData((prev) => ({
                                ...prev,
                                imapPort: parseInt(e.target.value),
                              }))
                            }
                          />
                        </Field>
                      </div>
                      <Field>
                        <FieldLabel>{t(locale, 'settings.accounts.security')}</FieldLabel>
                        <select
                          className="h-9 w-full rounded-md border border-input bg-transparent px-3 text-sm"
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
                      </Field>
                    </Field>

                    <Field className="gap-3 rounded-md border border-border/60 p-3">
                      <p className="text-sm font-medium">
                        {t(locale, 'settings.accounts.smtpSettings')}
                      </p>
                      <div className="grid grid-cols-[1fr_6rem] gap-3">
                        <Field>
                          <FieldLabel>{t(locale, 'settings.accounts.host')}</FieldLabel>
                          <Input
                            value={formData.smtpHost}
                            onChange={(e) =>
                              setFormData((prev) => ({
                                ...prev,
                                smtpHost: e.target.value,
                              }))
                            }
                          />
                        </Field>
                        <Field>
                          <FieldLabel>{t(locale, 'settings.accounts.port')}</FieldLabel>
                          <Input
                            type="number"
                            value={formData.smtpPort}
                            onChange={(e) =>
                              setFormData((prev) => ({
                                ...prev,
                                smtpPort: parseInt(e.target.value),
                              }))
                            }
                          />
                        </Field>
                      </div>
                      <Field>
                        <FieldLabel>{t(locale, 'settings.accounts.security')}</FieldLabel>
                        <select
                          className="h-9 w-full rounded-md border border-input bg-transparent px-3 text-sm"
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
                      </Field>
                    </Field>
                  </>
                )}

                {editingAccount ? (
                  <FolderRoleMapping accountId={editingAccount.id} locale={locale} />
                ) : null}
              </FieldGroup>

              {/* Footer: actions on their own row, status always full-width
                  below — a long probe result must never sit between the
                  buttons and wrap them onto separate rows. */}
              <div className="space-y-2">
                <div className="flex items-center gap-2">
                  <Button
                    type="button"
                    variant="outline"
                    disabled={testing || !formData.emailAddress.trim()}
                    onClick={() => void handleTestConnection()}
                    className="mr-auto"
                  >
                    {testing
                      ? t(locale, 'settings.accounts.testing')
                      : t(locale, 'settings.accounts.testConnection')}
                  </Button>
                  <Button
                    type="button"
                    variant="outline"
                    onClick={() => {
                      setShowAddForm(false);
                      setEditingAccount(null);
                    }}
                  >
                    {t(locale, 'common.cancel')}
                  </Button>
                  {!preferMailOAuth && (
                    <Button type="submit">
                      {editingAccount ? t(locale, 'common.save') : t(locale, 'common.add')}
                    </Button>
                  )}
                </div>
                {testResult && (
                  <p
                    className={cn(
                      'text-[12.5px] leading-snug',
                      testResult.ok ? 'text-ok' : 'text-destructive',
                    )}
                    role="status"
                  >
                    {testResult.ok
                      ? `${t(locale, 'settings.accounts.testOk')} — ${testResult.detail}`
                      : `${t(locale, 'settings.accounts.testFail')}: ${testResult.detail}`}
                  </p>
                )}
              </div>
            </form>
          </DialogContent>
        </Dialog>
      </main>
    </div>
  );
}
