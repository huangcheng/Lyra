/**
 * PWA plumbing: service-worker registration, install-prompt capture, and
 * standalone/platform detection for the Settings install card.
 *
 * The SW registers in production builds only — a dev server with a caching
 * worker serves stale modules and breaks HMR.
 */

import { useState, useEffect } from 'react';

interface BeforeInstallPromptEvent extends Event {
  prompt: () => Promise<void>;
  userChoice: Promise<{ outcome: 'accepted' | 'dismissed' }>;
}

let deferredPrompt: BeforeInstallPromptEvent | null = null;

if (typeof window !== 'undefined') {
  window.addEventListener('beforeinstallprompt', (ev) => {
    ev.preventDefault();
    deferredPrompt = ev as BeforeInstallPromptEvent;
    notifyPromptListeners();
  });
}

type PromptListener = (available: boolean) => void;
const promptListeners = new Set<PromptListener>();

function notifyPromptListeners(): void {
  for (const l of promptListeners) l(deferredPrompt !== null);
}

/** Register the app-shell service worker (production builds only). */
export function registerServiceWorker(): void {
  if (import.meta.env.PROD && 'serviceWorker' in navigator) {
    window.addEventListener('load', () => {
      navigator.serviceWorker.register('/sw.js').catch((err) => {
        console.warn('service worker registration failed', err);
      });
    });
  }
}

/** True when the running window is a standalone display-mode app. */
export function isStandalone(): boolean {
  if (typeof window === 'undefined') return false;
  return (
    window.matchMedia('(display-mode: standalone)').matches ||
    // iOS Safari never flips display-mode; it sets this nav flag instead.
    (navigator as Navigator & { standalone?: boolean }).standalone === true
  );
}

/** iOS Safari has no install prompt — users go through Share → Home Screen. */
export function isIos(): boolean {
  if (typeof navigator === 'undefined') return false;
  return (
    /iPad|iPhone|iPod/.test(navigator.userAgent) ||
    (navigator.platform === 'MacIntel' && navigator.maxTouchPoints > 1)
  );
}

/**
 * Trigger the browser install flow when available. Resolves 'accepted',
 * 'dismissed', or 'unavailable' when the browser never offered a prompt.
 */
export async function promptInstall(): Promise<'accepted' | 'dismissed' | 'unavailable'> {
  if (!deferredPrompt) return 'unavailable';
  await deferredPrompt.prompt();
  const { outcome } = await deferredPrompt.userChoice;
  deferredPrompt = null;
  notifyPromptListeners();
  return outcome;
}

/** Subscribe to install-prompt availability (beforeinstallprompt may fire late). */
export function useInstallAvailable(): boolean {
  const [available, setAvailable] = useState(deferredPrompt !== null);
  useEffect(() => {
    const l: PromptListener = (a) => setAvailable(a);
    promptListeners.add(l);
    return () => {
      promptListeners.delete(l);
    };
  }, []);
  return available;
}
