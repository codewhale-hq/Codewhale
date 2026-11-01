import type { FiberState as CordisFiberState } from '@deepseek-ai/cordis'

// @deepseek-ai/cordis 4.0.4 publishes FiberState as an erased const enum,
// with no runtime FiberState/FiberStatus export. These exact declared values
// keep esbuild imports valid; update this receipt when the exact pin changes.
export const FiberState = {
  PENDING: 0,
  LOADING: 1,
  ACTIVE: 2,
  FAILED: 3,
  DISPOSED: 4,
  UNLOADING: 5,
} as const satisfies Record<string, CordisFiberState>
