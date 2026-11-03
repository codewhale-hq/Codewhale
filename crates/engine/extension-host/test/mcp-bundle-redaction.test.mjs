import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { runInNewContext } from 'node:vm'

const here = dirname(fileURLToPath(import.meta.url))
const bundle = readFileSync(join(here, '..', 'dist', 'builtin', 'mcp.mjs'), 'utf8')

test('bundled MCP OAuth errors omit response bodies and dynamic error details', () => {
  assert.doesNotMatch(bundle, /Raw body: \$\{body\}/)
  assert.doesNotMatch(bundle, /Cause: \$\{JSON\.stringify\(error2\.message\)\}/)
  assert.doesNotMatch(bundle, /Cause: \$\{JSON\.stringify\(error2 instanceof Error/)
  assert.match(bundle, /OAuth error response details omitted/)
  assert.ok(bundle.includes('falling back to a new authorization request.'))
})

test('credential retry logging never reads provider-controlled OAuth error fields', () => {
  const logger = bundle.match(/function warnCredentialInvalidation\(provider, error2, invalidated\) \{[\s\S]*?\n\}/)?.[0]
  assert.ok(logger, 'the shipped SDK credential retry logger must be exercised')
  for (const provider of [{}, { invalidateCredentials() {} }]) {
    const warnings = []
    runInNewContext(`${logger}\nwarnCredentialInvalidation(provider, error, "tokens");`, {
      provider,
      error: new Proxy({}, { get() { throw new Error('read of provider-controlled OAuth error') } }),
      console: { warn: message => warnings.push(message) },
    })
    assert.equal(warnings.length, 1)
    assert.match(warnings[0], /tokens.*retrying authorization|retrying authorization.*tokens/)
    assert.match(warnings[0], /details omitted/)
  }
})
