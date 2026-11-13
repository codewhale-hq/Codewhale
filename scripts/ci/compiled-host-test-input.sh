#!/usr/bin/env bash
set -euo pipefail
bun_binary="$(bun -p 'process.execPath')"
host_binary="$(node -p "require('path').join(process.env.RUNNER_TEMP, 'codewhale-extension-host' + (process.platform === 'win32' ? '.exe' : ''))")"
compiled_info="$(node crates/engine/extension-host/compile-host.mjs --bun "$bun_binary" --output "$host_binary")"
printf 'CODEWHALE_COMPILED_HOST_TEST_BINARY=%s\n' "$host_binary" >> "$GITHUB_ENV"
node - "$host_binary" "$bun_binary" "$compiled_info" >> "$GITHUB_ENV" <<'JS'
const fs = require('node:fs'), crypto = require('node:crypto');
const digest = (file) => crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
const info = JSON.parse(process.argv[4]);
if (!info.platform || !info.arch || info.platform !== info.compiler?.platform || info.arch !== info.compiler?.arch) throw new Error('image/compiler identity mismatch');
console.log('CODEWHALE_COMPILED_HOST_TEST_PLATFORM=' + info.platform);
console.log('CODEWHALE_COMPILED_HOST_TEST_ARCH=' + info.arch);
console.log('CODEWHALE_COMPILED_HOST_TEST_SHA256=' + digest(process.argv[2]));
console.log('CODEWHALE_COMPILED_HOST_TEST_BUN=' + process.argv[3]);
console.log('CODEWHALE_COMPILED_HOST_TEST_RUNTIME_SHA256=' + digest(process.argv[3]));
console.log('CODEWHALE_COMPILED_HOST_TEST_BUNDLE_SHA256=' + digest('crates/engine/extension-host/dist/codewhale-extension-host.mjs'));
JS
