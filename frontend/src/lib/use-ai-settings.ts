/**
 * App-wide AI settings with a reactive module-level cache: many surfaces
 * (bubble, reader suggest, compose) need the same small GET. Invalidating
 * after a settings save notifies every mounted subscriber, so toggles take
 * effect immediately — no reload.
 */

import { useEffect, useSyncExternalStore } from 'react';

import { fetchAiSettings, type AiSettings } from '@/lib/ai-api';

let cache: AiSettings | null = null;
let inflight: Promise<AiSettings | null> | null = null;
const listeners = new Set<() => void>();
const emit = () => listeners.forEach((l) => l());

function fetchOnce(): Promise<AiSettings | null> {
    inflight ??= fetchAiSettings()
        .then((s) => {
            cache = s;
            return s;
        })
        .catch(() => null)
        .finally(() => {
            inflight = null;
            emit();
        });
    return inflight;
}

/** Re-fetch and notify subscribers (the settings page calls this on save). */
export function invalidateAiSettingsCache(): void {
    inflight = null;
    void fetchOnce();
}

export function useAiSettings(): AiSettings | null {
    useEffect(() => {
        void fetchOnce();
    }, []);
    return useSyncExternalStore(
        (cb) => {
            listeners.add(cb);
            return () => listeners.delete(cb);
        },
        () => cache,
        () => null,
    );
}
