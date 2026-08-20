/**
 * Utility functions for Lyra.
 */

/**
 * Format a date string for display in the mail list.
 * Shows time for today, date for older messages.
 */
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

/**
 * Get initials from a name or email address.
 */
export function getInitials(nameOrEmail: string): string {
  const parts = nameOrEmail.split(/[@.\s]+/).filter(Boolean);
  if (parts.length === 0) return '?';
  if (parts.length === 1) return parts[0].charAt(0).toUpperCase();
  return (parts[0].charAt(0) + parts[parts.length - 1].charAt(0)).toUpperCase();
}

/**
 * Join class names, filtering out falsy values.
 */
export function cn(...classes: (string | false | null | undefined)[]): string {
  return classes.filter(Boolean).join(' ');
}
