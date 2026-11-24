import compatibility from './compatibility.json' with { type: 'json' }

/**
 * Service names a plugin may never provide: each is one authority the Rust
 * core owns (§4.3 of the design). `tools`, `commands` and `logger` are
 * provided by the host root as shims and are refused to plugins for the same
 * reason.
 */
export const REFUSED_SERVICES: ReadonlySet<string> = new Set(compatibility.refused_services)

/** Services the root provides; `inject` of anything else fails activation. */
export const PROVIDED_SERVICES: ReadonlySet<string> = new Set(Object.values(compatibility.provided_services).flat())

const UNPROVIDED_REASONS: ReadonlyArray<readonly [(name: string) => boolean, string]> = [
  [(name) => name === 'webServer' || name === 'connection', 'DSH Web UI services; Codewhale has no plugin web server'],
  [(name) => name.startsWith('@deepseek-ai/dsh-client-'), 'DSH client UI services'],
]

const quoted = (names: readonly string[]) => names.map((name) => `\`${name}\``).join(', ')

export function describeMissingServices(names: readonly string[], { scoped: classifyScoped = true }: { scoped?: boolean } = {}): string {
  const absent = names.filter((name) => !classifyScoped || !PROVIDED_SERVICES.has(name))
  const scoped = names.filter((name) => classifyScoped && PROVIDED_SERVICES.has(name))
  const parts: string[] = []
  if (absent.length > 0) {
    const owned = absent.filter((name) => REFUSED_SERVICES.has(name))
    const reasons = [...new Set(absent.flatMap((name) => UNPROVIDED_REASONS.filter(([matches]) => matches(name)).map(([, reason]) => reason)))]
    parts.push(`${quoted(absent)}, not provided by the Codewhale extension host in this phase`
      + (reasons.length > 0 ? ` (${reasons.join('; ')})` : '')
      + (owned.length > 0 ? `; ${quoted(owned)} belong${owned.length === 1 ? 's' : ''} to the Codewhale core and no reviewed bridge exposes ${owned.length === 1 ? 'it' : 'them'} yet` : ''))
  }
  if (scoped.length > 0) parts.push(`${quoted(scoped)}, provided by the host but unavailable in this scope`)
  return parts.join('; ')
}
