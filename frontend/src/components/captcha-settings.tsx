/**
 * Settings → General: login captcha card.
 *
 * Server-wide bot protection for login/bootstrap: pick a provider preset
 * (Cloudflare Turnstile, hCaptcha, Google reCAPTCHA) or disable it entirely.
 * Each provider keeps its own site-key/secret pair, so switching providers
 * never discards credentials. Saved server-side (secrets encrypted under the
 * master key); a saved setting overrides the LYRA_CAPTCHA_* env config.
 */

import { useEffect, useState } from 'react';
import { ShieldCheck } from 'lucide-react';

import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select';
import { t } from '@/i18n';
import {
  fetchCaptchaSettings,
  saveCaptchaSettings,
  type CaptchaProviderSetting,
  type CaptchaSettings,
} from '@/lib/captcha-api';
import { useUIStore } from '@/stores/ui';

const PROVIDERS: CaptchaProviderSetting[] = [
  'none',
  'turnstile',
  'hcaptcha',
  'recaptcha',
  'recaptcha-v3',
];

function isConfigured(settings: CaptchaSettings | null, provider: CaptchaProviderSetting): boolean {
  const pair = settings?.providers[provider];
  return Boolean(pair && pair.siteKey && pair.hasSecret);
}

export function CaptchaSettingsCard() {
  const locale = useUIStore((s) => s.locale);
  const [settings, setSettings] = useState<CaptchaSettings | null>(null);
  const [provider, setProvider] = useState<CaptchaProviderSetting>('none');
  const [siteKey, setSiteKey] = useState('');
  const [secret, setSecret] = useState('');
  const [saving, setSaving] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const fillFields = (s: CaptchaSettings | null, p: CaptchaProviderSetting) => {
    setSiteKey(s?.providers[p]?.siteKey ?? '');
    setSecret('');
  };

  useEffect(() => {
    void fetchCaptchaSettings()
      .then((s) => {
        setSettings(s);
        setProvider(s.active);
        fillFields(s, s.active);
      })
      .catch(() => {});
  }, []);

  const enabled = provider !== 'none';
  const storedPair = settings?.providers[provider];
  const dirty =
    settings !== null &&
    (provider !== settings.active ||
      siteKey !== (storedPair?.siteKey ?? '') ||
      secret.trim() !== '');

  const handleSave = async () => {
    setSaving(true);
    setError(null);
    setMessage(null);
    try {
      const saved = await saveCaptchaSettings({
        active: provider,
        providers: enabled
          ? {
              [provider]: {
                siteKey: siteKey.trim(),
                ...(secret.trim() ? { secret: secret.trim() } : {}),
              },
            }
          : undefined,
      });
      setSettings(saved);
      setProvider(saved.active);
      fillFields(saved, saved.active);
      setMessage(t(locale, 'settings.captcha.saved'));
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  return (
    <section className="space-y-3 rounded-[10px] border border-border bg-card px-5 py-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex items-start gap-2.5">
          <ShieldCheck className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />
          <div>
            <div className="text-[13px] font-medium">{t(locale, 'settings.captcha.title')}</div>
            <div className="text-xs text-muted-foreground">
              {t(locale, 'settings.captcha.subtitle')}
            </div>
          </div>
        </div>
        <Select
          value={provider}
          onValueChange={(v) => {
            const next = v as CaptchaProviderSetting;
            setProvider(next);
            fillFields(settings, next);
            setMessage(null);
            setError(null);
          }}
          disabled={saving || settings === null}
        >
          <SelectTrigger size="sm" className="min-w-[190px]">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {PROVIDERS.map((p) => (
              <SelectItem key={p} value={p}>
                {t(locale, `settings.captcha.provider.${p}`)}
                {p !== 'none' && isConfigured(settings, p) ? ' ✓' : ''}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      {enabled && (
        <div className="space-y-2 border-t border-border pt-3">
          <div className="space-y-1.5">
            <label className="text-sm" htmlFor="settings-captcha-site-key">
              {t(locale, 'settings.captcha.siteKey')}
            </label>
            <Input
              id="settings-captcha-site-key"
              value={siteKey}
              onChange={(e) => setSiteKey(e.target.value)}
              autoComplete="off"
            />
          </div>
          <div className="space-y-1.5">
            <label className="text-sm" htmlFor="settings-captcha-secret">
              {t(locale, 'settings.captcha.secret')}
            </label>
            <Input
              id="settings-captcha-secret"
              type="password"
              value={secret}
              onChange={(e) => setSecret(e.target.value)}
              placeholder={storedPair?.hasSecret ? t(locale, 'settings.captcha.secretKeep') : ''}
              autoComplete="off"
            />
          </div>
        </div>
      )}

      {settings?.source === 'env' && settings.active !== 'none' && (
        <p className="text-[11px] text-muted-foreground">
          {t(locale, 'settings.captcha.sourceEnv')}
        </p>
      )}
      {error && <div className="text-sm text-destructive">{error}</div>}
      {message && (
        <div className="text-sm text-muted-foreground" role="status">
          {message}
        </div>
      )}

      <div>
        <Button
          variant="outline"
          size="sm"
          disabled={
            saving ||
            settings === null ||
            !dirty ||
            (enabled && (!siteKey.trim() || (!secret.trim() && !storedPair?.hasSecret)))
          }
          onClick={() => void handleSave()}
        >
          {saving ? t(locale, 'common.loading') : t(locale, 'common.save')}
        </Button>
      </div>
    </section>
  );
}
