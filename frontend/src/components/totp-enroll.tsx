/**
 * TOTP enrollment component.
 *
 * Allows users to enable two-factor authentication.
 */

import { useState } from 'react';
import { t } from '../i18n';
import { useUIStore } from '../stores/ui';

interface TotpEnrollProps {
  onComplete: () => void;
  onCancel: () => void;
}

export function TotpEnroll({ onComplete, onCancel }: TotpEnrollProps) {
  const locale = useUIStore((s) => s.locale);
  const [step, setStep] = useState<'init' | 'verify'>('init');
  const [secret, setSecret] = useState<string>('');
  const [otpauthUri, setOtpauthUri] = useState<string>('');
  const [code, setCode] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const startEnrollment = async () => {
    setLoading(true);
    setError(null);

    try {
      const token = localStorage.getItem('lyra_token');
      const res = await fetch('/api/v1/auth/totp/enroll', {
        method: 'POST',
        headers: {
          Authorization: `Bearer ${token}`,
        },
      });

      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        throw new Error(data.error || 'Failed to start enrollment');
      }

      const data = await res.json();
      setSecret(data.secret);
      setOtpauthUri(data.otpauth_uri);
      setStep('verify');
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to start enrollment');
    } finally {
      setLoading(false);
    }
  };

  const confirmEnrollment = async (e: React.FormEvent) => {
    e.preventDefault();
    setLoading(true);
    setError(null);

    try {
      const token = localStorage.getItem('lyra_token');
      const res = await fetch('/api/v1/auth/totp/confirm', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          Authorization: `Bearer ${token}`,
        },
        body: JSON.stringify({ code }),
      });

      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        throw new Error(data.error || 'Invalid code');
      }

      onComplete();
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to confirm');
    } finally {
      setLoading(false);
    }
  };

  if (step === 'init') {
    return (
      <div className="space-y-4">
        <h2 className="text-lg font-semibold">{t(locale, 'auth.totpEnrollTitle')}</h2>
        <p className="text-sm text-muted-foreground">{t(locale, 'auth.totpEnrollDescription')}</p>

        {error && <p className="text-sm text-destructive">{error}</p>}

        <div className="flex gap-2">
          <button
            onClick={startEnrollment}
            disabled={loading}
            className="rounded-md bg-primary px-4 py-2 text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
          >
            {loading ? t(locale, 'common.loading') : t(locale, 'common.confirm')}
          </button>
          <button
            onClick={onCancel}
            className="rounded-md border border-input px-4 py-2 hover:bg-accent"
          >
            {t(locale, 'common.cancel')}
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <h2 className="text-lg font-semibold">{t(locale, 'auth.totpEnrollTitle')}</h2>

      {/* QR Code placeholder - in production, use a QR code library */}
      {otpauthUri && (
        <div className="rounded-md border p-4">
          <p className="mb-2 text-sm font-medium">Scan with your authenticator app:</p>
          <div className="flex items-center justify-center rounded bg-white p-4">
            <pre className="text-xs break-all">{otpauthUri}</pre>
          </div>
        </div>
      )}

      {/* Manual entry */}
      <div>
        <label className="block text-sm font-medium">{t(locale, 'auth.totpEnrollSecret')}</label>
        <div className="mt-1 flex items-center gap-2">
          <code className="rounded bg-muted px-2 py-1 text-sm font-mono">{secret}</code>
        </div>
      </div>

      {/* Verification */}
      <form onSubmit={confirmEnrollment} className="space-y-4">
        <div>
          <label htmlFor="totp-code" className="block text-sm font-medium">
            {t(locale, 'auth.totpCode')}
          </label>
          <input
            id="totp-code"
            type="text"
            inputMode="numeric"
            pattern="[0-9]*"
            maxLength={6}
            value={code}
            onChange={(e) => setCode(e.target.value.replace(/\D/g, ''))}
            className="mt-1 block w-full rounded-md border border-input bg-background px-3 py-2 text-center text-lg tracking-widest"
            placeholder="000000"
            autoFocus
          />
        </div>

        {error && <p className="text-sm text-destructive">{error}</p>}

        <div className="flex gap-2">
          <button
            type="submit"
            disabled={loading || code.length !== 6}
            className="rounded-md bg-primary px-4 py-2 text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
          >
            {loading ? t(locale, 'common.loading') : t(locale, 'auth.totpEnrollConfirm')}
          </button>
          <button
            type="button"
            onClick={onCancel}
            className="rounded-md border border-input px-4 py-2 hover:bg-accent"
          >
            {t(locale, 'common.cancel')}
          </button>
        </div>
      </form>
    </div>
  );
}
