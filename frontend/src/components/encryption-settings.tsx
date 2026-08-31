/**
 * Settings → Encryption, Thunderbird-style:
 *   1. Passphrase-cache preference.
 *   2. Per-account identity status ("personal key") with Add Key…
 *   3. Key manager: every key, searchable, identity vs contact groups,
 *      unlock/export/delete/rebind actions.
 *   4. Import (optional target account).
 * Unlock dialog (XState) with remember-choice, idle-relock indicator.
 */

import { useCallback, useEffect, useMemo, useState } from 'react';
import { useMachine } from '@xstate/react';
import { KeyRound, Lock, Unlock } from 'lucide-react';
import { t } from '@/i18n';
import { useUIStore } from '@/stores/ui';
import { Button } from '@/components/ui/button';
import { Badge } from '@/components/ui/badge';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select';
import { Textarea } from '@/components/ui/textarea';
import { opengpgUnlockMachine } from '@/machines/opengpg-unlock';
import {
  type CacheMode,
  type OpengpgKey,
  type OpengpgSettings,
  deleteOpengpgKey,
  downloadArmored,
  exportOpengpgKey,
  fetchOpengpgSettings,
  generateOpengpgKey,
  importOpengpgKey,
  listOpengpgKeys,
  lockOpengpgKeys,
  setOpengpgKeyAccount,
  setPrimaryOpengpgKey,
  updateOpengpgSettings,
} from '@/lib/opengpg-api';
import { api } from '@/lib/api-client';

/** Match backend IDLE_TIMEOUT (10 minutes). */
const IDLE_MS = 10 * 60 * 1000;

interface AccountLite {
  id: string;
  emailAddress: string;
  displayName?: string | null;
}

function shortFp(fp: string): string {
  const clean = fp.replace(/\s/g, '');
  if (clean.length < 8) return clean;
  return clean.slice(-8);
}

function asCacheMode(raw: string | undefined): CacheMode {
  if (raw === 'once' || raw === 'timed' || raw === 'session') return raw;
  return 'timed';
}

export function EncryptionSettings() {
  const locale = useUIStore((s) => s.locale);
  const [keys, setKeys] = useState<OpengpgKey[]>([]);
  const [accounts, setAccounts] = useState<AccountLite[]>([]);
  const [settings, setSettings] = useState<OpengpgSettings | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  const [importArmor, setImportArmor] = useState('');
  const [importAccountId, setImportAccountId] = useState('');
  /** Account whose Add-Key dialog is open. */
  const [genAccountId, setGenAccountId] = useState<string | null>(null);
  const [genPass, setGenPass] = useState('');
  const [genAlgo, setGenAlgo] = useState<'rsa4096' | 'ed25519'>('ed25519');
  const [search, setSearch] = useState('');
  const [busy, setBusy] = useState(false);

  /** keyId → unlock expiry (local idle clock). */
  const [unlockedUntil, setUnlockedUntil] = useState<Record<string, number>>({});
  const [now, setNow] = useState(() => Date.now());

  const [exportSecretId, setExportSecretId] = useState<string | null>(null);
  const [exportPassword, setExportPassword] = useState('');

  const [unlockState, unlockSend] = useMachine(opengpgUnlockMachine);

  const refresh = useCallback(async () => {
    try {
      const [k, s, a] = await Promise.all([
        listOpengpgKeys(),
        fetchOpengpgSettings(),
        api<AccountLite[]>('/accounts'),
      ]);
      setKeys(k);
      setSettings(s);
      setAccounts(a);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : t(locale, 'settings.encryption.loadError'));
    } finally {
      setLoading(false);
    }
  }, [locale]);

  useEffect(() => {
    // Fetch-on-mount: synchronizing with the server (external system);
    // all setters inside refresh() run after awaits.
    // oxlint-disable-next-line set-state-in-effect
    void refresh();
  }, [refresh]);

  useEffect(() => {
    const id = window.setInterval(() => setNow(Date.now()), 15_000);
    return () => window.clearInterval(id);
  }, []);

  useEffect(() => {
    if (!unlockState.matches('success') || !unlockState.context.result) return;
    const { keyId, cached } = unlockState.context.result;
    // React state reacting to the XState machine's success event — a
    // genuine external-system sync, kept synchronous on purpose.
    if (cached) {
      // oxlint-disable-next-line set-state-in-effect
      setUnlockedUntil((prev) => ({ ...prev, [keyId]: Date.now() + IDLE_MS }));
      // oxlint-disable-next-line set-state-in-effect
      setMessage(t(locale, 'settings.encryption.unlockCached'));
    } else {
      // oxlint-disable-next-line set-state-in-effect
      setMessage(t(locale, 'settings.encryption.unlockOnce'));
    }
    void refresh();
  }, [unlockState, locale, refresh]);

  const prefMode = asCacheMode(settings?.passphraseCache.mode);
  const prefTtl = settings?.passphraseCache.ttlMinutes ?? 10;

  async function savePref(mode: CacheMode, ttlMinutes: number) {
    setBusy(true);
    setError(null);
    try {
      const updated = await updateOpengpgSettings({ mode, ttlMinutes });
      setSettings(updated);
      setMessage(t(locale, 'settings.encryption.prefSaved'));
    } catch (e) {
      setError(e instanceof Error ? e.message : t(locale, 'settings.encryption.saveError'));
    } finally {
      setBusy(false);
    }
  }

  function accountLabel(id: string | null | undefined): string | null {
    if (!id) return null;
    const acct = accounts.find((a) => a.id === id);
    return acct ? acct.emailAddress : null;
  }

  const identityKeys = useMemo(() => {
    const emailById = new Map(accounts.map((a) => [a.id, a.emailAddress]));
    return [...keys]
      .filter((k) => k.accountId)
      .sort((a, b) =>
        (emailById.get(a.accountId ?? '') ?? '').localeCompare(
          emailById.get(b.accountId ?? '') ?? '',
        ),
      );
  }, [keys, accounts]);
  const contactKeys = useMemo(() => keys.filter((k) => !k.accountId), [keys]);

  const filteredIdentity = useMemo(() => filterKeys(identityKeys, search), [identityKeys, search]);
  const filteredContact = useMemo(() => filterKeys(contactKeys, search), [contactKeys, search]);
  const showGroups = filteredIdentity.length > 0 || filteredContact.length > 0;

  async function onImport() {
    if (!importArmor.trim()) return;
    setBusy(true);
    setError(null);
    try {
      await importOpengpgKey(importArmor.trim(), {
        accountId: importAccountId || null,
      });
      setImportArmor('');
      setMessage(t(locale, 'settings.encryption.importOk'));
      await refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : t(locale, 'settings.encryption.importError'));
    } finally {
      setBusy(false);
    }
  }

  function openAddKey(accountId: string) {
    setGenAccountId(accountId);
    setGenPass('');
    setGenAlgo('ed25519');
  }

  async function onGenerate() {
    if (!genAccountId || !genPass) return;
    setBusy(true);
    setError(null);
    try {
      await generateOpengpgKey({
        accountId: genAccountId,
        passphrase: genPass,
        algorithm: genAlgo,
      });
      setMessage(t(locale, 'settings.encryption.generateOk'));
      setGenAccountId(null);
      setGenPass('');
      await refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : t(locale, 'settings.encryption.generateError'));
    } finally {
      setBusy(false);
    }
  }

  async function onPrimary(id: string) {
    setBusy(true);
    setError(null);
    try {
      await setPrimaryOpengpgKey(id);
      await refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : t(locale, 'settings.encryption.saveError'));
    } finally {
      setBusy(false);
    }
  }

  async function onBind(key: OpengpgKey, accountId: string | null) {
    setBusy(true);
    setError(null);
    try {
      await setOpengpgKeyAccount(key.id, accountId);
      setMessage(t(locale, 'settings.encryption.importOk'));
      await refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : t(locale, 'settings.encryption.saveError'));
    } finally {
      setBusy(false);
    }
  }

  async function onDelete(id: string) {
    if (!confirm(t(locale, 'settings.encryption.confirmDelete'))) return;
    setBusy(true);
    setError(null);
    try {
      await deleteOpengpgKey(id);
      setUnlockedUntil((prev) => {
        const next = { ...prev };
        delete next[id];
        return next;
      });
      await refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : t(locale, 'settings.encryption.deleteError'));
    } finally {
      setBusy(false);
    }
  }

  async function onExportPublic(key: OpengpgKey) {
    setBusy(true);
    setError(null);
    try {
      const { armored } = await exportOpengpgKey(key.id);
      downloadArmored(`${shortFp(key.fingerprint)}.asc`, armored);
    } catch (e) {
      setError(e instanceof Error ? e.message : t(locale, 'settings.encryption.exportError'));
    } finally {
      setBusy(false);
    }
  }

  async function onExportSecret() {
    if (!exportSecretId || !exportPassword) return;
    setBusy(true);
    setError(null);
    try {
      const { armored } = await exportOpengpgKey(exportSecretId, {
        includeSecret: true,
        currentPassword: exportPassword,
      });
      const key = keys.find((k) => k.id === exportSecretId);
      downloadArmored(`${shortFp(key?.fingerprint ?? 'secret')}-secret.asc`, armored);
      setExportSecretId(null);
      setExportPassword('');
      setMessage(t(locale, 'settings.encryption.exportSecretOk'));
    } catch (e) {
      setError(e instanceof Error ? e.message : t(locale, 'settings.encryption.exportError'));
    } finally {
      setBusy(false);
    }
  }

  async function onLock(keyId?: string) {
    setBusy(true);
    try {
      await lockOpengpgKeys(keyId);
      if (keyId) {
        setUnlockedUntil((prev) => {
          const next = { ...prev };
          delete next[keyId];
          return next;
        });
      } else {
        setUnlockedUntil({});
      }
      setMessage(t(locale, 'settings.encryption.locked'));
    } catch (e) {
      setError(e instanceof Error ? e.message : t(locale, 'settings.encryption.lockError'));
    } finally {
      setBusy(false);
    }
  }

  function openUnlock(key: OpengpgKey) {
    unlockSend({
      type: 'OPEN',
      keyId: key.id,
      fingerprint: key.fingerprint,
      cache: prefMode,
      ttlMinutes: prefTtl,
    });
  }

  const dialogOpen = !unlockState.matches('closed');

  function renderKeyRow(key: OpengpgKey) {
    const until = unlockedUntil[key.id] ?? 0;
    const unlocked = until > now;
    const minsLeft = unlocked ? Math.max(1, Math.ceil((until - now) / 60_000)) : 0;
    const boundEmail = accountLabel(key.accountId);
    return (
      <li
        key={key.id}
        className="space-y-2 border-t border-border pt-3 first:border-t-0 first:pt-0"
      >
        <div className="flex flex-wrap items-start justify-between gap-2">
          <div>
            <div className="text-[13px] font-medium">{key.primaryEmail}</div>
            <div className="font-mono text-[11px] text-ter-foreground">
              …{shortFp(key.fingerprint)}
            </div>
            <div className="mt-1 flex flex-wrap gap-1.5">
              {boundEmail ? (
                <Badge variant="secondary">
                  {t(locale, 'settings.encryption.boundTo', { email: boundEmail })}
                </Badge>
              ) : (
                <Badge variant="outline">{t(locale, 'settings.encryption.contactChip')}</Badge>
              )}
              {key.isPrimary ? (
                <Badge variant="secondary">{t(locale, 'settings.encryption.primary')}</Badge>
              ) : null}
              {key.isSecret ? (
                <Badge variant="outline">{t(locale, 'settings.encryption.secret')}</Badge>
              ) : (
                <Badge variant="outline">{t(locale, 'settings.encryption.public')}</Badge>
              )}
              {key.revoked ? (
                <Badge variant="outline" className="border-destructive/40 text-destructive">
                  {t(locale, 'settings.encryption.revoked')}
                </Badge>
              ) : null}
              {unlocked ? (
                <Badge variant="secondary" className="gap-1">
                  <Unlock className="size-3" />
                  {t(locale, 'settings.encryption.unlockedFor', {
                    minutes: String(minsLeft),
                  })}
                </Badge>
              ) : null}
            </div>
          </div>
          <div className="flex flex-wrap gap-1.5">
            {key.isSecret ? (
              unlocked ? (
                <Button
                  variant="outline"
                  size="sm"
                  disabled={busy}
                  onClick={() => void onLock(key.id)}
                >
                  {t(locale, 'settings.encryption.lock')}
                </Button>
              ) : (
                <Button
                  variant="secondary"
                  size="sm"
                  disabled={busy}
                  onClick={() => openUnlock(key)}
                >
                  {t(locale, 'settings.encryption.unlock')}
                </Button>
              )
            ) : null}
            {!key.isPrimary ? (
              <Button
                variant="outline"
                size="sm"
                disabled={busy}
                onClick={() => void onPrimary(key.id)}
              >
                {t(locale, 'settings.encryption.makePrimary')}
              </Button>
            ) : null}
            <Select
              value={key.accountId ?? ''}
              disabled={busy}
              onValueChange={(v) => void onBind(key, v === '' ? null : v)}
            >
              <SelectTrigger
                size="sm"
                className="min-w-[150px]"
                aria-label={t(locale, 'settings.encryption.rebindTitle')}
              >
                <SelectValue placeholder={t(locale, 'settings.encryption.rebindContact')} />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="" disabled={key.isSecret}>
                  {t(locale, 'settings.encryption.rebindContact')}
                </SelectItem>
                {accounts.map((a) => (
                  <SelectItem key={a.id} value={a.id}>
                    {a.emailAddress}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <Button
              variant="outline"
              size="sm"
              disabled={busy}
              onClick={() => void onExportPublic(key)}
            >
              {t(locale, 'settings.encryption.exportPublic')}
            </Button>
            {key.isSecret ? (
              <Button
                variant="outline"
                size="sm"
                disabled={busy}
                onClick={() => {
                  setExportSecretId(key.id);
                  setExportPassword('');
                }}
              >
                {t(locale, 'settings.encryption.exportSecret')}
              </Button>
            ) : null}
            <Button
              variant="ghost"
              size="sm"
              disabled={busy || key.isPrimary}
              onClick={() => void onDelete(key.id)}
            >
              {t(locale, 'common.delete')}
            </Button>
          </div>
        </div>
      </li>
    );
  }

  const genAccount = genAccountId ? accounts.find((a) => a.id === genAccountId) : undefined;

  return (
    <div className="space-y-4">
      {error ? <div className="text-sm text-destructive">{error}</div> : null}
      {message ? <div className="text-sm text-muted-foreground">{message}</div> : null}

      <section className="space-y-3 rounded-[10px] border border-border bg-card px-5 py-4">
        <div className="text-[13px] font-medium">
          {t(locale, 'settings.encryption.rememberTitle')}
        </div>
        <p className="text-xs text-ter-foreground">
          {t(locale, 'settings.encryption.rememberHint')}
        </p>
        <div className="flex flex-wrap items-center gap-3">
          <Select
            value={prefMode}
            onValueChange={(v) => void savePref(asCacheMode(v), prefTtl)}
            disabled={busy || !settings}
          >
            <SelectTrigger size="sm" className="min-w-[160px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="once">{t(locale, 'settings.encryption.cache.once')}</SelectItem>
              <SelectItem value="timed">{t(locale, 'settings.encryption.cache.timed')}</SelectItem>
              <SelectItem value="session">
                {t(locale, 'settings.encryption.cache.session')}
              </SelectItem>
            </SelectContent>
          </Select>
          {prefMode === 'timed' ? (
            <Select
              value={String(prefTtl)}
              onValueChange={(v) => void savePref('timed', Number(v))}
              disabled={busy}
            >
              <SelectTrigger size="sm" className="min-w-[120px]">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {[5, 10, 30, 60, 120].map((m) => (
                  <SelectItem key={m} value={String(m)}>
                    {t(locale, 'settings.encryption.ttlMinutes', { minutes: String(m) })}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          ) : null}
        </div>
      </section>

      <section className="space-y-3 rounded-[10px] border border-border bg-card px-5 py-4">
        <div className="flex items-center justify-between gap-2">
          <div className="flex items-center gap-2 text-[13px] font-medium">
            <KeyRound className="size-4" />
            {t(locale, 'settings.encryption.managerTitle')}
          </div>
          <Button
            variant="outline"
            size="sm"
            disabled={busy || Object.keys(unlockedUntil).length === 0}
            onClick={() => void onLock()}
          >
            <Lock className="size-3.5" />
            {t(locale, 'settings.encryption.lockAll')}
          </Button>
        </div>

        <input
          className="h-8 w-full rounded-md border border-input bg-transparent px-2 text-[13px]"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder={t(locale, 'settings.encryption.searchPlaceholder')}
        />

        {loading ? (
          <div className="text-xs text-ter-foreground">{t(locale, 'common.loading')}</div>
        ) : !showGroups ? (
          <div className="text-xs text-ter-foreground">
            {t(locale, 'settings.encryption.empty')}
          </div>
        ) : (
          <div className="space-y-4">
            {filteredIdentity.length > 0 ? (
              <div className="space-y-1">
                <div className="text-[11px] uppercase tracking-wide text-ter-foreground">
                  {t(locale, 'settings.encryption.groupIdentity')}
                </div>
                <ul>{filteredIdentity.map(renderKeyRow)}</ul>
              </div>
            ) : null}
            {filteredContact.length > 0 ? (
              <div className="space-y-1">
                <div className="text-[11px] uppercase tracking-wide text-ter-foreground">
                  {t(locale, 'settings.encryption.groupContact')}
                </div>
                <ul>{filteredContact.map(renderKeyRow)}</ul>
              </div>
            ) : null}
          </div>
        )}
      </section>

      <section className="space-y-3 rounded-[10px] border border-border bg-card px-5 py-4">
        <div className="text-[13px] font-medium">
          {t(locale, 'settings.encryption.accountsTitle')}
        </div>
        <p className="text-xs text-ter-foreground">
          {t(locale, 'settings.encryption.accountsHint')}
        </p>
        <ul className="space-y-2">
          {accounts.map((acct) => {
            const identities = keys.filter((k) => k.isSecret && k.accountId === acct.id);
            const primary = identities.find((k) => k.isPrimary);
            return (
              <li
                key={acct.id}
                className="flex flex-wrap items-center justify-between gap-2 rounded-md border border-border/60 px-3 py-2"
              >
                <div>
                  <div className="text-[13px] font-medium">{acct.emailAddress}</div>
                  <div className="text-xs text-ter-foreground">
                    {primary ? (
                      <>
                        {t(locale, 'settings.encryption.primary')} · …{shortFp(primary.fingerprint)}
                        {identities.length > 1 ? ` (+${identities.length - 1})` : ''}
                      </>
                    ) : (
                      t(locale, 'settings.encryption.noKeyFor', { email: acct.emailAddress })
                    )}
                  </div>
                </div>
                <Button variant="outline" size="sm" onClick={() => openAddKey(acct.id)}>
                  {t(locale, 'settings.encryption.addKey')}
                </Button>
              </li>
            );
          })}
          {accounts.length === 0 ? (
            <li className="text-xs text-ter-foreground">{t(locale, 'settings.accounts.empty')}</li>
          ) : null}
        </ul>
      </section>

      <section className="space-y-3 rounded-[10px] border border-border bg-card px-5 py-4">
        <div className="text-[13px] font-medium">
          {t(locale, 'settings.encryption.importTitle')}
        </div>
        <Textarea
          value={importArmor}
          onChange={(e) => setImportArmor(e.target.value)}
          placeholder={t(locale, 'settings.encryption.importPlaceholder')}
          className="min-h-[100px] font-mono text-xs"
        />
        <label className="block space-y-1 text-xs">
          <span className="text-ter-foreground">
            {t(locale, 'settings.encryption.importTargetLabel')}
          </span>
          <Select value={importAccountId} onValueChange={(v) => setImportAccountId(v)}>
            <SelectTrigger size="sm" className="min-w-[220px]">
              <SelectValue placeholder={t(locale, 'settings.encryption.importNoAccount')} />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="">{t(locale, 'settings.encryption.importNoAccount')}</SelectItem>
              {accounts.map((a) => (
                <SelectItem key={a.id} value={a.id}>
                  {a.emailAddress}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </label>
        <div>
          <Button size="sm" disabled={busy || !importArmor.trim()} onClick={() => void onImport()}>
            {t(locale, 'settings.encryption.import')}
          </Button>
        </div>
        <p className="text-xs text-ter-foreground">
          {t(locale, 'settings.encryption.generateHint')}
        </p>
      </section>

      <Dialog
        open={genAccountId != null}
        onOpenChange={(open) => {
          if (!open) setGenAccountId(null);
        }}
      >
        <DialogContent showCloseButton>
          <DialogHeader>
            <DialogTitle>
              {t(locale, 'settings.encryption.addKey')}
              {genAccount ? ` · ${genAccount.emailAddress}` : ''}
            </DialogTitle>
            <DialogDescription>{t(locale, 'settings.encryption.generateHint')}</DialogDescription>
          </DialogHeader>
          <div className="grid gap-2 sm:grid-cols-2">
            <label className="space-y-1 text-xs sm:col-span-2">
              <span className="text-ter-foreground">
                {t(locale, 'settings.encryption.genPassphrase')}
              </span>
              <input
                className="flex h-9 w-full rounded-md border border-input bg-transparent px-3 text-[13px]"
                value={genPass}
                onChange={(e) => setGenPass(e.target.value)}
                type="password"
                autoComplete="new-password"
              />
            </label>
            <label className="space-y-1 text-xs sm:col-span-2">
              <span className="text-ter-foreground">
                {t(locale, 'settings.encryption.algorithm')}
              </span>
              <Select value={genAlgo} onValueChange={(v) => setGenAlgo(v as 'rsa4096' | 'ed25519')}>
                <SelectTrigger size="sm">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="ed25519">
                    {t(locale, 'settings.encryption.algo.ed25519')}
                  </SelectItem>
                  <SelectItem value="rsa4096">
                    {t(locale, 'settings.encryption.algo.rsa4096')}
                  </SelectItem>
                </SelectContent>
              </Select>
            </label>
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setGenAccountId(null)}>
              {t(locale, 'common.cancel')}
            </Button>
            <Button disabled={busy || !genPass} onClick={() => void onGenerate()}>
              {busy ? t(locale, 'common.loading') : t(locale, 'settings.encryption.generate')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog
        open={dialogOpen}
        onOpenChange={(open) => {
          if (!open) unlockSend({ type: 'CLOSE' });
        }}
      >
        <DialogContent showCloseButton>
          <DialogHeader>
            <DialogTitle>{t(locale, 'settings.encryption.unlockTitle')}</DialogTitle>
            <DialogDescription>
              {unlockState.context.fingerprint
                ? t(locale, 'settings.encryption.unlockDesc', {
                    fingerprint: shortFp(unlockState.context.fingerprint),
                  })
                : t(locale, 'settings.encryption.unlockHint')}
            </DialogDescription>
          </DialogHeader>

          {unlockState.matches('success') ? (
            <p className="text-sm text-muted-foreground">
              {t(locale, 'settings.encryption.unlockSuccess')}
            </p>
          ) : (
            <div className="space-y-3">
              <label className="space-y-1 text-xs">
                <span className="text-ter-foreground">
                  {t(locale, 'settings.encryption.passphrase')}
                </span>
                <input
                  className="flex h-9 w-full rounded-md border border-input bg-transparent px-3 text-[13px]"
                  type="password"
                  autoComplete="current-password"
                  value={unlockState.context.passphrase}
                  disabled={unlockState.matches('unlocking')}
                  onChange={(e) => unlockSend({ type: 'SET_PASSPHRASE', value: e.target.value })}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') unlockSend({ type: 'SUBMIT' });
                  }}
                />
              </label>
              <label className="space-y-1 text-xs">
                <span className="text-ter-foreground">
                  {t(locale, 'settings.encryption.rememberChoice')}
                </span>
                <Select
                  value={unlockState.context.cache}
                  disabled={unlockState.matches('unlocking')}
                  onValueChange={(v) => unlockSend({ type: 'SET_CACHE', value: asCacheMode(v) })}
                >
                  <SelectTrigger size="sm">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="once">
                      {t(locale, 'settings.encryption.cache.once')}
                    </SelectItem>
                    <SelectItem value="timed">
                      {t(locale, 'settings.encryption.cache.timed')}
                    </SelectItem>
                    <SelectItem value="session">
                      {t(locale, 'settings.encryption.cache.session')}
                    </SelectItem>
                  </SelectContent>
                </Select>
              </label>
              {unlockState.context.error ? (
                <div className="text-sm text-destructive">{unlockState.context.error}</div>
              ) : null}
            </div>
          )}

          <DialogFooter>
            <Button variant="outline" onClick={() => unlockSend({ type: 'CLOSE' })}>
              {t(locale, 'common.cancel')}
            </Button>
            {!unlockState.matches('success') ? (
              <Button
                disabled={unlockState.matches('unlocking') || !unlockState.context.passphrase}
                onClick={() => unlockSend({ type: 'SUBMIT' })}
              >
                {unlockState.matches('unlocking')
                  ? t(locale, 'settings.encryption.unlocking')
                  : t(locale, 'settings.encryption.unlock')}
              </Button>
            ) : null}
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog
        open={exportSecretId != null}
        onOpenChange={(open) => {
          if (!open) {
            setExportSecretId(null);
            setExportPassword('');
          }
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t(locale, 'settings.encryption.exportSecretTitle')}</DialogTitle>
            <DialogDescription>
              {t(locale, 'settings.encryption.exportSecretDesc')}
            </DialogDescription>
          </DialogHeader>
          <label className="space-y-1 text-xs">
            <span className="text-ter-foreground">
              {t(locale, 'settings.security.currentPassword')}
            </span>
            <input
              className="flex h-9 w-full rounded-md border border-input bg-transparent px-3 text-[13px]"
              type="password"
              autoComplete="current-password"
              value={exportPassword}
              onChange={(e) => setExportPassword(e.target.value)}
            />
          </label>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => {
                setExportSecretId(null);
                setExportPassword('');
              }}
            >
              {t(locale, 'common.cancel')}
            </Button>
            <Button disabled={busy || !exportPassword} onClick={() => void onExportSecret()}>
              {t(locale, 'settings.encryption.exportSecret')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

/** Case-insensitive substring match over the fields users scan in a key list. */
function filterKeys(list: OpengpgKey[], query: string): OpengpgKey[] {
  const q = query.trim().toLowerCase();
  if (!q) return list;
  return list.filter(
    (k) =>
      k.primaryEmail.toLowerCase().includes(q) ||
      k.fingerprint.toLowerCase().includes(q.replace(/\s/g, '')) ||
      k.emails.some((e) => e.toLowerCase().includes(q)),
  );
}
