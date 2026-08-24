/**
 * Light / dark / system theme handling.
 * Persisted in localStorage; applied as a `dark` class on <html>
 * (Tailwind dark variant is wired via @custom-variant in index.css).
 */

export type ThemeMode = 'light' | 'dark' | 'system';

const STORAGE_KEY = 'lyra_theme';

export function getStoredTheme(): ThemeMode {
  const value = localStorage.getItem(STORAGE_KEY);
  return value === 'light' || value === 'dark' || value === 'system' ? value : 'system';
}

export function applyTheme(mode: ThemeMode): void {
  const prefersDark = window.matchMedia('(prefers-color-scheme: dark)').matches;
  const dark = mode === 'dark' || (mode === 'system' && prefersDark);
  document.documentElement.classList.toggle('dark', dark);
}

export function storeTheme(mode: ThemeMode): void {
  localStorage.setItem(STORAGE_KEY, mode);
}

/** Apply the stored theme and follow OS changes while mode is `system`. */
export function initTheme(): ThemeMode {
  const mode = getStoredTheme();
  applyTheme(mode);
  window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => {
    if (getStoredTheme() === 'system') applyTheme('system');
  });
  return mode;
}
