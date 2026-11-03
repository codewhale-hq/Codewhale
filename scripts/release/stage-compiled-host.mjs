#!/usr/bin/env node
// Release preparation only. Uses an exact local Bun, never installs a runtime.
import { spawnSync } from 'node:child_process'
import { readFileSync, statSync, lstatSync, realpathSync, mkdirSync, copyFileSync, chmodSync, writeFileSync, existsSync } from 'node:fs'
import { dirname, resolve, join, isAbsolute } from 'node:path'
import { fileURLToPath } from 'node:url'
import { createRequire } from 'node:module'

const require = createRequire(import.meta.url)
const hosts = require('../../npm/codewhale/scripts/compiled-hosts.js')
const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..')
const args = process.argv.slice(2)
const option = (name) => {
  const i = args.indexOf(name)
  if (i < 0 || !args[i + 1] || args[i + 1].startsWith('--')) throw new Error(`required ${name} VALUE`)
  return args[i + 1]
}
const json = (file) => {
  if (statSync(file).size > 64 * 1024) throw new Error(`metadata exceeds 64 KiB: ${file}`)
  return JSON.parse(readFileSync(file, 'utf8'))
}
const regular = (file) => {
  if (lstatSync(file).isSymbolicLink()) throw new Error(`symlink input refused: ${file}`)
  const selected = realpathSync(file)
  if (!statSync(selected).isFile()) throw new Error(`not a regular local input: ${file}`)
  return selected
}
const cleanEnv = { ...process.env, NODE_OPTIONS: '', BUN_OPTIONS: '', BUN_BE_BUN: '0', BUN_JSC_useShadowRealm: '0' }
const run = (binary, argv, extra = {}) => {
  const result = spawnSync(binary, argv, { cwd: root, env: cleanEnv, encoding: 'utf8', timeout: 600_000, maxBuffer: 8 * 1024 * 1024, ...extra })
  if (result.error || result.status !== 0) throw result.error ?? new Error(`${binary} failed (${result.status}): ${result.stderr}`)
  return result.stdout
}

if (args.includes('--collect')) {
  const input = resolve(option('--collect'))
  const output = resolve(option('--output-dir'))
  const catalogs = []
  for (const target of Object.keys(hosts.TARGETS)) {
    const directory = join(input, `codewhale-compiled-host-${target}`)
    const file = join(directory, hosts.HOST_CATALOG)
    if (!existsSync(file)) continue
    const catalog = hosts.parseCatalog(json(file))
    if (catalog.hosts.length !== 1 || catalog.hosts[0].target !== target) throw new Error(`wrong target receipt at ${file}`)
    hosts.verifyDirectory(directory, catalog)
    catalogs.push({ directory, catalog })
  }
  if (!catalogs.length) throw new Error('compiled-host delivery enabled but no qualified matching-native image exists')
  const catalog = { ...catalogs[0].catalog, hosts: catalogs.flatMap(({ catalog }) => catalog.hosts) }
  for (const entry of catalogs) for (const key of ['version', 'source_sha', 'bundle_sha256']) {
    if (entry.catalog[key] !== catalog[key]) throw new Error(`mixed release ${key}`)
  }
  hosts.parseCatalog(catalog)
  mkdirSync(output, { recursive: true })
  for (const entry of catalogs) for (const file of hosts.assets(entry.catalog).filter((name) => name !== hosts.HOST_CATALOG)) copyFileSync(join(entry.directory, file), join(output, file))
  writeFileSync(join(output, hosts.HOST_CATALOG), JSON.stringify(catalog, null, 2) + '\n')
  hosts.verifyDirectory(output, catalog)
  console.log(`Collected ${catalog.hosts.length} qualified compiled-host targets`)
} else {
  const target = option('--target')
  const names = hosts.names(target)
  if (hosts.TARGETS[target][0] !== process.platform || hosts.TARGETS[target][1] !== process.arch) throw new Error('compiled host requires a matching native build/qualification runner; no implicit cross-architecture runtime')
  const bunArg = option('--bun')
  if (!isAbsolute(bunArg)) throw new Error('--bun must be an absolute exact local executable')
  const bun = regular(bunArg)
  const closureFile = regular(option('--runtime-closure'))
  const closure = json(closureFile)
  const version = option('--version')
  const sourceSha = option('--source-sha')
  const bundle = join(root, 'crates/engine/extension-host/dist/codewhale-extension-host.mjs')
  const digest = hosts.sha256(readFileSync(bundle))
  const runtimeHash = hosts.sha256(readFileSync(bun))
  const revision = run(bun, ['--revision']).trim()
  if (closure.schema !== 1 || typeof closure.runtime_revision !== 'string' || !/^[a-f0-9]{40}$/.test(closure.runtime_revision) || closure.bundle_sha256 !== digest || closure.source_commit !== sourceSha || closure.runtime_sha256 !== runtimeHash || revision !== `${closure.runtime_version}+${closure.runtime_revision.slice(0, 9)}`) throw new Error('exact local Bun revision/digest differs from reviewed runtime closure')
  // Executed runtime facts distinguish native ARM from an x64 Bun under
  // emulation. The runner label and a hash alone cannot establish image arch.
  const runtimeInfo = JSON.parse(run(bun, ['--no-install', '--no-env-file', `--config=${process.platform === 'win32' ? 'NUL' : '/dev/null'}`, '--no-addons', '-p', 'JSON.stringify({platform:process.platform,arch:process.arch})']))
  if (runtimeInfo.platform !== process.platform || runtimeInfo.arch !== process.arch) throw new Error('selected Bun process identity differs from native qualification target; emulation proof does not transfer')
  const input = (field) => regular(resolve(dirname(closureFile), closure[field]))
  const notices = input('notices')
  const source = input('relink_source')
  hosts.verifyBytes(readFileSync(notices), closure.notices_sha256, 'runtime notices')
  hosts.verifyBytes(readFileSync(source), closure.source_sha256, 'corresponding relink source')
  if (closure.license_closure !== 'complete' || closure.corresponding_source !== 'complete') throw new Error('runtime copyright/license texts and LGPL corresponding source/relink inputs remain incomplete; delivery refused')
  // This reviewed input must contain actual matching texts, not component
  // names or links alone. The exact-build source bundle is a separate input.
  const requiredComponents = ['bun', 'JavaScriptCore', 'WebCore', 'tinycc', 'boringssl', 'brotli', 'libarchive', 'lolhtml', 'lshpack', 'lsqpack', 'lsquic', 'mimalloc', 'picohttpparser', 'zstd', 'simdutf', 'usockets', 'uwebsockets', 'zlib', 'cares', 'icu', 'libbase64', 'libuv', 'libdeflate', 'libjpeg-turbo', 'libspng', 'libwebp', 'highway', 'hdrhistogram', 'uucode', 'tigerbeetle-io', 'llvm-libcxxabi', 'embedded-polyfills', 'rust-dependencies']
  if (!Array.isArray(closure.components) || !requiredComponents.every((name) => closure.components.includes(name)) || !closure.notice_components || typeof closure.notice_components !== 'object') throw new Error('incomplete runtime component closure')
  const allNotices = readFileSync(notices)
  for (const component of requiredComponents) {
    const part = closure.notice_components[component]
    if (!part || typeof part.file !== 'string' || typeof part.sha256 !== 'string') throw new Error(`missing matching notice text for ${component}`)
    const file = regular(resolve(dirname(closureFile), part.file))
    if (statSync(file).size > 1024 * 1024) throw new Error(`oversized component notice ${component}`)
    const text = readFileSync(file)
    hosts.verifyBytes(text, part.sha256, component)
    if (!text.length || !allNotices.includes(text)) throw new Error(`combined notices omit actual ${component} text`)
  }
  if (!Array.isArray(closure.source_inputs) || !['bun', 'webkit', 'tinycc', 'relink-recipe'].every((name) => closure.source_inputs.some((entry) => entry.name === name && typeof entry.root === 'string'))) throw new Error('missing LGPL corresponding source and relink recipe inventory')
  const members = run('tar', ['-tzf', source]).trim().split('\n')
  if (members.some((name) => name.startsWith('/') || name.split('/').includes('..') || name.includes('\\'))) throw new Error('unsafe corresponding-source archive paths')
  for (const entry of closure.source_inputs) {
    if (!/^[A-Za-z0-9_.\/-]+$/.test(entry.root) || entry.root.startsWith('/') || entry.root.split('/').includes('..') || !members.some((member) => member === entry.root || member.startsWith(entry.root.replace(/\/$/, '') + '/'))) throw new Error(`corresponding-source archive omits ${entry.name}`)
  }
  if (closure.source_inputs.find((entry) => entry.name === 'bun').revision !== closure.runtime_revision || closure.source_inputs.find((entry) => entry.name === 'webkit').revision !== closure.webkit_revision) throw new Error('corresponding source revisions differ from the actual runtime')
  const selectedImage = regular(option('--compiled-image'))
  const imageHash = hosts.sha256(readFileSync(selectedImage))
  const selectedInfo = JSON.parse(run(selectedImage, ['--codewhale-host-info']))
  if (selectedInfo.kind !== 'codewhale-extension-host' || selectedInfo.runtime !== 'bun' || selectedInfo.bundle_sha256 !== digest || selectedInfo.version !== closure.runtime_version || selectedInfo.platform !== runtimeInfo.platform || selectedInfo.arch !== runtimeInfo.arch) throw new Error('executed compiled image identity differs from selected Bun/native qualification target')
  const native = json(regular(option('--native-receipt')))
  if (typeof native.log !== 'string' || !/^[A-Za-z0-9_.-]+$/.test(native.log) || native.log === '.' || native.log === '..') throw new Error('Native qualification log must be a contained basename')
  const nativeLog = regular(resolve(dirname(option('--native-receipt')), native.log))
  hosts.verifyBytes(readFileSync(nativeLog), native.log_sha256, 'Native compiled-host qualification log')
  if (native.scope !== 'native-compiled-host' || native.source_sha !== sourceSha || native.bundle_sha256 !== digest || native.runtime_sha256 !== runtimeHash || native.host_sha256 !== imageHash || native.platform !== process.platform || native.arch !== process.arch || (!Number.isSafeInteger(native.passed) || native.passed < hosts.nativeMinimum(target)) || native.failed !== 0 || native.skipped !== 0) throw new Error('missing exact-source matching Native compiled-image containment and memory qualification; fake Core alone is insufficient')
  if (process.platform === 'linux' && (native.libc?.scope !== 'native-runner-observed' || native.libc?.family !== closure.libc)) throw new Error('Linux runtime libc claim is not qualified by this actual Native runner; glibc proof does not qualify musl')
  const output = resolve(option('--output-dir'))
  mkdirSync(output, { recursive: true })
  const binary = join(output, hosts.HOST_NAME + (process.platform === 'win32' ? '.exe' : ''))
  if (selectedImage !== binary) copyFileSync(selectedImage, binary)
  if (hosts.sha256(readFileSync(binary)) !== imageHash) throw new Error('compiled image changed during staging')
  if (process.platform !== 'win32') chmodSync(binary, 0o755)
  const testLog = run(process.execPath, ['--test', '--test-reporter=tap', 'test/compiled-host.test.mjs'], { cwd: join(root, 'crates/engine/extension-host'), env: { ...cleanEnv, CODEWHALE_COMPILED_HOST_TEST_BINARY: binary, CODEWHALE_BUN_TEST_BINARY: bun } })
  writeFileSync(join(output, 'compiled-host-qualification.log'), testLog)
  const count = (name) => Number(testLog.match(new RegExp(`^# ${name} (\\d+)$`, 'm'))?.[1] ?? -1)
  if (count('pass') !== hosts.compiledMinimum(target) || count('fail') !== 0 || count('skipped') !== 0) throw new Error('compiled-image suite did not run every required case successfully')
  const info = JSON.parse(run(binary, ['--codewhale-host-info']))
  if (info.kind !== 'codewhale-extension-host' || info.bundle_sha256 !== digest || info.version !== closure.runtime_version || info.runtime !== 'bun' || info.platform !== runtimeInfo.platform || info.arch !== runtimeInfo.arch) throw new Error('compiled identity differs after actual image qualification')
  if (hosts.sha256(readFileSync(binary)) !== imageHash) throw new Error('qualified image changed during the acceptance probes')
  copyFileSync(binary, join(output, names.binary))
  // These are complete runtime + canonical JS dependency notices and complete
  // corresponding source, not a URL-only promise or a fabricated MIT label.
  writeFileSync(join(output, names.notices), Buffer.concat([readFileSync(notices), Buffer.from('\n\nCodewhale extension host dependencies:\n'), readFileSync(join(root, 'crates/engine/extension-host/dist/LICENSES.txt'))]))
  copyFileSync(source, join(output, names.source))
  const catalog = { schema: 1, version, source_sha: sourceSha, bundle_sha256: digest, hosts: [{
    target, asset: names.binary, sha256: hosts.sha256(readFileSync(binary)),
    notices_asset: names.notices, notices_sha256: hosts.sha256(readFileSync(join(output, names.notices))),
    source_asset: names.source, source_sha256: hosts.sha256(readFileSync(source)),
    runtime_version: closure.runtime_version, runtime_revision: closure.runtime_revision, runtime_sha256: runtimeHash,
    webkit_revision: closure.webkit_revision, bundle_sha256: digest, source_commit: sourceSha,
    native_platform: info.platform, native_arch: info.arch, libc: closure.libc,
    passed: count('pass'), failed: 0, skipped: 0, test_log_sha256: hosts.sha256(Buffer.from(testLog)),
    native_passed: native.passed, native_failed: 0, native_skipped: 0, native_log_sha256: native.log_sha256,
    native_runner_libc: native.libc ?? null,
    license_closure: 'complete', relink_source: 'complete',
  }] }
  hosts.parseCatalog(catalog, version)
  writeFileSync(join(output, hosts.HOST_CATALOG), JSON.stringify(catalog, null, 2) + '\n')
  hosts.verifyDirectory(output, catalog)
  console.log(`Staged opt-in ${target}; Node remains default`)
}
