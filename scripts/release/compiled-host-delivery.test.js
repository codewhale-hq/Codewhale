const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const test = require('node:test');
const hosts = require('../../npm/codewhale/scripts/compiled-hosts');
const artifacts = require('../../npm/codewhale/scripts/artifacts');
const { assemble, verifyAssetDirectory } = require('./assemble-release-assets');
const { fetchProof } = require('./fetch-compiled-host-proof');
const root = path.resolve(__dirname, '../..');
const source = 'b'.repeat(40), bundle = 'a'.repeat(64);
function catalog(target = 'linux-x64') {
  const names = hosts.names(target), payloads = { [names.binary]: Buffer.from('tested-image'), [names.notices]: Buffer.from('notices-fixture'), [names.source]: Buffer.from('source-fixture') };
  const value = { schema: 1, version: '0.10.1', source_sha: source, bundle_sha256: bundle, hosts: [{ target, asset: names.binary, sha256: hosts.sha256(payloads[names.binary]), notices_asset: names.notices, notices_sha256: hosts.sha256(payloads[names.notices]), source_asset: names.source, source_sha256: hosts.sha256(payloads[names.source]), runtime_version: '1.4.0', runtime_revision: 'c'.repeat(40), runtime_sha256: 'd'.repeat(64), webkit_revision: 'e'.repeat(40), bundle_sha256: bundle, source_commit: source, native_platform: hosts.TARGETS[target][0], native_arch: hosts.TARGETS[target][1], libc: target.startsWith('linux-') ? 'glibc' : 'none', passed: hosts.compiledMinimum(target), failed: 0, skipped: 0, test_log_sha256: 'f'.repeat(64), native_passed: hosts.nativeMinimum(target), native_failed: 0, native_skipped: 0, native_log_sha256: '1'.repeat(64), license_closure: 'complete', relink_source: 'complete' }] };
  return { value, payloads };
}
function temp(t) { const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cw-delivery-')); t.after(() => fs.rmSync(dir, { recursive: true, force: true })); return dir; }
function writeCatalog(dir, f) { fs.mkdirSync(dir, { recursive: true }); for (const [name, bytes] of Object.entries(f.payloads)) fs.writeFileSync(path.join(dir, name), bytes); fs.writeFileSync(path.join(dir, hosts.HOST_CATALOG), JSON.stringify(f.value)); }
function baselineInputs(input) {
  const bundles = path.join(input, 'codewhale-bundles'); fs.mkdirSync(bundles, { recursive: true });
  const rows = [];
  for (const name of artifacts.allReleaseAssetNames()) {
    if ([artifacts.CHECKSUM_MANIFEST, artifacts.BUNDLE_CHECKSUM_MANIFEST, 'codewhale.bat'].includes(name)) continue;
    const dir = artifacts.BUNDLE_ASSET_NAMES.includes(name) ? bundles : path.join(input, name); fs.mkdirSync(dir, { recursive: true });
    const bytes = Buffer.from('fixture-' + name); fs.writeFileSync(path.join(dir, name), bytes);
    if (artifacts.BUNDLE_ASSET_NAMES.includes(name)) rows.push(`${hosts.sha256(bytes)}  ${name}`);
  }
  fs.writeFileSync(path.join(bundles, artifacts.BUNDLE_CHECKSUM_MANIFEST), rows.sort().join('\n') + '\n');
}
test('qualified optional assembly extends exact inventory and verifies all payload digests', async t => {
  const dir = temp(t), input = path.join(dir, 'input'), output = path.join(dir, 'output'), f = catalog();
  baselineInputs(input); writeCatalog(path.join(input, 'codewhale-compiled-hosts'), f);
  await assemble(input, output); await verifyAssetDirectory(output);
  assert.equal(fs.readdirSync(output).length, 42);
  assert.deepEqual(fs.readdirSync(output).sort(), artifacts.allReleaseAssetNames(f.value).sort());
  fs.writeFileSync(path.join(output, f.value.hosts[0].notices_asset), 'changed');
  await assert.rejects(verifyAssetDirectory(output), /checksum|SHA256/);
});
test('collector retains same tested bytes, rejects mixed source and has no implicit empty eligibility', t => {
  const dir = temp(t), input = path.join(dir, 'input'), output = path.join(dir, 'output'), first = catalog(), second = catalog('macos-arm64');
  writeCatalog(path.join(input, 'codewhale-compiled-host-linux-x64'), first);
  writeCatalog(path.join(input, 'codewhale-compiled-host-macos-arm64'), second);
  const invoke = () => spawnSync(process.execPath, [path.join(__dirname, 'stage-compiled-host.mjs'), '--collect', input, '--output-dir', output], { encoding: 'utf8' });
  assert.equal(invoke().status, 0);
  const combined = hosts.parseCatalog(fs.readFileSync(path.join(output, hosts.HOST_CATALOG)));
  assert.equal(combined.hosts.length, 2); hosts.verifyDirectory(output, combined);
  second.value.source_sha = '3'.repeat(40); second.value.hosts[0].source_commit = second.value.source_sha;
  writeCatalog(path.join(input, 'codewhale-compiled-host-macos-arm64'), second);
  assert.match(invoke().stderr, /mixed release source_sha/);
  fs.rmSync(input, { recursive: true }); fs.mkdirSync(input);
  assert.match(invoke().stderr, /no qualified/);
});
test('CI collector accepts only green exact official workflow and same contained image/log', t => {
  const dir = temp(t), output = path.join(dir, 'proof'), image = Buffer.from('native-tested'), log = Buffer.from('actual-native-log');
  let run = { workflow_id: 42, path: '.github/workflows/ci.yml', head_sha: source, status: 'completed', conclusion: 'success', repository: { full_name: 'codewhale-hq/CodeWhale' } };
  let requestedArtifact, nativePassed = hosts.nativeMinimum('linux-x64');
  const exec = (_binary, args) => {
    if (args[0] === 'run') {
      requestedArtifact = args[args.indexOf('--name') + 1];
      fs.writeFileSync(path.join(output, 'codewhale-extension-host'), image);
      fs.writeFileSync(path.join(output, 'native-containment.log'), log);
      fs.writeFileSync(path.join(output, 'native-receipt.json'), JSON.stringify({ scope: 'native-compiled-host', source_sha: source, platform: 'linux', arch: 'x64', passed: nativePassed, failed: 0, skipped: 0, host_sha256: hosts.sha256(image), log: 'native-containment.log', log_sha256: hosts.sha256(log) }));
      return '';
    }
    if (args[1].endsWith('/ci.yml')) return JSON.stringify({ id: 42 });
    if (args[1].includes('/artifacts?')) return JSON.stringify([{ artifacts: [{ name: 'native-compiled-host-Linux-X64', expired: false }] }]);
    return JSON.stringify(run);
  };
  const request = { repo: 'codewhale-hq/CodeWhale', runId: '123', sourceSha: source, target: 'linux-x64', output };
  assert.equal(fetchProof(request, exec).image, path.join(output, 'codewhale-extension-host'));
  assert.equal(requestedArtifact, 'native-compiled-host-Linux-X64');
  fs.rmSync(output, { recursive: true }); nativePassed = 1;
  assert.throws(() => fetchProof(request, exec), /qualify this exact native target/);
  nativePassed = hosts.nativeMinimum('linux-x64');
  fs.rmSync(output, { recursive: true }); run = { ...run, head_sha: '4'.repeat(40) };
  assert.throws(() => fetchProof(request, exec), /exact source/);
  run = { ...run, head_sha: source, conclusion: 'failure' }; assert.throws(() => fetchProof(request, exec), /green official/);
});
test('archive companion is opt-in and missing qualification refuses before command writes', t => {
  const dir = temp(t), archive = path.join(dir, 'archive'), home = path.join(dir, 'home'); fs.mkdirSync(archive); fs.mkdirSync(home);
  fs.copyFileSync(path.join(__dirname, 'install.sh'), path.join(archive, 'install.sh'));
  for (const name of ['codewhale', 'codew']) fs.writeFileSync(path.join(archive, name), '#!/bin/sh\nexit 0\n', { mode: 0o755 });
  const attempt = spawnSync('bash', [path.join(archive, 'install.sh')], { env: { ...process.env, HOME: home, CODEWHALE_INSTALL_COMPILED_HOST: '1' }, encoding: 'utf8' });
  assert.notEqual(attempt.status, 0); assert.match(attempt.stderr, /no complete qualified payload/);
  assert.equal(fs.existsSync(path.join(home, '.local/bin/codewhale')), false);
});
test('whole platform archives contain only their own companion and Windows shared installer', t => {
  const dir = temp(t), input = path.join(dir, 'input'), output = path.join(dir, 'output'), f = catalog();
  baselineInputs(input); writeCatalog(path.join(input, 'codewhale-compiled-hosts'), f);
  const result = spawnSync('bash', [path.join(__dirname, 'create-release-bundles.sh'), input, output], { cwd: root, env: { ...process.env, SOURCE_DATE_EPOCH: '1728000000' }, encoding: 'utf8' });
  assert.equal(result.status, 0, result.stderr);
  const list = name => spawnSync(name.endsWith('.zip') ? 'unzip' : 'tar', name.endsWith('.zip') ? ['-Z1', path.join(output, name)] : ['-tzf', path.join(output, name)], { encoding: 'utf8' }).stdout;
  const linux = list('codewhale-linux-x64.tar.gz'); assert.match(linux, /codewhale-extension-host\.relink-source\.tar\.gz/); assert.match(linux, /codewhale-extension-host\.release\.json/);
  assert.doesNotMatch(list('codewhale-android-arm64.tar.gz'), /codewhale-extension-host/);
  assert.match(list('codewhale-windows-x64.zip'), /install\.ps1/);
});

test('runtime names and completeness flags alone cannot qualify missing notice texts', { skip: process.platform === 'win32' ? 'Unix executable fixture; actual Windows Native compiled-image proof is a separate required gate' : false }, t => {
  const dir = temp(t), bun = path.join(dir, 'local-bun'), notices = path.join(dir, 'notices.txt'), sourceArchive = path.join(dir, 'source.tar.gz'), closure = path.join(dir, 'closure.json');
  const revision = 'c'.repeat(40), version = '1.4.0';
  fs.writeFileSync(bun, `#!/bin/sh\nif [ \"$1\" = \"--revision\" ]; then printf '${version}+${revision.slice(0, 9)}\\n'; else printf '${JSON.stringify({ platform: process.platform, arch: process.arch })}\\n'; fi\n`, { mode: 0o755 });
  fs.writeFileSync(notices, 'MIT alone is not a complete Bun notice set'); fs.writeFileSync(sourceArchive, 'not a source closure');
  const bundleFile = path.join(root, 'crates/engine/extension-host/dist/codewhale-extension-host.mjs');
  const receipt = { schema: 1, runtime_version: version, runtime_revision: revision, runtime_sha256: hosts.sha256(fs.readFileSync(bun)), webkit_revision: 'e'.repeat(40), libc: process.platform === 'linux' ? 'glibc' : 'none', bundle_sha256: hosts.sha256(fs.readFileSync(bundleFile)), source_commit: source, notices: 'notices.txt', notices_sha256: hosts.sha256(fs.readFileSync(notices)), relink_source: 'source.tar.gz', source_sha256: hosts.sha256(fs.readFileSync(sourceArchive)), license_closure: 'complete', corresponding_source: 'complete', components: ['bun', 'JavaScriptCore', 'WebCore', 'tinycc', 'boringssl'] };
  fs.writeFileSync(closure, JSON.stringify(receipt));
  const target = Object.keys(hosts.TARGETS).find(name => hosts.TARGETS[name][0] === process.platform && hosts.TARGETS[name][1] === process.arch);
  assert.ok(target, 'test must run on a supported native delivery platform');
  const result = spawnSync(process.execPath, [path.join(__dirname, 'stage-compiled-host.mjs'), '--target', target, '--bun', bun, '--runtime-closure', closure, '--version', '0.10.1', '--source-sha', source, '--output-dir', path.join(dir, 'out')], { encoding: 'utf8' });
  assert.notEqual(result.status, 0); assert.match(result.stderr, /incomplete runtime component closure/);
  assert.equal(fs.existsSync(path.join(dir, 'out')), false);
});

test('executed Bun architecture cannot inherit an ARM runner label under emulation', { skip: process.platform === 'win32' ? 'Unix runtime executable fixture; Windows producer qualification remains required' : false }, t => {
  const dir = temp(t), bun = path.join(dir, 'foreign-bun'), closure = path.join(dir, 'closure.json');
  const revision = 'c'.repeat(40), version = '1.4.0';
  const wrongArch = process.arch === 'arm64' ? 'x64' : 'arm64';
  fs.writeFileSync(bun, `#!/bin/sh\nif [ "$1" = "--revision" ]; then printf '${version}+${revision.slice(0, 9)}\\n'; else printf '${JSON.stringify({ platform: process.platform, arch: wrongArch })}\\n'; fi\n`, { mode: 0o755 });
  const bundleFile = path.join(root, 'crates/engine/extension-host/dist/codewhale-extension-host.mjs');
  fs.writeFileSync(closure, JSON.stringify({ schema: 1, runtime_version: version, runtime_revision: revision, runtime_sha256: hosts.sha256(fs.readFileSync(bun)), bundle_sha256: hosts.sha256(fs.readFileSync(bundleFile)), source_commit: source }));
  const target = Object.keys(hosts.TARGETS).find(name => hosts.TARGETS[name][0] === process.platform && hosts.TARGETS[name][1] === process.arch);
  assert.ok(target);
  const result = spawnSync(process.execPath, [path.join(__dirname, 'stage-compiled-host.mjs'), '--target', target, '--bun', bun, '--runtime-closure', closure, '--version', '0.10.1', '--source-sha', source, '--output-dir', path.join(dir, 'out')], { encoding: 'utf8' });
  assert.notEqual(result.status, 0); assert.match(result.stderr, /selected Bun process identity.*emulation proof does not transfer/);
  assert.equal(fs.existsSync(path.join(dir, 'out')), false);
});
