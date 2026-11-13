import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { readFileSync, readdirSync, statSync } from 'node:fs'
import { dirname, join, relative, sep } from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const pin = join(here, '..', 'src', 'dsh', 'upstream', 'creator')
const manifest = JSON.parse(readFileSync(join(pin, 'UPSTREAM.json'), 'utf8'))
const sha256 = (bytes) => createHash('sha256').update(bytes).digest('hex')

function walk(dir) {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name)
    return statSync(path).isDirectory() ? walk(path) : [path]
  })
}

test('every pinned creator file equals its recorded hash and is verbatim', () => {
  assert.equal(manifest.version, '0.2.2-rc.1')
  assert.ok(manifest.files.length >= 2)
  for (const entry of manifest.files) {
    assert.equal(entry.verbatim, true, entry.path)
    const bytes = readFileSync(join(pin, entry.path))
    assert.equal(bytes.length, entry.bytes, `${entry.path} size`)
    assert.equal(sha256(bytes), entry.sha256, `${entry.path} hash`)
    assert.ok(!bytes.includes(13), `${entry.path} must keep LF line endings`)
  }
})

test('the pin directory holds exactly the files the manifest lists', () => {
  const listed = new Set([...manifest.files.map((entry) => entry.path), 'UPSTREAM.json', 'LICENSE', 'README.md'])
  const present = walk(pin).map((path) => relative(pin, path).split(sep).join('/'))
  assert.deepEqual(present.sort(), [...listed].sort())
})

test('the licence is the MIT text the four packages ship', () => {
  const license = readFileSync(join(pin, 'LICENSE'))
  assert.equal(sha256(license), manifest.license_sha256)
  assert.match(license.toString('utf8'), /^MIT License\n\nCopyright \(c\) 2026 DeepSeek\n/)
  assert.deepEqual(manifest.packages.sort(), [
    '@deepseek-ai/dsh-agent-preset',
    '@deepseek-ai/dsh-plugin-manager',
    '@deepseek-ai/dsh-sandbox',
    '@deepseek-ai/dsh-tool-cordis',
  ])
})
