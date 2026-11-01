/**
 * Which trust tier this host process serves.
 *
 * The core starts one host process per tier from the same bundle and says
 * which with `--tier=plugin|builtin`. A plugin-tier host runs reviewed
 * third-party plugins, whose owner ids are the plugin ids the core's discovery
 * builds (`<scope>/<hex>/<name>`); a builtin-tier host runs Codewhale's own
 * host code, whose owner ids are `host:<module>`. The two id spaces cannot
 * meet, and each process refuses an owner of the other tier
 * (`HostRoot.activate`), so a core that mixed them up would be told so
 * instead of running one tier's code in the other's process.
 *
 * Not a security boundary on its own (the OS sandbox and the separate
 * processes are); the Rust core is the authority on what runs where.
 */
export type { HostTier } from './protocol.generated.ts'
import type { HostTier } from './protocol.generated.ts'

/** Every tier-0 owner id starts with this. Mirrors `tier::HOST_OWNER_PREFIX` in Rust. */
export const HOST_OWNER_PREFIX = 'host:'

const TIERS: readonly string[] = ['plugin', 'builtin']

/**
 * The tier named by `--tier=` in `argv` (the arguments after the script), or
 * `plugin` when there is none: the least-privileged tier is the default for a
 * host started by hand. An unknown value, a flag without a value, or a tier
 * named twice throws, so the host refuses to start rather than guess.
 */
export function parseTier(argv: readonly string[]): HostTier {
  let found: HostTier | undefined
  for (const arg of argv) {
    if (arg !== '--tier' && !arg.startsWith('--tier=')) continue
    const value = arg.startsWith('--tier=') ? arg.slice('--tier='.length) : ''
    if (!TIERS.includes(value)) {
      throw new Error(`unknown host tier ${JSON.stringify(value)} (expected --tier=plugin or --tier=builtin)`)
    }
    if (found !== undefined) throw new Error('the host tier was given more than once')
    found = value as HostTier
  }
  return found ?? 'plugin'
}

/**
 * The SHA-256 of each built-in module source this build embeds, in id order:
 * what `host/hello` reports and the core checks against its own pinned table.
 * The build (`build.mjs`) builds the modules first and substitutes their
 * digests for `__BUILTIN_MODULE_DIGESTS__`; run from source it is empty.
 */
declare const __BUILTIN_MODULE_DIGESTS__: Readonly<Record<string, string>>
export function builtinModuleDigests(): { id: string; sha256: string }[] {
  const digests = typeof __BUILTIN_MODULE_DIGESTS__ === 'undefined' ? {} : __BUILTIN_MODULE_DIGESTS__
  return Object.entries(digests)
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
    .map(([id, sha256]) => ({ id, sha256 }))
}

/** The tier an owner id belongs to. Total: the id decides. */
export function ownerTier(ownerId: string): HostTier {
  return ownerId.startsWith(HOST_OWNER_PREFIX) ? 'builtin' : 'plugin'
}
