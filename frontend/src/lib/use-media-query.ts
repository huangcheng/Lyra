/**
 * Tracks a CSS media query. False during SSR / first paint.
 */

import { useEffect, useState } from 'react';

function readMatches(query: string): boolean {
  return typeof window === 'undefined' ? false : window.matchMedia(query).matches;
}

export function useMediaQuery(query: string): boolean {
  // Keyed by query so a query change re-syncs during render (React's
  // documented adjust-state pattern) instead of a synchronous effect.
  const [state, setState] = useState(() => ({ query, matches: readMatches(query) }));
  if (state.query !== query) {
    setState({ query, matches: readMatches(query) });
  }

  useEffect(() => {
    const mql = window.matchMedia(query);
    const onChange = (e: MediaQueryListEvent) => setState({ query, matches: e.matches });
    mql.addEventListener('change', onChange);
    return () => mql.removeEventListener('change', onChange);
  }, [query]);

  return state.matches;
}
