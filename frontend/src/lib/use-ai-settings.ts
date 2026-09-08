/**
 * App-wide AI settings fetch with a module-level cache: many surfaces
 * (bubble, reader suggest) need the same small GET — cache it for the
 * session; failures mean "AI off".
 */

import { useEffect, useState } from 'react';

import { fetchAiSettings, type AiSettings } from '@/lib/ai-api';

let cached: Promise<AiSettings | null> | null = null;

function load(): Promise<AiSettings | null> {
  cached ??= fetchAiSettings().catch(() => null);
  return cached;
}

/** Invalidate after settings change (the settings page calls this). */
export function invalidateAiSettingsCache(): void {
  cached = null;
}

export function useAiSettings(): AiSettings | null {
  const [settings, setSettings] = useState<AiSettings | null>(null);
  useEffect(() => {
    void load().then(setSettings);
  }, []);
  return settings;
}
