/*! Adapted dsh-persona/dsh-system-prompt; upstream licence:
MIT License

Copyright (c) 2026 DeepSeek

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
*/
/** Bounded additive projection of dsh-persona 0.1.6-alpha.1 (MIT), pinned
 * 0d1f50007f9bca3f52b06e1c3074fa14d5fb0720. Core retains its prompt and
 * runtime context. Prefix/suffix become exact entry-scoped contributions. */
import { normalizePromptSection, validatePromptTemplate, MAX_PROMPT_SECTION_BYTES } from '../shims/prompt.ts'
import { ownerStorage } from '../root.ts'

export const PERSONA = '@deepseek-ai/dsh-persona'
export interface PersonaConfig { prefix: string; suffix: string }

/** Used by both nonexecuting catalog readiness and actual Loader activation. */
export function normalizePersonaConfig(value: unknown): PersonaConfig {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new TypeError('persona config must be plain data')
  const config = value as Record<string, unknown>
  if (Object.keys(config).some(key => !['prefix', 'suffix', 'complete', 'includeRuntimeContext'].includes(key))) throw new TypeError('persona config has an unsupported field')
  if (config.complete !== undefined && config.complete !== false) throw new TypeError('persona replacement belongs to the Core prompt and cannot be imported')
  if (config.includeRuntimeContext !== undefined && config.includeRuntimeContext !== true) throw new TypeError('persona runtime-context suppression belongs to the Core prompt and cannot be imported')
  if (typeof config.prefix !== 'string' || (config.suffix !== undefined && typeof config.suffix !== 'string')) throw new TypeError('persona prefix and suffix must be text')
  const result = {prefix: config.prefix, suffix: config.suffix ?? ''} as PersonaConfig
  for (const [position, text] of Object.entries(result)) {
    if (Buffer.byteLength(text, 'utf8') > MAX_PROMPT_SECTION_BYTES) throw new RangeError('persona text exceeds 4096 UTF-8 bytes')
    if (/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]/u.test(text)) throw new TypeError('persona text contains control characters')
    validatePromptTemplate(text)
    // Empty persona sections are absent, matching the borrowed upstream API.
    if (text.trim().length > 0) normalizePromptSection({id: `persona-${position}`, text, interpolate: 'model-cwd'})
  }
  return Object.freeze(result)
}

export const reviewedPersonaModule = Object.freeze({
  name: 'persona',
  inject: ['prompt'],
  apply(ctx: any, value: unknown): void {
    if (!ownerStorage.getStore()?.scope) throw new Error('persona requires an exact Core-selected entry scope')
    const config = normalizePersonaConfig(value)
    for (const [position, text] of Object.entries(config)) {
      if (text.trim().length > 0) ctx.prompt.registerSection({id: `persona-${position}`, text, interpolate: 'model-cwd'})
    }
  },
})
