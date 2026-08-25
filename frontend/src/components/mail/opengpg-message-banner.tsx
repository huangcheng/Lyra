/**
 * Reading-pane OpenGPG status: lock/shield badge, signature line, unlock when locked.
 */

import { useEffect, useState } from 'react';
import { useMachine } from '@xstate/react';
import { Lock, ShieldCheck, ShieldAlert } from 'lucide-react';

import { Button } from '@/components/ui/button';
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
import { t, type SupportedLocale } from '@/i18n';
import {
  listOpengpgKeys,
  fetchOpengpgSettings,
  type CacheMode,
  type OpengpgKey,
} from '@/lib/opengpg-api';
import { opengpgUnlockMachine } from '@/machines/opengpg-unlock';
import type { MailOpengpgStatus } from '@/types';

function asCacheMode(v: string): CacheMode {
  if (v === 'once' || v === 'timed' || v === 'session') return v;
  return 'timed';
}

function shortFp(fp: string): string {
  const clean = fp.replace(/\s+/g, '');
  return clean.length > 8 ? clean.slice(-8) : clean;
}

export function OpengpgMessageBanner({
  locale,
  status,
  onUnlocked,
}: {
  locale: SupportedLocale;
  status: MailOpengpgStatus;
  onUnlocked: () => void;
}) {
  const [unlockState, unlockSend] = useMachine(opengpgUnlockMachine);
  const [secretKeys, setSecretKeys] = useState<OpengpgKey[]>([]);
  const [prefMode, setPrefMode] = useState<CacheMode>('timed');
  const [prefTtl, setPrefTtl] = useState(10);

  useEffect(() => {
    void listOpengpgKeys()
      .then((keys) => setSecretKeys(keys.filter((k) => k.isSecret && !k.revoked)))
      .catch(() => setSecretKeys([]));
    void fetchOpengpgSettings()
      .then((s) => {
        setPrefMode(asCacheMode(s.passphraseCache.mode));
        setPrefTtl(s.passphraseCache.ttlMinutes);
      })
      .catch(() => {});
  }, []);

  useEffect(() => {
    if (!unlockState.matches('success')) return;
    onUnlocked();
    unlockSend({ type: 'CLOSE' });
  }, [unlockState, onUnlocked, unlockSend]);

  const locked = status.error === 'locked';
  const Icon = status.decrypted
    ? ShieldCheck
    : locked || status.encrypted
      ? Lock
      : status.signatures.some((s) => s.valid)
        ? ShieldCheck
        : ShieldAlert;

  const sigLine = status.signatures.find((s) => s.valid) ?? status.signatures[0];
  let statusText: string;
  if (locked) {
    statusText = t(locale, 'mail.opengpg.locked');
  } else if (status.decrypted) {
    statusText = t(locale, 'mail.opengpg.decrypted');
  } else if (status.encrypted && status.error) {
    statusText = t(locale, 'mail.opengpg.decryptFailed');
  } else if (sigLine?.valid) {
    statusText = t(locale, 'mail.opengpg.sigValid', {
      email: sigLine.email ?? shortFp(sigLine.fingerprint),
    });
  } else if (status.signatures.length > 0) {
    statusText = t(locale, 'mail.opengpg.sigInvalid');
  } else if (status.encrypted) {
    statusText = t(locale, 'mail.opengpg.encrypted');
  } else {
    statusText = t(locale, 'mail.opengpg.present');
  }

  function openUnlock() {
    const key = secretKeys.find((k) => k.isPrimary) ?? secretKeys[0];
    if (!key) return;
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
    <>
      <div className="px-4 pt-3">
        <div className="flex items-center gap-2 rounded-lg border border-border px-3.5 py-2.5 text-[12.5px] text-muted-foreground">
          <Icon className="size-3.5 shrink-0" aria-hidden />
          <span className="min-w-0 flex-1">{statusText}</span>
          {locked && secretKeys.length > 0 ? (
            <Button
              type="button"
              variant="outline"
              size="sm"
              className="shrink-0"
              onClick={openUnlock}
            >
              {t(locale, 'mail.opengpg.unlock')}
            </Button>
          ) : null}
        </div>
      </div>

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
          <DialogFooter>
            <Button variant="outline" onClick={() => unlockSend({ type: 'CLOSE' })}>
              {t(locale, 'common.cancel')}
            </Button>
            <Button
              disabled={unlockState.matches('unlocking') || !unlockState.context.passphrase}
              onClick={() => unlockSend({ type: 'SUBMIT' })}
            >
              {unlockState.matches('unlocking')
                ? t(locale, 'settings.encryption.unlocking')
                : t(locale, 'settings.encryption.unlock')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
