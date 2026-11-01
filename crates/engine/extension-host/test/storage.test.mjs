import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtemp, mkdir, readFile, writeFile, readdir, stat, symlink, rm } from 'node:fs/promises'
import { createHash } from 'node:crypto'
import { spawn } from 'node:child_process'
import { once } from 'node:events'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { createStorage, STORAGE_LIMITS } from '../src/shims/storage.ts'

const moduleUrl = new URL('../src/shims/storage.ts', import.meta.url).href
const keyPath = (dir, key) => join(dir, 'storage-v1', `${createHash('sha256').update(key).digest('hex')}.json`)
const code = (expected) => (error) => error.code === expected

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'codewhale-plugin-storage-'))
  t.after(() => rm(root, { recursive: true, force: true }))
  const a = join(root, 'a'), b = join(root, 'b')
  await Promise.all([mkdir(a, { mode: 0o700 }), mkdir(b, { mode: 0o700 })])
  let live = true
  return { a, b, store: createStorage({ dataDir: a, isActive: () => live }), revoke: () => { live = false } }
}

function worker(t, directory, script) {
  const child = spawn(process.execPath, ['-e', script, directory], { stdio: ['ignore', 'ignore', 'pipe', 'ipc'] })
  let stderr = ''
  child.stderr.on('data', (chunk) => { stderr = (stderr + chunk).slice(-4000) })
  const exited = once(child, 'exit')
  t.after(async () => { if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL'); await exited })
  return { child, exited, stderr: () => stderr }
}

test('storage persists JSON across owner generations and deletes exact keys', async (t) => {
  const { a, store } = await fixture(t)
  assert.ok(Object.isFrozen(store))
  assert.equal(await store.get('absent'), undefined)
  await store.set('profile', { enabled: true, list: [null, 3, '你好'] })
  const restarted = createStorage({ dataDir: a, isActive: () => true })
  assert.deepEqual(await restarted.get('profile'), { enabled: true, list: [null, 3, '你好'] })
  const copy = await restarted.get('profile'); copy.enabled = false
  assert.equal((await store.get('profile')).enabled, true)
  assert.equal(await restarted.delete('profile'), true)
  assert.equal(await restarted.delete('profile'), false)
  assert.equal(await store.get('profile'), undefined)
  await store.set('permissions', true)
  if (process.platform !== 'win32') assert.equal((await stat(keyPath(a, 'permissions'))).mode & 0o777, 0o600)
})

test('owner directories isolate equal keys and keys never become paths', async (t) => {
  const { a, b, store } = await fixture(t)
  const other = createStorage({ dataDir: b, isActive: () => true })
  await store.set('../outside', 'a')
  await other.set('../outside', 'b')
  await store.set('__proto__', { owner: 'a' })
  assert.equal(await store.get('../outside'), 'a')
  assert.equal(await other.get('../outside'), 'b')
  assert.deepEqual(await store.get('__proto__'), { owner: 'a' })
  assert.equal(await other.get('__proto__'), undefined)
  assert.ok((await readdir(join(a, 'storage-v1'))).every((name) => /^[a-f0-9]{64}\.json$/u.test(name)))
})

test('storage refuses invalid JSON, oversized values and keys without damaging state', async (t) => {
  const { a, store } = await fixture(t)
  await store.set('kept', 'original')
  const before = await readFile(keyPath(a, 'kept'))
  const sparse = new Array(2), cyclic = {}; cyclic.self = cyclic
  const symbol = { [Symbol('hidden')]: 1 }, extra = [1]; extra.custom = 2
  const getter = Object.defineProperty({}, 'x', { enumerable: true, get() { throw new Error('must not run') } })
  for (const value of [undefined, NaN, Infinity, 1n, new Date(), () => 1, sparse, cyclic, symbol, getter, extra]) await assert.rejects(store.set('bad', value), code('invalid'))
  for (const key of ['', 'nul\0key', '鲸'.repeat(STORAGE_LIMITS.keyBytes)]) await assert.rejects(store.set(key, 1), code('invalid'))
  await assert.rejects(store.set('big', 'x'.repeat(STORAGE_LIMITS.valueBytes)), code('limit'))
  assert.deepEqual(await readFile(keyPath(a, 'kept')), before)
})

test('observed owner quota refuses the next write and allows replacement or deletion', async (t) => {
  const { store } = await fixture(t)
  const value = 'x'.repeat(STORAGE_LIMITS.valueBytes - 2)
  let count = 0
  for (;;) {
    try { await store.set(`key${count}`, value); count++ } catch (error) { assert.equal(error.code, 'limit'); break }
  }
  assert.ok(count > 0)
  assert.equal(await store.get('key0'), value)
  assert.equal(await store.get(`key${count}`), undefined)
  await store.set('key0', 'smaller')
  await store.set(`key${count}`, value)
  await store.delete('key1')
})

test('observed key-count quota is shared between API instances in one host', async (t) => {
  const { a, store } = await fixture(t)
  await store.set('kept', true)
  await Promise.all(Array.from({ length: STORAGE_LIMITS.keys - 1 }, (_, i) => writeFile(keyPath(a, `k${i}`), JSON.stringify({ version: 1, key: `k${i}`, value: null }), { mode: 0o600 })))
  const other = createStorage({ dataDir: a, isActive: () => true })
  await assert.rejects(other.set('extra', 1), code('limit'))
  await other.set('kept', false)
  await store.delete('k1')
  await other.set('extra', 2)
  assert.equal(await store.get('extra'), 2)
})

test('queued operations refuse a revoked owner and state survives revocation', async (t) => {
  const { a, store, revoke } = await fixture(t)
  await store.set('kept', 7)
  const pending = store.set('late', 8)
  revoke()
  await assert.rejects(pending, code('not_available'))
  await assert.rejects(store.get('kept'), code('not_available'))
  await assert.rejects(store.delete('kept'), code('not_available'))
  const restarted = createStorage({ dataDir: a, isActive: () => true })
  assert.equal(await restarted.get('kept'), 7)
  assert.equal(await restarted.get('late'), undefined)
  await restarted.set('after-reload', 9)
  assert.equal(await restarted.get('after-reload'), 9)
})

test('corrupt records fail closed, preserve old bytes, and support explicit key deletion', async (t) => {
  const { a, store } = await fixture(t)
  await store.set('key', true)
  for (const bytes of ['{broken', '{"version":2,"key":"key","value":null}', '{"version":1,"key":"other","value":null}']) {
    await writeFile(keyPath(a, 'key'), bytes)
    await assert.rejects(store.get('key'), code('corrupt'))
    await assert.rejects(store.set('key', 1), code('corrupt'))
    assert.equal(await readFile(keyPath(a, 'key'), 'utf8'), bytes)
  }
  assert.equal(await store.delete('key'), true)
  await store.set('key', 'recovered')
  assert.equal(await store.get('key'), 'recovered')
})

test('storage refuses a linked record instead of reading or changing its target', { skip: process.platform === 'win32' }, async (t) => {
  const { a, b, store } = await fixture(t)
  await store.get('absent')
  const outside = join(b, 'untouched.json'), bytes = '{"version":1,"key":"secret","value":"untouched"}'
  await writeFile(outside, bytes)
  await symlink(outside, keyPath(a, 'secret'))
  await assert.rejects(store.get('secret'), code('corrupt'))
  await assert.rejects(store.set('secret', 'changed'), code('corrupt'))
  assert.equal(await readFile(outside, 'utf8'), bytes)
})

test('concurrent calls across API instances share the directory queue', async (t) => {
  const { a, store } = await fixture(t)
  const other = createStorage({ dataDir: a, isActive: () => true })
  await Promise.all(Array.from({ length: 24 }, (_, i) => (i % 2 ? store : other).set(`k${i}`, i)))
  assert.deepEqual(await Promise.all(Array.from({ length: 24 }, (_, i) => store.get(`k${i}`))), Array.from({ length: 24 }, (_, i) => i))
})

test('set snapshots input at invocation before callers can mutate it', async (t) => {
  const { store } = await fixture(t)
  const value = { counter: 1 }, pending = store.set('value', value)
  value.counter = 99
  await pending
  assert.deepEqual(await store.get('value'), { counter: 1 })
})

test('storage requires an assigned absolute directory and refuses directory links', { skip: process.platform === 'win32' }, async (t) => {
  const { a, b } = await fixture(t)
  assert.throws(() => createStorage({ dataDir: 'relative', isActive: () => true }), code('invalid'))
  const linked = join(a, 'linked')
  await symlink(b, linked)
  await assert.rejects(createStorage({ dataDir: linked, isActive: () => true }).set('key', 1), code('invalid'))
})

test('killing a writer before publication preserves old state and reload stays writable', async (t) => {
  const { a, store } = await fixture(t)
  await store.set('kept', 'original')
  const run = worker(t, a, `
    const fs = require('node:fs');
    fs.promises.rename = async () => { setInterval(() => {}, 1000); process.send('publishing'); await new Promise(() => {}); };
    require('node:module').syncBuiltinESMExports();
    import(${JSON.stringify(moduleUrl)}).then(({ createStorage }) => createStorage({ dataDir: process.argv[1], isActive: () => true }).set('kept', 'interrupted')).catch(e => { console.error(e); process.exit(1); });
  `)
  const ready = await Promise.race([once(run.child, 'message', { signal: AbortSignal.timeout(5000) }), run.exited.then(() => { throw new Error(run.stderr()) })])
  assert.equal(ready[0], 'publishing')
  run.child.kill('SIGKILL')
  const [, signal] = await run.exited
  assert.equal(signal, 'SIGKILL')
  const names = await readdir(join(a, 'storage-v1'))
  assert.ok(names.some((name) => name.startsWith('.pending-')))
  const reloaded = createStorage({ dataDir: a, isActive: () => true })
  assert.equal(await reloaded.get('kept'), 'original')
  await reloaded.set('kept', 'after-crash')
  await reloaded.set('new', true)
  assert.equal(await reloaded.get('kept'), 'after-crash')
  assert.equal(await reloaded.get('new'), true)
})

test('different-key writers in separate processes cannot erase each other', async (t) => {
  const { a, store } = await fixture(t)
  const makeScript = (prefix) => `import(${JSON.stringify(moduleUrl)}).then(async ({ createStorage }) => {
    const store = createStorage({ dataDir: process.argv[1], isActive: () => true });
    for (let i = 0; i < 12; i++) await store.set(${JSON.stringify(prefix)} + i, i);
    process.disconnect();
  }).catch(e => { console.error(e); process.exit(1); });`
  const first = worker(t, a, makeScript('first')), second = worker(t, a, makeScript('second'))
  for (const run of [first, second]) assert.equal((await run.exited)[0], 0, run.stderr())
  for (let i = 0; i < 12; i++) {
    assert.equal(await store.get(`first${i}`), i)
    assert.equal(await store.get(`second${i}`), i)
  }
})

test('same-key writers publish complete last-writer-wins records during concurrent reads', async (t) => {
  const { a, store } = await fixture(t)
  await store.set('shared', { writer: 'seed', payload: 'seed' })
  const makeScript = (writer) => `import(${JSON.stringify(moduleUrl)}).then(async ({ createStorage }) => {
    const store = createStorage({ dataDir: process.argv[1], isActive: () => true });
    for (let i = 0; i < 12; i++) await store.set('shared', { writer: ${JSON.stringify(writer)}, payload: ${JSON.stringify(writer)}.repeat(10000) });
    process.disconnect();
  }).catch(e => { console.error(e); process.exit(1); });`
  const first = worker(t, a, makeScript('first')), second = worker(t, a, makeScript('second'))
  while (first.child.exitCode === null || second.child.exitCode === null) {
    const value = await store.get('shared')
    assert.ok(['seed', 'first', 'second'].includes(value.writer))
    assert.equal(value.payload, value.writer === 'seed' ? 'seed' : value.writer.repeat(10000))
  }
  for (const run of [first, second]) assert.equal((await run.exited)[0], 0, run.stderr())
  assert.ok(['first', 'second'].includes((await store.get('shared')).writer))
})

// Faults are injected before the first storage import in an owned subprocess.
// Bun's syncBuiltinESMExports is a no-op, so installing hooks after import would
// leave named filesystem bindings untouched. Seed records through the parent
// store first; every child operation then observes the actual injected failure.
// These prove retry/guard behavior, not Windows kernel semantics; the unchanged
// concurrent-writer case runs there.
async function sharingWorker(t, directory, body) {
  const run = worker(t, directory, `
    (async () => {
      const fs = require('node:fs'), { syncBuiltinESMExports } = require('node:module');
      ${body}
      process.disconnect();
    })().catch(e => { console.error(e); process.exit(1); });
  `)
  const message = once(run.child, 'message', { signal: AbortSignal.timeout(5000) })
  const [report] = await Promise.race([message, run.exited.then(() => { throw new Error(run.stderr()) })])
  assert.equal((await run.exited)[0], 0, run.stderr())
  return report
}

test('Windows sharing retries keep complete records and retain atomic replacement', async (t) => {
  const { a, store } = await fixture(t)
  await store.set('shared', 'original')
  const report = await sharingWorker(t, a, `
    const realOpen = fs.promises.open, realRename = fs.promises.rename;
    let reads = 0, publications = 0;
    fs.promises.open = async (...args) => {
      if (String(args[0]).endsWith('.json') && reads++ < 2) throw Object.assign(new Error('sharing'), { code: 'EPERM' });
      return realOpen(...args);
    };
    fs.promises.rename = async (...args) => {
      if (publications++ < 2) throw Object.assign(new Error('sharing'), { code: 'EACCES' });
      return realRename(...args);
    };
    syncBuiltinESMExports();
    const { createStorage } = await import(${JSON.stringify(moduleUrl)});
    const store = createStorage({ dataDir: process.argv[1], isActive: () => true });
    Object.defineProperty(process, 'platform', { value: 'win32' });
    await store.set('shared', 'complete replacement');
    const value = await store.get('shared');
    process.send({ value, publications, reads, names: await fs.promises.readdir(require('node:path').join(process.argv[1], 'storage-v1')) });
  `)
  assert.equal(report.value, 'complete replacement')
  assert.equal(report.publications, 3)
  assert.ok(report.reads >= 3)
  assert.equal(report.names.length, 1)
  assert.match(report.names[0], /^[a-f0-9]{64}\.json$/u)
})

test('Windows destination revalidation recovers after a sharing interval beyond 500ms', async (t) => {
  const { a, store } = await fixture(t)
  await store.set('shared', 'original')
  const report = await sharingWorker(t, a, `
    const timers = require('node:timers/promises');
    const realOpen = fs.promises.open, realRename = fs.promises.rename, realRead = fs.promises.readFile;
    const path = require('node:path').join(process.argv[1], 'storage-v1', require('node:crypto').createHash('sha256').update('shared').digest('hex') + '.json');
    const original = await realRead(path, 'utf8');
    let elapsed = 0, publications = 0, validationDenials = 0;
    const observed = [], waits = [];
    // A virtual sharing interval avoids tying this regression to runner load.
    // Filesystem reads, writes, fsync and the eventual rename remain real.
    timers.setTimeout = async (ms) => { elapsed += ms; waits.push(ms); };
    fs.promises.open = async (...args) => {
      if (String(args[0]).endsWith(require('node:path').basename(path)) && publications && elapsed < 600) {
        validationDenials++;
        observed.push(await realRead(path, 'utf8'));
        throw Object.assign(new Error('destination sharing'), { code: 'EPERM' });
      }
      return realOpen(...args);
    };
    fs.promises.rename = async (...args) => {
      if (publications++ === 0) {
        observed.push(await realRead(path, 'utf8'));
        throw Object.assign(new Error('publication sharing'), { code: 'EACCES' });
      }
      return realRename(...args);
    };
    syncBuiltinESMExports();
    const { createStorage } = await import(${JSON.stringify(moduleUrl)});
    const store = createStorage({ dataDir: process.argv[1], isActive: () => true });
    Object.defineProperty(process, 'platform', { value: 'win32' });
    let code;
    try { await store.set('shared', 'complete replacement'); } catch (error) { code = error.code; }
    process.send({ code, original, observed, waits, elapsed, publications, validationDenials,
      value: await store.get('shared'), bytes: await realRead(path, 'utf8'),
      names: await fs.promises.readdir(require('node:path').dirname(path)) });
  `)
  t.diagnostic(JSON.stringify(report))
  assert.equal(report.code, undefined, JSON.stringify(report))
  assert.ok(report.elapsed >= 600 && report.elapsed <= 2750, 'the finite sharing interval must settle within the bounded retry budget')
  assert.ok(report.validationDenials > 0)
  assert.equal(report.publications, 2, 'one refused rename followed by one real atomic publication')
  assert.equal(report.original, JSON.stringify({ version: 1, key: 'shared', value: 'original' }) + '\n')
  assert.ok(report.observed.length > 1 && report.observed.every((bytes) => bytes === report.original), 'old complete bytes remain visible throughout refusal')
  assert.equal(report.value, 'complete replacement')
  assert.equal(report.bytes, JSON.stringify({ version: 1, key: 'shared', value: 'complete replacement' }) + '\n')
  assert.equal(report.names.length, 1, 'this invocation cleans its temporary file without deleting the destination')
  assert.match(report.names[0], /^[a-f0-9]{64}\.json$/u)
})

test('Windows nonsharing publication errors preserve old state without retry', async (t) => {
  const { a, store } = await fixture(t)
  await store.set('shared', 'original')
  const report = await sharingWorker(t, a, `
    const path = require('node:path').join(process.argv[1], 'storage-v1', require('node:crypto').createHash('sha256').update('shared').digest('hex') + '.json');
    const original = await fs.promises.readFile(path, 'utf8');
    let publications = 0;
    fs.promises.rename = async () => { publications++; throw Object.assign(new Error('io'), { code: 'EIO' }); };
    syncBuiltinESMExports();
    const { createStorage } = await import(${JSON.stringify(moduleUrl)});
    const store = createStorage({ dataDir: process.argv[1], isActive: () => true });
    Object.defineProperty(process, 'platform', { value: 'win32' });
    let code;
    try { await store.set('shared', 'must not publish'); } catch (error) { code = error.code; }
    process.send({ code, publications, original, bytes: await fs.promises.readFile(path, 'utf8'),
      names: await fs.promises.readdir(require('node:path').dirname(path)) });
  `)
  assert.equal(report.code, 'io')
  assert.equal(report.publications, 1)
  assert.equal(report.bytes, report.original)
  assert.equal(report.names.length, 1)
  assert.match(report.names[0], /^[a-f0-9]{64}\.json$/u)
})

test('Windows sharing retries refuse revocation, corrupt replacements and permanent denial', async (t) => {
  const { a } = await fixture(t)
  for (const mode of ['revoked', 'corrupt', 'denied']) {
    const directory = join(a, mode)
    await mkdir(directory, { mode: 0o700 })
    await createStorage({ dataDir: directory, isActive: () => true }).set('shared', 'original')
    const report = await sharingWorker(t, directory, `
      let live = true;
      const path = require('node:path').join(process.argv[1], 'storage-v1', require('node:crypto').createHash('sha256').update('shared').digest('hex') + '.json');
      const original = await fs.promises.readFile(path, 'utf8');
      let publications = 0;
      fs.promises.rename = async () => {
        publications++;
        if (${JSON.stringify(mode)} === 'revoked') live = false;
        if (${JSON.stringify(mode)} === 'corrupt') await fs.promises.writeFile(path, '{broken');
        throw Object.assign(new Error('sharing'), { code: 'EPERM' });
      };
      syncBuiltinESMExports();
      const { createStorage } = await import(${JSON.stringify(moduleUrl)});
      const store = createStorage({ dataDir: process.argv[1], isActive: () => live });
      Object.defineProperty(process, 'platform', { value: 'win32' });
      let code;
      try { await store.set('shared', 'must not publish'); throw new Error('unexpected publication'); } catch (error) { code = error.code; }
      process.send({ code, publications, bytes: await fs.promises.readFile(path, 'utf8'), original, names: await fs.promises.readdir(require('node:path').dirname(path)) });
    `)
    assert.equal(report.code, mode === 'revoked' ? 'not_available' : mode === 'corrupt' ? 'corrupt' : 'io')
    assert.equal(report.publications, mode === 'denied' ? 11 : 1)
    assert.equal(report.bytes, mode === 'corrupt' ? '{broken' : report.original)
    assert.equal(report.names.length, 1, 'only this invocation temporary file is cleaned up')
  }
})

test('sharing failures on other platforms remain visible without replaying publication', async (t) => {
  const { a, store } = await fixture(t)
  await store.set('shared', 'original')
  const report = await sharingWorker(t, a, `
    let publications = 0;
    fs.promises.rename = async () => { publications++; throw Object.assign(new Error('denied'), { code: 'EPERM' }); };
    syncBuiltinESMExports();
    const { createStorage } = await import(${JSON.stringify(moduleUrl)});
    const store = createStorage({ dataDir: process.argv[1], isActive: () => true });
    Object.defineProperty(process, 'platform', { value: 'linux' });
    let code;
    try { await store.set('shared', 'must not publish'); throw new Error('unexpected publication'); } catch (error) { code = error.code; }
    process.send({ code, publications, value: await store.get('shared') });
  `)
  assert.equal(report.code, 'io')
  assert.equal(report.publications, 1)
  assert.equal(report.value, 'original')
})

test('Windows sharing read retries refuse revocation before reopening private state', async (t) => {
  const { a, store } = await fixture(t)
  await store.set('shared', 'private value')
  const report = await sharingWorker(t, a, `
    let live = true;
    const realOpen = fs.promises.open;
    let opens = 0;
    fs.promises.open = async (...args) => {
      if (String(args[0]).endsWith('.json')) {
        opens++;
        live = false;
        throw Object.assign(new Error('sharing'), { code: 'EPERM' });
      }
      return realOpen(...args);
    };
    syncBuiltinESMExports();
    const { createStorage } = await import(${JSON.stringify(moduleUrl)});
    const store = createStorage({ dataDir: process.argv[1], isActive: () => live });
    Object.defineProperty(process, 'platform', { value: 'win32' });
    let code, value;
    try { value = await store.get('shared'); } catch (error) { code = error.code; }
    process.send({ code, opens, value });
  `)
  assert.equal(report.code, 'not_available')
  assert.equal(report.opens, 1)
  assert.equal(Object.hasOwn(report, 'value'), false, 'revoked state is never returned over IPC')
})
