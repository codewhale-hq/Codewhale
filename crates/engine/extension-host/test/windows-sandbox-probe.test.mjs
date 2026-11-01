// Fixed diagnostic logic, not a Windows/kernel isolation receipt.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import fs from 'node:fs'
import net from 'node:net'
import cp from 'node:child_process'
import { EventEmitter } from 'node:events'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { windowsSandboxProbe } from '../src/windows-sandbox-probe.mjs'

const receipt = { version: 1, data_roundtrip: true, outside_read_denied: true, outside_write_denied: true, network_denied: true, descendant_denied: true }
const env = {
  CODEWHALE_WINDOWS_PROBE_INSIDE: '/inside/marker', CODEWHALE_WINDOWS_PROBE_OUTSIDE: '/outside/write',
  CODEWHALE_WINDOWS_PROBE_READS: JSON.stringify(['/codex/auth', '/dsh/credentials', '/builtin/state']),
  CODEWHALE_WINDOWS_PROBE_PORT: '12345', CODEWHALE_WINDOWS_PROBE_CHILD_ARGS: JSON.stringify(['fixed-probe']),
}
function controls(t, options = {}) {
  let value
  const outputPath = `${env.CODEWHALE_WINDOWS_PROBE_INSIDE}.receipt`
  const state = { created: false, closed: false, removed: false, killed: 0, events: [], positions: [], output: Buffer.from(options.output ?? JSON.stringify(receipt)) }
  const originals = [fs.writeFileSync, fs.readFileSync, fs.unlinkSync, fs.openSync, fs.fstatSync, fs.readSync, fs.closeSync, net.connect, cp.spawn]
  t.after(() => { [fs.writeFileSync, fs.readFileSync, fs.unlinkSync, fs.openSync, fs.fstatSync, fs.readSync, fs.closeSync, net.connect, cp.spawn] = originals })
  const denied = (code) => Object.assign(new Error(code), { code })
  fs.writeFileSync = (path, content) => {
    if (path === env.CODEWHALE_WINDOWS_PROBE_INSIDE) { value = content; return }
    if (options.outsideWrite) return
    throw denied('EACCES')
  }
  fs.readFileSync = (path) => {
    if (path === env.CODEWHALE_WINDOWS_PROBE_INSIDE) return value
    throw denied(options.readError ?? 'EACCES')
  }
  fs.unlinkSync = (path) => {
    if (path !== outputPath) { value = undefined; return }
    assert.equal(state.closed, true, 'close the owned descriptor before removing its file')
    state.removed = true; state.events.push('unlink')
  }
  fs.openSync = (path, flags) => {
    assert.equal(path, outputPath)
    assert.equal(flags, 'wx+')
    if (options.openError) throw denied(options.openError)
    state.created = true
    return 71
  }
  fs.fstatSync = (fd) => {
    assert.equal(fd, 71); assert.equal(state.closed, false)
    return { size: options.statSize ?? state.output.length }
  }
  fs.readSync = (fd, buffer, offset, length, position) => {
    assert.equal(fd, 71); assert.equal(state.closed, false)
    assert.ok(length <= 4097)
    state.positions.push(position)
    return state.output.copy(buffer, offset, position, Math.min(position + length, position + (options.shortRead ?? length)))
  }
  fs.closeSync = (fd) => {
    assert.equal(fd, 71); assert.equal(state.created, true); assert.equal(state.closed, false)
    state.closed = true; state.events.push('file-close')
  }
  net.connect = () => {
    const socket = new EventEmitter()
    socket.destroy = () => {}
    if (!options.timeout) process.nextTick(() => {
      if (options.networkConnect) socket.emit('connect')
      else socket.emit('error', denied(options.networkError ?? 'EACCES'))
    })
    return socket
  }
  cp.spawn = (program, args, launch) => {
    assert.equal(program, process.execPath)
    assert.deepEqual(args, ['fixed-probe'])
    assert.equal(launch.env.CODEWHALE_WINDOWS_PROBE_DESCENDANT, '1')
    assert.equal(launch.env.CODEWHALE_WINDOWS_PROBE_INSIDE, `${env.CODEWHALE_WINDOWS_PROBE_INSIDE}.child`)
    assert.deepEqual(launch.stdio, [0, 71, 71], 'no NUL open or runtime-created pipes')
    if (options.spawnThrow) throw denied('EPERM')
    const child = new EventEmitter()
    state.child = child
    const close = (code, signal) => { state.events.push('child-close'); child.emit('close', code, signal) }
    child.kill = () => {
      state.killed += 1; state.events.push('kill')
      if (!options.hangAfterKill) process.nextTick(() => close(null, 'SIGTERM'))
      return true
    }
    if (options.spawnError) process.nextTick(() => child.emit('error', denied('EPERM')))
    else if (!options.hang) process.nextTick(() => close(options.exitCode ?? 0, options.signal ?? null))
    return child
  }
  return state
}

test('fixed probe requires exact own-data and parent/descendant denial receipt', async (t) => {
  const state = controls(t, { shortRead: 7 })
  assert.deepEqual(await windowsSandboxProbe(env), receipt)
  assert.equal(state.positions[0], 0)
  assert.ok(state.positions.length > 2, 'short reads must be completed with explicit positions')
  assert.deepEqual(state.events, ['child-close', 'file-close', 'unlink'])
})
test('preexisting receipt is preserved when exclusive creation fails', async (t) => {
  const state = controls(t, { openError: 'EEXIST' })
  await assert.rejects(windowsSandboxProbe(env), /EEXIST/)
  assert.deepEqual(state.events, [])
  assert.equal(state.removed, false)
})
test('synchronous spawn failure cleans up only the newly created receipt', async (t) => {
  const state = controls(t, { spawnThrow: true })
  await assert.rejects(windowsSandboxProbe(env), /EPERM/)
  assert.deepEqual(state.events, ['file-close', 'unlink'])
})
test('asynchronous spawn failure waits for child closure before cleanup', async (t) => {
  const state = controls(t, { spawnError: true })
  await assert.rejects(windowsSandboxProbe(env), /EPERM/)
  assert.deepEqual(state.events, ['kill', 'child-close', 'file-close', 'unlink'])
})
for (const [name, options] of [
  ['nonzero exit', { exitCode: 1 }],
  ['signal exit', { signal: 'SIGTERM' }],
  ['missing receipt', { output: '' }],
  ['malformed receipt', { output: '{' }],
  ['incomplete denial receipt', { output: JSON.stringify({ ...receipt, descendant_denied: false }) }],
  ['stderr mixed into receipt', { output: `${JSON.stringify(receipt)}\nunexpected warning` }],
  ['oversized receipt', { output: 'x'.repeat(4097) }],
  ['receipt growth after stat', { output: 'x'.repeat(4097), statSize: 10 }],
]) {
  test(`descendant ${name} fails closed and cleans up`, async (t) => {
    const state = controls(t, options)
    await assert.rejects(windowsSandboxProbe(env))
    assert.deepEqual(state.events, ['child-close', 'file-close', 'unlink'])
  })
}
test('in-flight excess output terminates the child before its deadline', async (t) => {
  const state = controls(t, { hang: true, output: 'x'.repeat(4097) })
  await assert.rejects(windowsSandboxProbe(env), /exceeds 4096 bytes/)
  assert.deepEqual(state.events, ['kill', 'child-close', 'file-close', 'unlink'])
})
test('descendant timeout and missing close stay bounded; late close cannot reread a cleaned descriptor', async (t) => {
  const state = controls(t, { hang: true, hangAfterKill: true })
  const timeout = globalThis.setTimeout
  t.after(() => { globalThis.setTimeout = timeout })
  globalThis.setTimeout = (callback, after, ...args) => timeout(callback, after === 6000 || after === 1000 ? 1 : after, ...args)
  await assert.rejects(windowsSandboxProbe(env), /descendant probe timed out/)
  assert.deepEqual(state.events, ['kill', 'file-close', 'unlink'])
  state.child.emit('close', 0, null)
  assert.deepEqual(state.positions, [])
})
test('outside write success is an admission failure', async (t) => {
  controls(t, { outsideWrite: true })
  await assert.rejects(windowsSandboxProbe(env), /allowed an outside write/)
})
test('a missing credential file cannot count as filesystem isolation', async (t) => {
  controls(t, { readError: 'ENOENT' })
  await assert.rejects(windowsSandboxProbe(env), /ENOENT/)
})
test('a reachable loopback connection is an admission failure', async (t) => {
  controls(t, { networkConnect: true })
  await assert.rejects(windowsSandboxProbe(env), /allowed direct network/)
})
test('an unavailable listener cannot count as network isolation', async (t) => {
  controls(t, { networkError: 'ECONNREFUSED' })
  await assert.rejects(windowsSandboxProbe(env), /without an access denial: ECONNREFUSED/)
})
test('network timeout fails admission instead of passing a denial', async (t) => {
  controls(t, { timeout: true })
  const timeout = globalThis.setTimeout
  t.after(() => { globalThis.setTimeout = timeout })
  globalThis.setTimeout = (callback, after, ...args) => {
    assert.equal(after, 3000)
    queueMicrotask(() => callback(...args))
    return 0
  }
  await assert.rejects(windowsSandboxProbe(env), /network probe timed out/)
})
test('invalid Core projection fails before side effects', async () => {
  await assert.rejects(windowsSandboxProbe({}), /invalid Core sandbox probe projection/)
})
test('actual runtime inherits the receipt descriptor and leaves no file; transport evidence only', async (t) => {
  const root = fs.mkdtempSync(join(tmpdir(), 'cw-win-probe-stdio-'))
  const inside = join(root, 'inside'), outside = join(root, 'outside')
  const reads = ['codex', 'dsh', 'builtin'].map((name) => join(root, name))
  const writeFile = fs.writeFileSync, readFile = fs.readFileSync, connect = net.connect
  t.after(() => {
    fs.writeFileSync = writeFile; fs.readFileSync = readFile; net.connect = connect
    fs.rmSync(root, { recursive: true, force: true })
  })
  const denied = () => Object.assign(new Error('synthetic access denial'), { code: 'EACCES' })
  fs.writeFileSync = (path, ...args) => {
    if (path === outside) throw denied()
    return writeFile(path, ...args)
  }
  fs.readFileSync = (path, ...args) => {
    if (reads.includes(path)) throw denied()
    return readFile(path, ...args)
  }
  net.connect = () => {
    const socket = new EventEmitter()
    socket.destroy = () => {}
    process.nextTick(() => socket.emit('error', denied()))
    return socket
  }
  // Only this transport fixture substitutes the child body. Production still
  // receives Core's same full probe argv; these synthetic denials are not LPAC proof.
  const args = ['-e', `process.stdout.write(${JSON.stringify(JSON.stringify(receipt) + '\n')})`]
  assert.deepEqual(await windowsSandboxProbe({ ...env,
    CODEWHALE_WINDOWS_PROBE_INSIDE: inside, CODEWHALE_WINDOWS_PROBE_OUTSIDE: outside,
    CODEWHALE_WINDOWS_PROBE_READS: JSON.stringify(reads), CODEWHALE_WINDOWS_PROBE_CHILD_ARGS: JSON.stringify(args),
  }), receipt)
  assert.deepEqual(fs.readdirSync(root), [])
})
test('actual unrestricted Node is rejected by the fixed write control', async () => {
  const root = fs.mkdtempSync(join(tmpdir(), 'cw-win-probe-negative-'))
  const inside = join(root, 'inside'), outside = join(root, 'outside')
  const reads = ['codex', 'dsh', 'builtin'].map((name) => join(root, name))
  try {
    reads.forEach((path) => fs.writeFileSync(path, 'non-secret-control'))
    await assert.rejects(windowsSandboxProbe({ ...env, CODEWHALE_WINDOWS_PROBE_INSIDE: inside,
      CODEWHALE_WINDOWS_PROBE_OUTSIDE: outside, CODEWHALE_WINDOWS_PROBE_READS: JSON.stringify(reads) }), /allowed an outside write/)
    assert.equal(fs.existsSync(outside), true, 'real unrestricted write must prove the negative control is exercised')
  } finally { fs.rmSync(root, { recursive: true, force: true }) }
})
