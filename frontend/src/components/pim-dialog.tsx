/**
 * Per-account Calendar & Contacts (CardDAV/CalDAV) setup dialog.
 *
 * Replaces the old inline "PIM password" input + raw Discover/Sync dropdown:
 * one guided Connect flow (save password → RFC 6764 discovery → sync both
 * services), explicit connected/not-connected state per service, manual URL
 * override for exotic providers, and a real Disconnect.
 */

import { CalendarDays, ChevronDown, ChevronRight, Contact, KeyRound } from 'lucide-react';
import { ThinkingOrb } from 'thinking-orbs';
import { useState } from 'react';

import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import { Field, FieldDescription, FieldGroup, FieldLabel } from '@/components/ui/field';
import { Input } from '@/components/ui/input';
import { Separator } from '@/components/ui/separator';
import { t } from '../i18n';
import { api } from '@/lib/api-client';
import { normalizeDavUrlInput, pimProviderFor, providerHintKey } from '@/lib/pim-setup';
import { cn } from '@/lib/utils';
import { useUIStore } from '@/stores/ui';

/** Structural slice of the settings-page MailAccount. */
export interface PimDialogAccount {
  id: string;
  emailAddress: string;
  hasPimCredential?: boolean;
  carddavUrl?: string | null;
  caldavUrl?: string | null;
}

interface PimDialogProps {
  account: PimDialogAccount | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Refresh the parent's account list (credential/URL fields changed). */
  onChanged: () => void;
}

type BusyStage =
  | 'savingPassword'
  | 'discovering'
  | 'syncingContacts'
  | 'syncingCalendars'
  | 'savingUrls'
  | 'disconnecting';

interface SyncSummary {
  contacts?: string;
  calendars?: string;
}

interface ServiceStatus {
  icon: typeof Contact;
  label: string;
  url?: string | null;
}

function ServiceRow({ icon: Icon, label, url }: ServiceStatus) {
  const locale = useUIStore((s) => s.locale);
  return (
    <div className="flex items-center gap-2.5">
      <span className="flex size-7 shrink-0 items-center justify-center rounded-md bg-accent text-muted-foreground">
        <Icon className="size-3.5" />
      </span>
      <span className="w-16 shrink-0 text-[13px] font-medium">{label}</span>
      {url ? (
        <span className="flex min-w-0 items-center gap-1.5">
          <span className="size-1.5 shrink-0 rounded-full bg-ok" />
          <span
            className="min-w-0 truncate font-mono text-[11.5px] text-muted-foreground"
            title={url}
          >
            {url}
          </span>
        </span>
      ) : (
        <span className="text-[11.5px] text-muted-foreground/70">
          {t(locale, 'settings.pim.notConnected')}
        </span>
      )}
    </div>
  );
}

interface PimDialogFormProps {
  account: PimDialogAccount;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onChanged: () => void;
}

export function PimDialog({ account, open, onOpenChange, onChanged }: PimDialogProps) {
  if (!account) return null;
  // The dialog stays mounted in the settings page, so the form below is
  // keyed: every open cycle starts from the account's current server-side
  // state (URLs prefill, password cleared) without reset effects.
  return (
    <PimDialogForm
      key={`${account.id}:${open}`}
      account={account}
      open={open}
      onOpenChange={onOpenChange}
      onChanged={onChanged}
    />
  );
}

function PimDialogForm({ account, open, onOpenChange, onChanged }: PimDialogFormProps) {
  const locale = useUIStore((s) => s.locale);
  const [password, setPassword] = useState('');
  const [busy, setBusy] = useState<BusyStage | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [summary, setSummary] = useState<SyncSummary | null>(null);
  const [advanced, setAdvanced] = useState(false);
  const [carddavUrl, setCarddavUrl] = useState(account.carddavUrl ?? '');
  const [caldavUrl, setCaldavUrl] = useState(account.caldavUrl ?? '');

  const provider = pimProviderFor(account.emailAddress);

  async function putAccount(body: Record<string, unknown>): Promise<void> {
    await api(`/accounts/${account.id}`, { method: 'PUT', body: JSON.stringify(body) });
  }

  async function syncService(
    service: 'contacts' | 'calendars',
    stage: BusyStage,
  ): Promise<string | undefined> {
    setBusy(stage);
    const res = await api<{ status?: string; synced?: number; removed?: number }>(
      `/accounts/${account.id}/${service}/sync`,
    );
    if (res.status === 'skipped') return undefined;
    return t(locale, `settings.pim.summary${service === 'contacts' ? 'Contacts' : 'Calendars'}`, {
      synced: res.synced ?? 0,
      removed: res.removed ?? 0,
    });
  }

  /** One-click setup: save app password → discover → sync both services. */
  async function connect(): Promise<void> {
    const pw = password.trim();
    setError(null);
    setNotice(null);
    setSummary(null);
    try {
      if (pw) {
        setBusy('savingPassword');
        await putAccount({ pimPassword: pw });
        setPassword('');
      }
      // No password yet is fine — the server falls back to the mail
      // credential and reports "PIM app password required" when it can't.
      setBusy('discovering');
      const disc = await api<{ carddavUrl?: string | null; caldavUrl?: string | null }>(
        `/accounts/${account.id}/pim/discover`,
        { method: 'POST' },
      );
      const next: SyncSummary = {};
      if (disc.carddavUrl) {
        const line = await syncService('contacts', 'syncingContacts');
        if (line) next.contacts = line;
      }
      if (disc.caldavUrl) {
        const line = await syncService('calendars', 'syncingCalendars');
        if (line) next.calendars = line;
      }
      setSummary(next);
      if (!disc.carddavUrl && !disc.caldavUrl) {
        setNotice(t(locale, 'settings.pim.discoveryNone'));
        setAdvanced(true);
      }
      onChanged();
    } catch (err: unknown) {
      const message = err instanceof Error ? err.message : String(err);
      setError(
        /pim app password required/i.test(message)
          ? t(locale, 'settings.pim.passwordMissing')
          : message,
      );
    } finally {
      setBusy(null);
    }
  }

  async function syncNow(): Promise<void> {
    setError(null);
    setNotice(null);
    setSummary(null);
    try {
      const next: SyncSummary = {};
      if (account.carddavUrl) {
        const line = await syncService('contacts', 'syncingContacts');
        if (line) next.contacts = line;
      }
      if (account.caldavUrl) {
        const line = await syncService('calendars', 'syncingCalendars');
        if (line) next.calendars = line;
      }
      setSummary(next);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(null);
    }
  }

  async function saveUrls(): Promise<void> {
    const carddav = normalizeDavUrlInput(carddavUrl);
    const caldav = normalizeDavUrlInput(caldavUrl);
    if (!carddav.ok) {
      setError(t(locale, carddav.errorKey));
      return;
    }
    if (!caldav.ok) {
      setError(t(locale, caldav.errorKey));
      return;
    }
    setError(null);
    setNotice(null);
    try {
      setBusy('savingUrls');
      await putAccount({ carddavUrl: carddav.value, caldavUrl: caldav.value });
      setNotice(t(locale, 'settings.pim.urlsSaved'));
      onChanged();
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(null);
    }
  }

  /** Clear the app password AND homesets — the mail-password fallback keeps
   * DAV working otherwise, so both must go to actually disconnect. */
  async function disconnect(): Promise<void> {
    setError(null);
    setNotice(null);
    setSummary(null);
    try {
      setBusy('disconnecting');
      await putAccount({ clearPimPassword: true, carddavUrl: '', caldavUrl: '' });
      setPassword('');
      setNotice(t(locale, 'settings.pim.disconnected'));
      onChanged();
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(null);
    }
  }

  const hasAnyConnection =
    !!account.hasPimCredential || !!account.carddavUrl || !!account.caldavUrl;
  const connectStages: BusyStage[] = [
    'savingPassword',
    'discovering',
    'syncingContacts',
    'syncingCalendars',
  ];
  const connectLabel =
    busy && connectStages.includes(busy)
      ? t(locale, `settings.pim.${busy}`)
      : t(locale, 'settings.pim.connect');

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[calc(100dvh-2rem)] overflow-y-auto sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{t(locale, 'settings.pim.title')}</DialogTitle>
          <DialogDescription>
            {t(locale, 'settings.pim.subtitle', { email: account.emailAddress })}
          </DialogDescription>
        </DialogHeader>

        {/* min-w-0: grid items default to min-width auto, which would push
            the dialog track past its max-w-md cap on long DAV URLs. */}
        <FieldGroup className="min-w-0">
          {/* Per-service connection state */}
          <div className="flex flex-col gap-2 rounded-[10px] border border-border/70 bg-accent/30 px-3 py-2.5">
            <ServiceRow
              icon={Contact}
              label={t(locale, 'settings.pim.contacts')}
              url={account.carddavUrl}
            />
            <ServiceRow
              icon={CalendarDays}
              label={t(locale, 'settings.pim.calendars')}
              url={account.caldavUrl}
            />
            <p className="pl-[38px] text-[11px] text-muted-foreground">
              {account.hasPimCredential
                ? t(locale, 'settings.pim.passwordSaved')
                : t(locale, 'settings.pim.noPassword')}
            </p>
          </div>

          {/* App password + guided connect */}
          <Field>
            <FieldLabel htmlFor="pim-password">
              {t(locale, 'settings.pim.passwordLabel')}
            </FieldLabel>
            <div className="flex gap-2">
              <Input
                id="pim-password"
                type="password"
                autoComplete="off"
                spellCheck={false}
                className="flex-1"
                value={password}
                placeholder={
                  account.hasPimCredential
                    ? '••••••••••'
                    : t(locale, 'settings.pim.passwordPlaceholder')
                }
                onChange={(e) => setPassword(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter' && !busy) void connect();
                }}
              />
              <Button type="button" disabled={busy !== null} onClick={() => void connect()}>
                {busy && connectStages.includes(busy) ? (
                  <ThinkingOrb state="connecting" size={20} className="size-3.5" />
                ) : null}
                {connectLabel}
              </Button>
            </div>
            <FieldDescription className="flex items-start gap-1.5">
              <KeyRound className="mt-0.5 size-3 shrink-0" />
              <span>{t(locale, providerHintKey(provider))}</span>
            </FieldDescription>
          </Field>

          {/* Result feedback */}
          {summary?.contacts || summary?.calendars ? (
            <div role="status" className="flex flex-col gap-0.5 text-[12px]">
              {summary.contacts ? <span className="text-ok">{summary.contacts}</span> : null}
              {summary.calendars ? <span className="text-ok">{summary.calendars}</span> : null}
            </div>
          ) : null}
          {notice ? (
            <p role="status" className="text-[12px] text-muted-foreground">
              {notice}
            </p>
          ) : null}
          {error ? (
            <p role="alert" className="text-[12px] text-destructive">
              {error}
            </p>
          ) : null}
        </FieldGroup>

        <Separator />

        {/* Manual server URLs for providers without discovery */}
        <div className="flex min-w-0 flex-col gap-3">
          <button
            type="button"
            className="flex items-center gap-1 text-[12.5px] text-muted-foreground hover:text-foreground"
            onClick={() => setAdvanced((v) => !v)}
          >
            {advanced ? (
              <ChevronDown className="size-3.5" />
            ) : (
              <ChevronRight className="size-3.5" />
            )}
            {t(locale, 'settings.pim.advanced')}
          </button>
          {advanced ? (
            <FieldGroup className="min-w-0">
              <Field>
                <FieldLabel htmlFor="pim-carddav-url" className="text-[13px]">
                  {t(locale, 'settings.pim.carddavUrlLabel')}
                </FieldLabel>
                <Input
                  id="pim-carddav-url"
                  spellCheck={false}
                  className="font-mono text-[12.5px]"
                  value={carddavUrl}
                  placeholder="https://dav.example.com/"
                  onChange={(e) => setCarddavUrl(e.target.value)}
                />
              </Field>
              <Field>
                <FieldLabel htmlFor="pim-caldav-url" className="text-[13px]">
                  {t(locale, 'settings.pim.caldavUrlLabel')}
                </FieldLabel>
                <Input
                  id="pim-caldav-url"
                  spellCheck={false}
                  className="font-mono text-[12.5px]"
                  value={caldavUrl}
                  placeholder="https://dav.example.com/"
                  onChange={(e) => setCaldavUrl(e.target.value)}
                />
              </Field>
              <p className="text-[11.5px] text-muted-foreground">
                {t(locale, 'settings.pim.advancedHint')}
              </p>
              <Button
                type="button"
                variant="outline"
                size="sm"
                className="self-start"
                disabled={busy !== null}
                onClick={() => void saveUrls()}
              >
                {busy === 'savingUrls' ? (
                  <ThinkingOrb state="working" size={20} className="size-3.5" />
                ) : null}
                {t(locale, 'settings.pim.saveUrls')}
              </Button>
            </FieldGroup>
          ) : null}
        </div>

        <DialogFooter className="min-w-0 sm:justify-between">
          {hasAnyConnection ? (
            <Button
              type="button"
              variant="ghost"
              className="text-destructive hover:text-destructive"
              disabled={busy !== null}
              onClick={() => void disconnect()}
            >
              {busy === 'disconnecting' ? (
                <ThinkingOrb state="working" size={20} className="size-3.5" />
              ) : null}
              {t(locale, 'settings.pim.disconnect')}
            </Button>
          ) : (
            <span />
          )}
          <Button
            type="button"
            variant="outline"
            className={cn(!hasAnyConnection && 'invisible')}
            disabled={busy !== null || (!account.carddavUrl && !account.caldavUrl)}
            onClick={() => void syncNow()}
          >
            {busy === 'syncingContacts' || busy === 'syncingCalendars' ? (
              <ThinkingOrb state="working" size={20} className="size-3.5" />
            ) : null}
            {t(locale, 'settings.pim.syncNow')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
