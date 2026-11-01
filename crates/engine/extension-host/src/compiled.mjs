// A compiled launch uses the same canonical host entry as Node/system Bun.
// These values come from compile-host.mjs, never from the environment.
const args = process.argv.slice(2)
if (args.length === 1 && args[0] === '--codewhale-host-info') {
  console.log(JSON.stringify({
    kind: 'codewhale-extension-host',
    runtime: 'bun',
    platform: process.platform,
    arch: process.arch,
    version: process.versions.bun,
    bundle_sha256: CODEWHALE_COMPILED_BUNDLE_SHA256,
  }))
} else if (args.length === 1 && args[0] === '--version') {
  console.log(process.versions.bun)
} else {
  await import('../dist/codewhale-extension-host.mjs')
}
