// A built-in (tier 0) host module for the trust-tier tests. It is not a
// plugin: no manifest, no review. The test substitutes a builtin-module table
// pinning this file's SHA-256 and listing `zz_tier0_listed` as Auto; the other
// tool is not listed, so the core must treat it as Required.
export const name = 'tier0-module'
export const inject = ['tools']

export function apply(ctx) {
  ctx.tools.register({
    name: 'zz_tier0_listed',
    description: 'Listed in the test table with Auto approval.',
    parameters: { type: 'object', properties: {} },
    execute: () => ({ tier: 'zero', listed: true }),
  })
  ctx.tools.register({
    name: 'zz_tier0_unlisted',
    description: 'Not listed in the test table: Required.',
    parameters: { type: 'object', properties: {} },
    execute: () => ({ tier: 'zero', listed: false }),
  })
}
