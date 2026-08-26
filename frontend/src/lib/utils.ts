/**
 * Shared helpers for the Lyra web client.
 */

import { clsx, type ClassValue } from 'clsx';
import { twMerge } from 'tailwind-merge';

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

/** Time for today, short date otherwise. */
export function formatMailDate(dateStr: string): string {
  const date = new Date(dateStr);
  const now = new Date();
  const isToday =
    date.getFullYear() === now.getFullYear() &&
    date.getMonth() === now.getMonth() &&
    date.getDate() === now.getDate();

  if (isToday) {
    return date.toLocaleTimeString(undefined, {
      hour: '2-digit',
      minute: '2-digit',
    });
  }

  return date.toLocaleDateString(undefined, {
    month: 'short',
    day: 'numeric',
  });
}

const CJK_CHAR = /[\u3400-\u9FFF\uF900-\uFAFF]/g;

export function getInitials(nameOrEmail: string): string {
  // CJK names read best as their first one or two characters; mixing a latin
  // letter with a hanzi ("M帐") looks broken.
  const cjk = nameOrEmail.match(CJK_CHAR);
  if (cjk && cjk.length > 0) return cjk.slice(0, 2).join('');
  const parts = nameOrEmail.split(/[@.\s]+/).filter(Boolean);
  if (parts.length === 0) return '?';
  if (parts.length === 1) return parts[0].charAt(0).toUpperCase();
  return (parts[0].charAt(0) + parts[parts.length - 1].charAt(0)).toUpperCase();
}
