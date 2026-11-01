// Ambient Node file access from inside a host plugin. Under the host sandbox
// the read of a Codewhale secret must fail even though the plugin is trusted.
import { readFile, writeFile, readlink } from 'node:fs/promises'
import { connect } from 'node:net'
import { createSocket } from 'node:dgram'
import { execFile } from 'node:child_process'

export const name = 'secret-probe'
export const inject = ['tools']

export function apply(ctx) {
  ctx.tools.register({
    name: 'probe_read',
    description: 'Read a file with node:fs and report what happened.',
    parameters: { type: 'object', properties: { path: { type: 'string' }, child: { type: 'boolean' } }, required: ['path'] },
    async execute(args) {
      try {
        if (args.child) return await new Promise((resolve, reject) => {
          execFile(process.execPath, ['-e', "try { process.stdout.write(JSON.stringify({ ok: true, text: require('node:fs').readFileSync(process.argv[1], 'utf8') })) } catch (error) { process.stdout.write(JSON.stringify({ ok: false, code: error.code })) }", args.path], { encoding: 'utf8', timeout: 5000, maxBuffer: 4096 }, (error, stdout) => {
            if (error) reject(error)
            else { try { resolve(JSON.parse(stdout)) } catch (error) { reject(error) } }
          })
        })
        return { ok: true, text: await readFile(args.path, 'utf8') }
      } catch (error) {
        return { ok: false, code: error.code ?? String(error) }
      }
    },
  })
  ctx.tools.register({
    name: 'probe_write',
    description: 'Write a file with node:fs and report what happened.',
    parameters: { type: 'object', properties: { path: { type: 'string' } }, required: ['path'] },
    async execute(args) {
      try {
        await writeFile(args.path, 'probe')
        return { ok: true }
      } catch (error) {
        return { ok: false, code: error.code ?? String(error) }
      }
    },
  })
  ctx.tools.register({
    name: 'probe_connect',
    description: 'Connect to the test controller loopback listener and report the OS result.',
    parameters: { type: 'object', properties: { port: { type: 'integer', minimum: 1, maximum: 65535 } }, required: ['port'] },
    async execute(args) {
      // Linux's private loopback cannot reach the controller's listener.
      // Report the actual kernel namespace, rather than infer isolation from errno.
      const networkNamespace = process.platform === 'linux'
        ? await readlink('/proc/self/ns/net')
        : undefined
      return new Promise((resolve) => {
        const socket = connect({ host: '127.0.0.1', port: args.port })
        const finish = (result) => {
          socket.destroy()
          resolve(networkNamespace === undefined ? result : { ...result, network_namespace: networkNamespace })
        }
        socket.once('connect', () => finish({ ok: true }))
        socket.once('error', (error) => finish({ ok: false, code: error.code ?? String(error) }))
        socket.setTimeout(1000, () => finish({ ok: false, code: 'ETIMEDOUT' }))
      })
    },
  })
  ctx.tools.register({
    name: 'probe_send',
    description: 'Send a datagram to the test controller and report the unmodified OS result.',
    parameters: { type: 'object', properties: { port: { type: 'integer', minimum: 1, maximum: 65535 } }, required: ['port'] },
    async execute(args) {
      return new Promise((resolve) => {
        const socket = createSocket('udp4')
        let finished = false
        const finish = (result) => {
          if (finished) return
          finished = true
          clearTimeout(timer)
          // A refused bind leaves the socket unopened; cleanup must not replace
          // the original access-denied result with ERR_SOCKET_DGRAM_NOT_RUNNING.
          try { socket.close() } catch {}
          resolve(result)
        }
        const failure = (error) => finish({ ok: false, code: error.code ?? String(error), syscall: error.syscall ?? null })
        const timer = setTimeout(() => finish({ ok: false, code: 'ETIMEDOUT' }), 1000)
        socket.once('error', failure)
        try {
          socket.send('native-network-probe', args.port, '127.0.0.1', (error) => {
            if (error) failure(error)
            else finish({ ok: true })
          })
        } catch (error) { failure(error) }
      })
    },
  })
}
