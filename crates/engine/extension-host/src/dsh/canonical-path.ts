/**
 * One canonical-path rule for the reviewed-closure checks.
 *
 * Windows: JS `realpathSync` lstats every ancestor starting at the drive root,
 * and a Windows LPAC (AppContainer) host cannot read `C:\` (EPERM).
 * `realpathSync.native` asks the OS for the opened file's final path
 * (GetFinalPathNameByHandle) and needs access to that file alone. Its spelling
 * differs from the caller's: a `\\?\` prefix, long names in place of 8.3 short
 * names (`RUNNER~1`), drive-letter and on-disk case. So a canonical path is
 * never compared with a raw one. Both sides go through `canonicalPath`, then
 * `pathKey`, and containment is `relative(canonical root, canonical target)`.
 *
 * Under LPAC, `realpathSync.native` still fails with EPERM on some paths —
 * especially directories such as the reviewed `source/` root — because the
 * open uses FILE_FLAG_BACKUP_SEMANTICS. Core already refused links while
 * granting that tree, so on EPERM/EACCES we keep a stable stripped spelling
 * instead of throwing and aborting admitReviewedClosure. Link refusal for
 * files still goes through the same helper when native succeeds.
 *
 * Elsewhere `realpathSync` is unchanged.
 */
import { realpathSync } from 'node:fs'
import { posix, win32 } from 'node:path'

type Platform = NodeJS.Platform

export function canonicalPath(path: string, platform: Platform = process.platform): string {
  if (platform !== 'win32') return realpathSync(path)
  try {
    return stripVerbatim(realpathSync.native(path), platform)
  } catch (error) {
    const code = error && typeof error === 'object' && 'code' in error ? (error as { code?: string }).code : undefined
    // LPAC cannot open some granted paths the way native realpath requires
    // (notably directories). Fall back to a stable spelling; Core's grant
    // already refused links/reparse points in the admitted tree.
    if (code === 'EPERM' || code === 'EACCES') {
      return stripVerbatim(win32.normalize(path), platform)
    }
    throw error
  }
}

/** Drop the Win32 verbatim prefix: `\\?\C:\x` → `C:\x`, `\\?\UNC\h\s` → `\\h\s`. Case is kept. */
export function stripVerbatim(path: string, platform: Platform = process.platform): string {
  if (platform !== 'win32') return path
  if (/^[\\/]{2}\?[\\/]UNC[\\/]/i.test(path)) return `\\\\${path.slice(8)}`
  if (/^[\\/]{2}\?[\\/]/.test(path)) return path.slice(4)
  return path
}

/** The identity used for equality: normalized and, on Windows, case-folded. Never joined or displayed. */
export function pathKey(path: string, platform: Platform = process.platform): string {
  if (platform !== 'win32') return posix.normalize(path)
  return win32.normalize(stripVerbatim(path, platform)).toLowerCase()
}

export function samePath(a: string, b: string, platform: Platform = process.platform): boolean {
  return pathKey(a, platform) === pathKey(b, platform)
}

/**
 * The `/`-separated path of `target` inside `root`, or `undefined` when it is
 * outside. Both arguments must be spelled the same way (both raw, or both
 * from `canonicalPath`). `''` is the root itself, which is never a file key.
 * Win32 `relative` compares case-insensitively and keeps `target`'s case.
 */
export function insideKey(root: string, target: string, platform: Platform = process.platform): string | undefined {
  const path = platform === 'win32' ? win32 : posix
  const inside = path.relative(stripVerbatim(root, platform), stripVerbatim(target, platform))
  if (inside === '..' || inside.startsWith(`..${path.sep}`) || path.isAbsolute(inside)) return undefined
  return platform === 'win32' ? inside.split(win32.sep).join('/') : inside
}

/**
 * True when `target` names `root/key` with no symbolic link (or junction)
 * inside the root: its canonical path is the canonical root joined with the
 * reviewed key. Links above the root are the root's own location and are
 * absorbed by canonicalizing it. A missing or unreadable target is false.
 */
export function isUnlinkedInside(root: string, key: string, target: string, platform: Platform = process.platform): boolean {
  let canonicalRoot: string, canonicalTarget: string
  try {
    canonicalRoot = canonicalPath(root, platform)
    canonicalTarget = canonicalPath(target, platform)
  } catch {
    return false
  }
  return unlinkedKeyMatches(canonicalRoot, key, canonicalTarget, platform)
}

/** Pure half of `isUnlinkedInside`, over already-canonical paths. */
export function unlinkedKeyMatches(canonicalRoot: string, key: string, canonicalTarget: string, platform: Platform = process.platform): boolean {
  if (!key || key.split('/').some((part) => !part || part === '.' || part === '..')) return false
  const path = platform === 'win32' ? win32 : posix
  return samePath(path.join(stripVerbatim(canonicalRoot, platform), ...key.split('/')), canonicalTarget, platform)
}
