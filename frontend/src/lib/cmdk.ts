/**
 * react-cmdk Vite/CJS interop shim.
 *
 * The package ships CommonJS only; depending on the pre-bundle path the
 * component can surface as `ns.default` or nested as `ns.default.default`.
 * Resolve it once at module scope — types stay the package's own.
 */

import * as cmdkNs from 'react-cmdk';
import type CmdkDefault from 'react-cmdk';

type Palette = typeof CmdkDefault;

function resolve(): Palette {
  const outer = cmdkNs as unknown as { default?: unknown };
  if (typeof outer.default === 'function') {
    return outer.default as unknown as Palette;
  }
  const inner = outer.default as { default?: unknown } | undefined;
  if (inner && typeof inner.default === 'function') {
    return inner.default as unknown as Palette;
  }
  return cmdkNs as unknown as Palette;
}

export const CommandPalette: Palette = resolve();
