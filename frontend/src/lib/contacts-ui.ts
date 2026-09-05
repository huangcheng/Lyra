/**
 * Contacts list helpers (Fastmail-style A–Z + addressbook labels).
 */

export type ContactLike = {
  id: string;
  displayName?: string | null;
  emailAddresses: string[];
  addressbookUrl?: string | null;
  accountId: string;
};

/** Sort key: display name, else first email, else empty. */
export function contactSortKey(c: ContactLike): string {
  const name = (c.displayName ?? '').trim();
  if (name) return name;
  return (c.emailAddresses[0] ?? '').trim();
}

/** First letter bucket for A–Z list headers (`#` for digits/symbols). */
export function contactLetter(c: ContactLike): string {
  const key = contactSortKey(c);
  const ch = key.charAt(0).toUpperCase();
  if (ch >= 'A' && ch <= 'Z') return ch;
  return '#';
}

export type LetterGroup<T extends ContactLike> = { letter: string; contacts: T[] };

/** Group sorted contacts under A–Z / `#` headers. */
export function groupContactsByLetter<T extends ContactLike>(contacts: T[]): LetterGroup<T>[] {
  const sorted = [...contacts].sort((a, b) =>
    contactSortKey(a).localeCompare(contactSortKey(b), undefined, { sensitivity: 'base' }),
  );
  const map = new Map<string, T[]>();
  for (const c of sorted) {
    const letter = contactLetter(c);
    const bucket = map.get(letter);
    if (bucket) bucket.push(c);
    else map.set(letter, [c]);
  }
  return [...map.entries()].map(([letter, items]) => ({ letter, contacts: items }));
}

/**
 * Human label from a CardDAV collection href.
 * Fastmail: `…/Default` → Personal, `…/Shared` → Shared; else last path segment.
 */
export function addressbookLabelFromUrl(url: string | null | undefined): string | null {
  if (!url) return null;
  try {
    const path = url.includes('://') ? new URL(url).pathname : url;
    const parts = path.split('/').filter(Boolean);
    const last = parts[parts.length - 1] ?? '';
    const normalized = last.replace(/\.vcf$/i, '');
    if (/^default$/i.test(normalized)) return 'Personal';
    if (/shared$/i.test(normalized)) return 'Shared';
    if (!normalized) return null;
    return decodeURIComponent(normalized);
  } catch {
    return null;
  }
}

export type BookFilter = 'all' | { accountId: string; addressbookUrl?: string };

export function filterContacts<T extends ContactLike>(
  contacts: T[],
  filter: BookFilter,
  query: string,
): T[] {
  const q = query.trim().toLowerCase();
  return contacts.filter((c) => {
    if (filter !== 'all') {
      if (c.accountId !== filter.accountId) return false;
      if (filter.addressbookUrl && (c.addressbookUrl ?? '') !== filter.addressbookUrl) {
        return false;
      }
    }
    if (!q) return true;
    const hay = [c.displayName ?? '', ...c.emailAddresses].join(' ').toLowerCase();
    return hay.includes(q);
  });
}

/** Distinct addressbooks present in the contact set, for the left rail. */
export function uniqueAddressbooks(
  contacts: ContactLike[],
): { accountId: string; addressbookUrl: string; label: string }[] {
  const seen = new Map<string, { accountId: string; addressbookUrl: string; label: string }>();
  for (const c of contacts) {
    const url = c.addressbookUrl;
    if (!url) continue;
    const key = `${c.accountId}\0${url}`;
    if (seen.has(key)) continue;
    seen.set(key, {
      accountId: c.accountId,
      addressbookUrl: url,
      label: addressbookLabelFromUrl(url) ?? 'Address book',
    });
  }
  return [...seen.values()].sort((a, b) => a.label.localeCompare(b.label));
}
