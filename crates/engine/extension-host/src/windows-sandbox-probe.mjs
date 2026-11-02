// Fixed Core-selected startup diagnostic. No plugin or provider code executes.
import fs from 'node:fs'
import net from 'node:net'
import childProcess from 'node:child_process'

const denied = (operation) => {
  try { operation() } catch (error) {
    if (error?.code === 'EACCES' || error?.code === 'EPERM') return true
    throw error
  }
  return false
}

export async function windowsSandboxProbe(env = process.env) {
  const inside = env.CODEWHALE_WINDOWS_PROBE_INSIDE
  const outside = env.CODEWHALE_WINDOWS_PROBE_OUTSIDE
  const reads = JSON.parse(env.CODEWHALE_WINDOWS_PROBE_READS ?? '[]')
  const port = Number(env.CODEWHALE_WINDOWS_PROBE_PORT)
  if (!inside || !outside || reads.length < 3 || !Number.isInteger(port) || port <= 0 || port > 65535) throw new Error('invalid Core sandbox probe projection')
  const marker = 'codewhale-windows-isolation-probe'
  fs.writeFileSync(inside, marker, { flag: 'wx' })
  if (fs.readFileSync(inside, 'utf8') !== marker) throw new Error('sandbox data roundtrip failed')
  fs.unlinkSync(inside)
  if (!denied(() => fs.writeFileSync(outside, marker, { flag: 'wx' }))) throw new Error('sandbox allowed an outside write')
  for (const path of reads) {
    if (!denied(() => fs.readFileSync(path))) throw new Error('sandbox allowed an outside credential read')
  }
  // The Core keeps a real listening socket alive. A timeout, ENOENT, refusal,
  // or unreachable address cannot be mistaken for network isolation.
  await new Promise((resolve, reject) => {
    const socket = net.connect({ host: '127.0.0.1', port })
    const timer = setTimeout(() => { socket.destroy(); reject(new Error('sandbox network probe timed out')) }, 3000)
    socket.once('connect', () => { clearTimeout(timer); socket.destroy(); reject(new Error('sandbox allowed direct network')) })
    socket.once('error', (error) => {
      clearTimeout(timer); socket.destroy()
      if (error.code === 'EACCES' || error.code === 'EPERM') resolve()
      else reject(new Error(`network probe failed without an access denial: ${error.code}`))
    })
  })
  const receipt = { version: 1, data_roundtrip: true, outside_read_denied: true, outside_write_denied: true, network_denied: true, descendant_denied: true }
  if (env.CODEWHALE_WINDOWS_PROBE_DESCENDANT !== '1') {
    const args = JSON.parse(env.CODEWHALE_WINDOWS_PROBE_CHILD_ARGS ?? 'null')
    if (!Array.isArray(args) || args.some((arg) => typeof arg !== 'string')) throw new Error('missing Core child probe argv')
    // LPAC cannot open NUL or libuv's global named pipes. Core already gave
    // us an EOF stdin handle; capture the fixed child receipt in own-data and
    // inherit all three handles instead of asking the runtime to create any.
    const outputPath = `${inside}.receipt`
    const fd = fs.openSync(outputPath, 'wx+')
    try {
      await new Promise((resolve, reject) => {
        let child, timer, poll, closeTimer, failure, settled = false
        const finish = (error) => {
          if (settled) return
          settled = true
          clearTimeout(timer); clearInterval(poll); clearTimeout(closeTimer)
          if (error) reject(error)
          else resolve()
        }
        const fail = (error) => {
          if (settled || failure) return
          failure = error
          clearTimeout(timer); clearInterval(poll)
          // Wait for inherited handles to close before cleanup. Core's outer
          // job deadline still kills the whole tree if termination stalls.
          closeTimer = setTimeout(() => finish(failure), 1000)
          try { child.kill() } catch {}
        }
        try {
          child = childProcess.spawn(process.execPath, args, { stdio: [0, fd, fd], windowsHide: true, env: {
            ...env, CODEWHALE_WINDOWS_PROBE_DESCENDANT: '1', CODEWHALE_WINDOWS_PROBE_INSIDE: `${inside}.child`,
          } })
        } catch (error) { finish(error); return }
        timer = setTimeout(() => fail(new Error('sandbox descendant probe timed out')), 6000)
        // This stops excess output promptly; it bounds neither a single write
        // nor disk growth between polls. The fixed diagnostic has no plugin code.
        poll = setInterval(() => {
          try {
            if (fs.fstatSync(fd).size > 4096) fail(new Error('sandbox descendant output exceeds 4096 bytes'))
          } catch (error) { fail(error) }
        }, 25)
        child.once('error', fail)
        child.once('close', (code, signal) => {
          if (settled) return
          if (failure) { finish(failure); return }
          try {
            if (code !== 0 || signal) throw new Error('sandbox descendant has no exact denial receipt')
            if (fs.fstatSync(fd).size > 4096) throw new Error('sandbox descendant output exceeds 4096 bytes')
            const output = Buffer.alloc(4097)
            let bytes = 0
            while (bytes < output.length) {
              // Inherited file handles share their offset; always read from
              // the beginning explicitly, including after a short read.
              const read = fs.readSync(fd, output, bytes, output.length - bytes, bytes)
              if (read === 0) break
              bytes += read
            }
            if (bytes > 4096) throw new Error('sandbox descendant output exceeds 4096 bytes')
            if (JSON.stringify(JSON.parse(output.subarray(0, bytes).toString())) !== JSON.stringify(receipt)) throw new Error('sandbox descendant has no exact denial receipt')
            finish()
          } catch (error) { finish(error) }
        })
      })
    } finally {
      try { fs.closeSync(fd) } finally { fs.unlinkSync(outputPath) }
    }
  }
  return receipt
}
