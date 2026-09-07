/**
 * Settings → General: notifications (in-app banners + background push) and
 * the install card.
 *
 * Notifications need a user gesture to request browser permission, so the
 * switch both flips the preference and asks (once). The install card adapts
 * to platform: deferred prompt button (Chromium), manual Share → Home
 * Screen steps (iOS Safari), or "already installed" in standalone mode.
 */

import { useEffect, useState } from 'react';
import { Bell, BellRing, Download } from 'lucide-react';

import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Switch } from '@/components/ui/switch';
import { t } from '@/i18n';
import { api } from '@/lib/api-client';
import {
  notificationPermission,
  readNotificationPrefs,
  requestNotificationPermission,
  sendTestNotification,
  writeNotificationPrefs,
} from '@/lib/notifications';
import {
  pushStatus,
  subscribePush,
  syncPushPrefs,
  unsubscribePush,
  type PushStatus,
} from '@/lib/push';
import { isIos, isStandalone, promptInstall, useInstallAvailable } from '@/lib/pwa';
import { useUIStore } from '@/stores/ui';

export function NotificationSettings() {
  const locale = useUIStore((s) => s.locale);
  const [enabled, setEnabled] = useState(() => readNotificationPrefs().enabled);
  const [permission, setPermission] = useState(notificationPermission());
  const [busy, setBusy] = useState(false);

  const installable = useInstallAvailable();
  const standalone = isStandalone();
  const ios = isIos();

  const [push, setPush] = useState<PushStatus>('unsubscribed');
  const [pushBusy, setPushBusy] = useState(false);
  const [pushTestResult, setPushTestResult] = useState<string | null>(null);

  useEffect(() => {
    // Never reject: 'unsubscribed' is the safe default when status lookup fails.
    void pushStatus()
      .then(setPush)
      .catch(() => {});
  }, []);

  const handlePushToggle = async (next: boolean) => {
    setPushBusy(true);
    try {
      if (!next) {
        await unsubscribePush();
        setPush('unsubscribed');
        return;
      }
      const granted = await requestNotificationPermission();
      setPermission(granted);
      if (granted !== 'granted') {
        setPush(granted === 'denied' ? 'denied' : 'unsubscribed');
        return;
      }
      try {
        await subscribePush();
      } catch {
        // Subscribe failed (network, iOS non-standalone rejection, …): stay
        // unsubscribed and say so instead of leaking an unhandled rejection.
        setPush('unsubscribed');
        setPushTestResult(t(locale, 'settings.notifications.push.testFailed'));
        return;
      }
      const prefs = readNotificationPrefs();
      await syncPushPrefs({
        mutedFolderIds: prefs.mutedFolderIds,
        mutedThreadIds: prefs.mutedThreadIds,
        locale,
      });
      setPush('subscribed');
    } finally {
      setPushBusy(false);
    }
  };

  const handlePushTest = async () => {
    setPushBusy(true);
    setPushTestResult(null);
    try {
      const res = await api<{ sent: number; removed: number }>('/push/test', { method: 'POST' });
      setPushTestResult(t(locale, 'settings.notifications.push.testResult', { sent: res.sent }));
    } catch {
      setPushTestResult(t(locale, 'settings.notifications.push.testFailed'));
    } finally {
      setPushBusy(false);
    }
  };

  const handleToggle = async (next: boolean) => {
    if (!next) {
      setEnabled(false);
      writeNotificationPrefs({ ...readNotificationPrefs(), enabled: false });
      return;
    }
    setBusy(true);
    try {
      const granted = await requestNotificationPermission();
      setPermission(granted);
      if (granted === 'granted') {
        setEnabled(true);
        writeNotificationPrefs({ ...readNotificationPrefs(), enabled: true });
      }
      // denied/default → leave off; hint below explains how to unblock.
    } finally {
      setBusy(false);
    }
  };

  const handleTest = async () => {
    setBusy(true);
    try {
      await sendTestNotification(locale);
    } finally {
      setBusy(false);
    }
  };

  const handleInstall = async () => {
    setBusy(true);
    try {
      await promptInstall();
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <section className="space-y-3 rounded-[10px] border border-border bg-card px-5 py-4">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div className="flex items-start gap-2.5">
            <Bell className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />
            <div>
              <div className="text-[13px] font-medium">
                {t(locale, 'settings.notifications.title')}
              </div>
              <div className="text-xs text-muted-foreground">
                {t(locale, 'settings.notifications.hint')}
              </div>
            </div>
          </div>
          <div className="flex items-center gap-2">
            {enabled && permission === 'granted' ? (
              <Button variant="outline" size="sm" disabled={busy} onClick={() => void handleTest()}>
                {t(locale, 'settings.notifications.test')}
              </Button>
            ) : null}
            <Switch
              checked={enabled && permission === 'granted'}
              disabled={busy || permission === 'denied' || permission === 'unsupported'}
              onCheckedChange={(v) => void handleToggle(v)}
              aria-label={t(locale, 'settings.notifications.title')}
            />
          </div>
        </div>
        {permission === 'denied' ? (
          <p className="text-xs text-muted-foreground">
            {t(locale, 'settings.notifications.denied')}
          </p>
        ) : null}
        {permission === 'unsupported' ? (
          <p className="text-xs text-muted-foreground">
            {t(locale, 'settings.notifications.unsupported')}
          </p>
        ) : null}
        {enabled && permission === 'granted' ? (
          <p className="text-xs text-muted-foreground">
            {t(locale, 'settings.notifications.runningNote')}
          </p>
        ) : null}
        <div className="flex flex-wrap items-center justify-between gap-3 border-t border-border pt-3">
          <div className="flex items-start gap-2.5">
            <BellRing className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />
            <div>
              <div className="text-[13px] font-medium">
                {t(locale, 'settings.notifications.push.title')}
              </div>
              <div className="text-xs text-muted-foreground">
                {t(locale, 'settings.notifications.push.hint')}
              </div>
            </div>
          </div>
          <div className="flex items-center gap-2">
            {push === 'subscribed' ? (
              <Button
                variant="outline"
                size="sm"
                disabled={pushBusy}
                onClick={() => void handlePushTest()}
              >
                {t(locale, 'settings.notifications.push.test')}
              </Button>
            ) : null}
            <Switch
              checked={push === 'subscribed'}
              disabled={pushBusy || push === 'unsupported' || push === 'denied'}
              onCheckedChange={(v) => void handlePushToggle(v)}
              aria-label={t(locale, 'settings.notifications.push.title')}
            />
          </div>
        </div>
        {push === 'unsupported' ? (
          <p className="text-xs text-muted-foreground">
            {t(locale, 'settings.notifications.push.unsupported')}
          </p>
        ) : null}
        {push === 'denied' ? (
          <p className="text-xs text-muted-foreground">
            {t(locale, 'settings.notifications.denied')}
          </p>
        ) : null}
        {ios && !standalone ? (
          <p className="text-xs text-muted-foreground">
            {t(locale, 'settings.notifications.push.iosHint')}
          </p>
        ) : null}
        {pushTestResult ? <p className="text-xs text-muted-foreground">{pushTestResult}</p> : null}
      </section>

      <section className="space-y-3 rounded-[10px] border border-border bg-card px-5 py-4">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div className="flex items-start gap-2.5">
            <Download className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />
            <div>
              <div className="flex items-center gap-2 text-[13px] font-medium">
                {t(locale, 'settings.install.title')}
                {standalone ? (
                  <Badge variant="outline" className="text-[10.5px] font-normal">
                    {t(locale, 'settings.install.installed')}
                  </Badge>
                ) : null}
              </div>
              <div className="text-xs text-muted-foreground">
                {t(locale, 'settings.install.hint')}
              </div>
            </div>
          </div>
          {!standalone && installable ? (
            <Button
              variant="outline"
              size="sm"
              disabled={busy}
              onClick={() => void handleInstall()}
            >
              {t(locale, 'settings.install.button')}
            </Button>
          ) : null}
        </div>
        {!standalone && !installable && ios ? (
          <p className="text-xs text-muted-foreground">{t(locale, 'settings.install.iosHint')}</p>
        ) : null}
      </section>
    </>
  );
}
