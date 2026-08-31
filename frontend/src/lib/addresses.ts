/**
 * Address-list helpers shared by the compose dialog and its recipient
 * chip input (kept out of the component files for fast-refresh boundaries).
 */

/** Split a raw chunk into addresses on comma/semicolon boundaries. */
export function splitAddresses(raw: string): string[] {
  return raw
    .split(/[,;]/)
    .map((s) => s.trim())
    .filter(Boolean);
}
