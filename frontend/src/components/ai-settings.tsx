/** AI assist settings (BYOK): provider dialect/base URL/model/key, master
 * switch, per-feature flags, connection test. The key is write-only. */

import { useEffect, useState } from 'react';
import { Sparkles } from 'lucide-react';

import { Button } from '@/components/ui/button';
import { invalidateAiSettingsCache } from '@/lib/use-ai-settings';
import { Input } from '@/components/ui/input';
import { Switch } from '@/components/ui/switch';
import { t, type SupportedLocale } from '@/i18n';
import {
  fetchAiSettings,
  saveAiSettings,
  testAiConnection,
  type AiDialect,
  type AiSettings,
  type AiSpamMode,
} from '@/lib/ai-api';

const DIALECTS: AiDialect[] = ['openai_chat', 'openai_responses', 'anthropic'];
const SPAM_MODES: AiSpamMode[] = ['off', 'suggest', 'auto'];

const inputClass =
  'h-8 w-full max-w-md rounded-lg border border-input bg-background px-2.5 text-[13px]';

export function AiSettingsCard({ locale }: { locale: SupportedLocale }) {
  const [settings, setSettings] = useState<AiSettings | null>(null);
  const [apiKey, setApiKey] = useState('');
  const [saving, setSaving] = useState(false);
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Load once on mount; on failure fall back to blank-but-editable defaults.
  useEffect(() => {
    fetchAiSettings()
      .then((s) => setSettings(s))
      .catch((e) => {
        setSettings({
          enabled: false,
          dialect: 'openai_chat',
          baseUrl: '',
          model: '',
          hasKey: false,
          features: { draftReply: false, assistant: false, calendar: false },
          spamMode: 'off',
        });
        setError(e instanceof Error ? e.message : String(e));
      });
  }, []);

  const patch = async (update: Parameters<typeof saveAiSettings>[0]) => {
    if (!settings || saving) return;
    setSaving(true);
    setError(null);
    try {
      setSettings(await saveAiSettings(update));
      invalidateAiSettingsCache();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  const handleTest = async () => {
    if (testing) return;
    setTesting(true);
    setTestResult(null);
    setError(null);
    try {
      const res = await testAiConnection();
      setTestResult(`${t(locale, 'settings.ai.testOk')} (${res.model}): ${res.reply}`);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setTesting(false);
    }
  };

  const s = settings;
  if (!s) {
    return (
      <section className="space-y-3 rounded-[10px] border border-border bg-card px-5 py-4">
        <div className="text-[13px] text-muted-foreground">{t(locale, 'common.loading')}</div>
        {error ? <div className="text-xs text-destructive">{error}</div> : null}
      </section>
    );
  }

  const field = (label: string, node: React.ReactNode) => (
    <label className="flex flex-col gap-1.5">
      <span className="text-[13px] font-medium">{label}</span>
      {node}
    </label>
  );

  return (
    <div className="space-y-4">
      <section className="space-y-4 rounded-[10px] border border-border bg-card px-5 py-4">
        <div className="flex items-center justify-between gap-3">
          <div>
            <h2 className="flex items-center gap-2 text-[13px] font-medium">
              <Sparkles className="size-4 text-muted-foreground" aria-hidden />
              {t(locale, 'settings.ai.provider')}
            </h2>
            <p className="text-xs text-muted-foreground">{t(locale, 'settings.ai.providerDesc')}</p>
          </div>
          <Switch
            checked={s.enabled}
            disabled={saving}
            onCheckedChange={(enabled) => void patch({ enabled })}
          />
        </div>

        {field(
          t(locale, 'settings.ai.dialect'),
          <select
            className={inputClass}
            value={s.dialect}
            disabled={saving}
            onChange={(e) => void patch({ dialect: e.target.value as AiDialect })}
          >
            {DIALECTS.map((d) => (
              <option key={d} value={d}>
                {t(locale, `settings.ai.dialect_${d}`)}
              </option>
            ))}
          </select>,
        )}
        {field(
          t(locale, 'settings.ai.baseUrl'),
          <Input
            className={inputClass}
            placeholder={
              s.dialect === 'anthropic'
                ? 'https://api.anthropic.com'
                : 'https://dashscope.aliyuncs.com/compatible-mode/v1'
            }
            value={s.baseUrl}
            disabled={saving}
            onChange={(e) => setSettings({ ...s, baseUrl: e.target.value })}
            onBlur={() => void patch({ baseUrl: s.baseUrl })}
          />,
        )}
        {field(
          t(locale, 'settings.ai.model'),
          <Input
            className={inputClass}
            placeholder={s.dialect === 'anthropic' ? 'claude-sonnet-4-5' : 'qwen3-max'}
            value={s.model}
            disabled={saving}
            onChange={(e) => setSettings({ ...s, model: e.target.value })}
            onBlur={() => void patch({ model: s.model })}
          />,
        )}
        {field(
          t(locale, 'settings.ai.apiKey'),
          <div className="flex items-center gap-2">
            <Input
              className={inputClass}
              type="password"
              autoComplete="off"
              placeholder={s.hasKey ? t(locale, 'settings.ai.keySet') : 'sk-…'}
              value={apiKey}
              disabled={saving}
              onChange={(e) => setApiKey(e.target.value)}
              onBlur={() => {
                if (apiKey.trim()) {
                  void patch({ apiKey: apiKey.trim() }).then(() => setApiKey(''));
                }
              }}
            />
            {s.hasKey ? (
              <Button
                variant="ghost"
                size="sm"
                disabled={saving}
                onClick={() => void patch({ apiKey: '' })}
              >
                {t(locale, 'settings.ai.clearKey')}
              </Button>
            ) : null}
          </div>,
        )}
        <p className="text-xs text-muted-foreground">{t(locale, 'settings.ai.keyEncrypted')}</p>

        <div className="flex items-center gap-3">
          <Button
            variant="outline"
            size="sm"
            disabled={testing || saving}
            onClick={() => void handleTest()}
          >
            {testing ? t(locale, 'settings.ai.testing') : t(locale, 'settings.ai.test')}
          </Button>
          {testResult ? <span className="text-xs text-ok">{testResult}</span> : null}
          {error ? <span className="text-xs text-destructive">{error}</span> : null}
        </div>
      </section>

      <section className="space-y-3 rounded-[10px] border border-border bg-card px-5 py-4">
        <h2 className="text-[13px] font-medium">{t(locale, 'settings.ai.features')}</h2>
        <div className="flex items-center justify-between gap-3">
          <div>
            <div className="text-[13px] font-medium">{t(locale, 'settings.ai.draftReply')}</div>
            <div className="text-xs text-muted-foreground">
              {t(locale, 'settings.ai.draftReplyDesc')}
            </div>
          </div>
          <Switch
            checked={s.features.draftReply}
            disabled={saving}
            onCheckedChange={(draftReply) =>
              void patch({ features: { ...s.features, draftReply } })
            }
          />
        </div>
        <div className="flex items-center justify-between gap-3 border-t border-border pt-3">
          <div>
            <div className="text-[13px] font-medium">{t(locale, 'settings.ai.assistant')}</div>
            <div className="text-xs text-muted-foreground">
              {t(locale, 'settings.ai.assistantDesc')}
            </div>
          </div>
          <Switch
            checked={s.features.assistant}
            disabled={saving}
            onCheckedChange={(assistant) => void patch({ features: { ...s.features, assistant } })}
          />
        </div>
        <div className="flex items-center justify-between gap-3 border-t border-border pt-3">
          <div>
            <div className="text-[13px] font-medium">{t(locale, 'settings.ai.calendar')}</div>
            <div className="text-xs text-muted-foreground">
              {t(locale, 'settings.ai.calendarDesc')}
            </div>
          </div>
          <Switch
            checked={s.features.calendar}
            disabled={saving}
            onCheckedChange={(calendar) => void patch({ features: { ...s.features, calendar } })}
          />
        </div>
        <div className="flex items-center justify-between gap-3 border-t border-border pt-3">
          <div>
            <div className="text-[13px] font-medium">{t(locale, 'settings.ai.spamMode')}</div>
            <div className="text-xs text-muted-foreground">
              {t(locale, 'settings.ai.spamModeDesc')}
            </div>
          </div>
          <select
            className="h-8 rounded-lg border border-input bg-background px-2 text-[13px]"
            value={s.spamMode}
            disabled={saving}
            onChange={(e) => void patch({ spamMode: e.target.value as AiSpamMode })}
          >
            {SPAM_MODES.map((m) => (
              <option key={m} value={m}>
                {t(locale, `settings.ai.spamMode_${m}`)}
              </option>
            ))}
          </select>
        </div>
        <p className="border-t border-border pt-3 text-xs text-muted-foreground">
          {t(locale, 'settings.ai.privacy')}
        </p>
      </section>
    </div>
  );
}
