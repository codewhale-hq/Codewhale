/**
 * How one composition row's `name` reaches a module.
 *
 * A preset composition is read by `Include`, which rewrites its context's
 * `baseUrl` to the composition's own directory. That is right for a row
 * naming a file the preset ships and wrong for a row naming a package: a
 * locally authored preset lives under the user's home, where Node's upward
 * `node_modules` walk never reaches the harness's own dependencies. Both the
 * mount's import override and discovery's health check therefore have to
 * classify a row's name before they can act on it, and they must classify it
 * the same way — a row discovery resolves from one base and the mount imports
 * from another would be reported healthy and then fail to load.
 * @module @deepseek-ai/dsh-agent-presets/specifier
 */

import { isAbsolute } from 'node:path'
import { pathToFileURL } from 'node:url'

/** One composition row's module specifier, classified by where it resolves. */
export type RowSpecifier =
  /** A `cordis:` builtin the Loader supplies; nothing is resolved. */
  | { readonly kind: 'builtin'; readonly specifier: string }
  /** A path relative to the preset's own directory; the preset ships the file. */
  | { readonly kind: 'preset'; readonly specifier: string }
  /** An absolute path or `file:` URL; it names one file and no base. */
  | { readonly kind: 'file'; readonly specifier: string }
  /** A package name resolved from the installed harness. */
  | { readonly kind: 'package'; readonly specifier: string }

/**
 * Classify one row's `name`.
 *
 * An absolute filesystem path becomes a file URL here rather than at each
 * call site, because Node's ESM resolver rejects a bare drive-letter path on
 * Windows. A `file:` URL is already one and joins it: the Loader accepts both
 * spellings for the same thing, and treating the URL as a package name would
 * hand it to a resolver that only normalizes it, reporting a file that is not
 * there as present. The `specifier` a caller receives is always the string to
 * hand a resolver; only `kind` decides which base it goes with.
 * @param name - the module specifier exactly as the row wrote it.
 * @returns the classification, carrying the specifier to resolve.
 */
export function classifyRowSpecifier(name: string): RowSpecifier {
  if (name.startsWith('cordis:')) return { kind: 'builtin', specifier: name }
  if (name.startsWith('.')) return { kind: 'preset', specifier: name }
  if (name.startsWith('file:')) return { kind: 'file', specifier: name }
  if (isAbsolute(name)) return { kind: 'file', specifier: pathToFileURL(name).href }
  return { kind: 'package', specifier: name }
}
