/**
 * Settings → Encryption: OpenGPG key list, import/generate/export/primary,
 * unlock dialog (XState) with remember-choice, idle-relock indicator.
 */

import { useCallback, useEffect, useState } from 'react';
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
  setPrimaryOpengpgKey,
  updateOpengpgSettings,
} from '@/lib/opengpg-api';

/** Match backend IDLE_TIMEOUT (10 minutes). */
const IDLE_MS = 10 * 60 * 1000;

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
  const [settings, setSettings] = useState<OpengpgSettings | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  const [importArmor, setImportArmor] = useState('');
  const [genEmail, setGenEmail] = useState('');
  const [genName, setGenName] = useState('');
  const [genPass, setGenPass] = useState('');
  const [genAlgo, setGenAlgo] = useState<'rsa4096' | 'ed25519'>('ed25519');
  const [busy, setBusy] = useState(false);

  /** keyId → unlock expiry (local idle clock). */
  const [unlockedUntil, setUnlockedUntil] = useState<Record<string, number>>({});
  const [now, setNow] = useState(() => Date.now());

  const [exportSecretId, setExportSecretId] = useState<string | null>(null);
  const [exportPassword, setExportPassword] = useState('');

  const [unlockState, unlockSend] = useMachine(opengpgUnlockMachine);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [k, s] = await Promise.all([listOpengpgKeys(), fetchOpengpgSettings()]);
      setKeys(k);
      setSettings(s);
    } catch (e) {
      setError(e instanceof Error ? e.message : t(locale, 'settings.encryption.loadError'));
    } finally {
      setLoading(false);
    }
  }, [locale]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    const id = window.setInterval(() => setNow(Date.now()), 15_000);
    return () => window.clearInterval(id);
  }, []);

  useEffect(() => {
    if (!unlockState.matches('success') || !unlockState.context.result) return;
    const { keyId, cached } = unlockState.context.result;
    if (cached) {
      setUnlockedUntil((prev) => ({ ...prev, [keyId]: Date.now() + IDLE_MS }));
      setMessage(t(locale, 'settings.encryption.unlockCached'));
    } else {
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

  async function onImport() {
    if (!importArmor.trim()) return;
    setBusy(true);
    setError(null);
    try {
      await importOpengpgKey(importArmor.trim(), keys.length === 0);
      setImportArmor('');
      setMessage(t(locale, 'settings.encryption.importOk'));
      await refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : t(locale, 'settings.encryption.importError'));
    } finally {
      setBusy(false);
    }
  }

  async function onGenerate() {
    if (!genEmail.trim() || !genPass) return;
    setBusy(true);
    setError(null);
    try {
      await generateOpengpgKey({
        email: genEmail.trim(),
        name: genName.trim(),
        passphrase: genPass,
        algorithm: genAlgo,
      });
      setGenPass('');
      setMessage(t(locale, 'settings.encryption.generateOk'));
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
            {t(locale, 'settings.encryption.keysTitle')}
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

        {loading ? (
          <div className="text-xs text-ter-foreground">{t(locale, 'common.loading')}</div>
        ) : keys.length === 0 ? (
          <div className="text-xs text-ter-foreground">
            {t(locale, 'settings.encryption.empty')}
          </div>
        ) : (
          <ul className="space-y-3">
            {keys.map((key) => {
              const until = unlockedUntil[key.id] ?? 0;
              const unlocked = until > now;
              const minsLeft = unlocked ? Math.max(1, Math.ceil((until - now) / 60_000)) : 0;
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
                        {key.isPrimary ? (
                          <Badge variant="secondary">
                            {t(locale, 'settings.encryption.primary')}
                          </Badge>
                        ) : null}
                        {key.isSecret ? (
                          <Badge variant="outline">{t(locale, 'settings.encryption.secret')}</Badge>
                        ) : (
                          <Badge variant="outline">{t(locale, 'settings.encryption.public')}</Badge>
                        )}
                        {key.revoked ? (
                          <Badge
                            variant="outline"
                            className="border-destructive/40 text-destructive"
                          >
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
            })}
          </ul>
        )}
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
        <Button size="sm" disabled={busy || !importArmor.trim()} onClick={() => void onImport()}>
          {t(locale, 'settings.encryption.import')}
        </Button>
      </section>

      <section className="space-y-3 rounded-[10px] border border-border bg-card px-5 py-4">
        <div className="text-[13px] font-medium">
          {t(locale, 'settings.encryption.generateTitle')}
        </div>
        <p className="text-xs text-ter-foreground">
          {t(locale, 'settings.encryption.generateHint')}
        </p>
        <div className="grid gap-2 sm:grid-cols-2">
          <label className="space-y-1 text-xs">
            <span className="text-ter-foreground">{t(locale, 'settings.encryption.genEmail')}</span>
            <input
              className="flex h-8 w-full rounded-md border border-input bg-transparent px-2 text-[13px]"
              value={genEmail}
              onChange={(e) => setGenEmail(e.target.value)}
              type="email"
              autoComplete="email"
            />
          </label>
          <label className="space-y-1 text-xs">
            <span className="text-ter-foreground">{t(locale, 'settings.encryption.genName')}</span>
            <input
              className="flex h-8 w-full rounded-md border border-input bg-transparent px-2 text-[13px]"
              value={genName}
              onChange={(e) => setGenName(e.target.value)}
              type="text"
              autoComplete="name"
            />
          </label>
          <label className="space-y-1 text-xs sm:col-span-2">
            <span className="text-ter-foreground">
              {t(locale, 'settings.encryption.genPassphrase')}
            </span>
            <input
              className="flex h-8 w-full rounded-md border border-input bg-transparent px-2 text-[13px]"
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
        <Button
          size="sm"
          disabled={busy || !genEmail.trim() || !genPass}
          onClick={() => void onGenerate()}
        >
          {t(locale, 'settings.encryption.generate')}
        </Button>
      </section>

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
