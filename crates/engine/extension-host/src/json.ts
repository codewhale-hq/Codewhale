/**
 * A value that survives `JSON.stringify` unchanged and is accepted on the wire:
 * `null`, booleans, strings, finite numbers, dense arrays, and plain objects
 * whose own keys are enumerable string data properties. Holes, extra array
 * properties, accessors and symbol keys would serialize to something other
 * than what was checked (a hole becomes `null`, a getter can answer twice), so
 * they are refused rather than passed to Rust as a different value.
 */
import type { Json } from './protocol.ts'

export function isJson(value: unknown, depth = 0): value is Json {
  if (depth > 64) return false
  if (value === null) return true
  switch (typeof value) {
    case 'boolean':
    case 'string':
      return true
    case 'number':
      return Number.isFinite(value)
    case 'object': {
      const array = Array.isArray(value)
      if (!array && Object.getPrototypeOf(value) !== Object.prototype && Object.getPrototypeOf(value) !== null) return false
      const keys = Reflect.ownKeys(value as object)
      if (array && keys.length !== (value as unknown[]).length + 1) return false
      for (const key of keys) {
        if (array && key === 'length') continue
        if (typeof key !== 'string') return false
        if (array && (!/^(0|[1-9]\d*)$/u.test(key) || Number(key) >= (value as unknown[]).length)) return false
        const descriptor = Object.getOwnPropertyDescriptor(value, key)!
        if (!descriptor.enumerable || !('value' in descriptor) || !isJson(descriptor.value, depth + 1)) return false
      }
      return true
    }
    default:
      return false
  }
}
