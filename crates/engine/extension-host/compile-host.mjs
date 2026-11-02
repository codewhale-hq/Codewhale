// Compile the committed canonical host with an explicitly supplied local Bun.
// --compile-executable-path forbids Bun's implicit runtime download. Target and
// architecture therefore follow that binary; cross targets need a matching
// verified build input, not a runtime fetched during host startup.
import { spawnSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { readFileSync, realpathSync } from 'node:fs'
import { dirname, isAbsolute, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const args = process.argv.slice(2)
const value = (flag) => {
  const index = args.indexOf(flag)
  if (index < 0 || !args[index + 1] || args[index + 1].startsWith('--')) throw new Error(`required: ${flag} PATH`)
  return args[index + 1]
}
const bun = value('--bun')
if (!isAbsolute(bun)) throw new Error('--bun must name an absolute local Bun executable')
const binary = realpathSync(bun)
const output = resolve(value('--output'))
const bundle = resolve(here, 'dist/codewhale-extension-host.mjs')
const sourceSha = createHash('sha256').update(readFileSync(bundle)).digest('hex')
const cleanEnv = { ...process.env, NODE_OPTIONS: '', BUN_OPTIONS: '', BUN_BE_BUN: '0', BUN_JSC_useShadowRealm: '0' }
// Actual selected compiler process facts, including emulation. Never label a
// runtime/image using the architecture of the Node driver or Rust runner.
const facts = spawnSync(binary, ['--no-install', '--no-env-file', `--config=${process.platform === 'win32' ? 'NUL' : '/dev/null'}`, '-p',
  'JSON.stringify({platform:process.platform,arch:process.arch})'],
  { cwd: here, env: cleanEnv, encoding: 'utf8', timeout: 5000, maxBuffer: 16 * 1024 })
if (facts.error || facts.status !== 0) throw facts.error ?? new Error(`compiler runtime identity failed (${facts.status})`)
const runtimeInfo = JSON.parse(facts.stdout)
if (!['linux', 'darwin', 'win32'].includes(runtimeInfo.platform) || !['x64', 'arm64'].includes(runtimeInfo.arch)) throw new Error('unsupported compiler runtime identity')
const run = (argv) => {
  const result = spawnSync(binary, argv, { cwd: here, env: cleanEnv, encoding: 'utf8', timeout: 120_000, maxBuffer: 4 * 1024 * 1024 })
  if (result.stdout) process.stderr.write(result.stdout)
  if (result.stderr) process.stderr.write(result.stderr)
  if (result.error || result.status !== 0) throw result.error ?? new Error(`Bun failed (${result.status})`)
}
run([
  '--no-install', '--no-env-file', `--config=${process.platform === 'win32' ? 'NUL' : '/dev/null'}`, 'build', '--compile', `--compile-executable-path=${binary}`,
  // A Windows Native host runs in an LPAC, which cannot open the NUL device;
  // --no-compile-autoload-bunfig below already keeps bunfig.toml unread there.
  `--compile-exec-argv=--no-install --no-env-file${process.platform === 'win32' ? '' : ' --config=/dev/null'} --no-addons`,
  '--no-compile-autoload-dotenv', '--no-compile-autoload-bunfig',
  '--no-compile-autoload-tsconfig', '--no-compile-autoload-package-json',
  '--define=CODEWHALE_COMPILED_HOST=true',
  `--define=CODEWHALE_COMPILED_BUNDLE_SHA256=${JSON.stringify(sourceSha)}`,
  resolve(here, 'src/compiled.mjs'), '--outfile', output,
])
const probe = spawnSync(output, ['--codewhale-host-info'], { env: cleanEnv, encoding: 'utf8', timeout: 5000, maxBuffer: 16 * 1024 })
if (probe.error || probe.status !== 0) throw probe.error ?? new Error(`compiled host probe failed (${probe.status})`)
const info = JSON.parse(probe.stdout)
if (info.kind !== 'codewhale-extension-host' || info.runtime !== 'bun' || info.bundle_sha256 !== sourceSha || typeof info.version !== 'string' || info.platform !== runtimeInfo.platform || info.arch !== runtimeInfo.arch) {
  throw new Error('compiled host identity does not match the canonical bundle')
}
console.log(JSON.stringify({ output, ...info, compiler: runtimeInfo }))
