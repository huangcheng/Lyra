/**
 * Settings → General: notifications + install card.
 *
 * Notifications need a user gesture to request browser permission, so the
 * switch both flips the preference and asks (once). The install card adapts
 * to platform: deferred prompt button (Chromium), manual Share → Home
 * Screen steps (iOS Safari), or "already installed" in standalone mode.
 */

import { useState } from 'react';
import { Bell, Download } from 'lucide-react';

import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Switch } from '@/components/ui/switch';
import { t } from '@/i18n';
import {
  notificationPermission,
  readNotificationPrefs,
  requestNotificationPermission,
  sendTestNotification,
  writeNotificationPrefs,
} from '@/lib/notifications';
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

  const handleToggle = async (next: boolean) => {
    if (!next) {
      setEnabled(false);
      writeNotificationPrefs({ enabled: false });
      return;
    }
    setBusy(true);
    try {
      const granted = await requestNotificationPermission();
      setPermission(granted);
      if (granted === 'granted') {
        setEnabled(true);
        writeNotificationPrefs({ enabled: true });
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
