#!/usr/bin/env node

// Offline shell proofs. Every destination and download is a disposable fixture;
// curl, uname, and sudo are intercepted so no real installation or network runs.
const assert = require("node:assert/strict");
const { spawnSync } = require("node:child_process");
const crypto = require("node:crypto");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");

const repo = path.resolve(__dirname, "../..");
const bytes = '#!/bin/sh\necho "executed $*" >> "$INSTALL_TEST_EXECUTED"\necho "codewhale 0.9.11"\n';

function executable(file, body) {
  fs.writeFileSync(file, body, { mode: 0o755 });
}

function fixture(t, kind) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "cw-install-safety-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const home = path.join(root, "home");
  const bin = path.join(root, "tools");
  const archive = path.join(root, "archive");
  const assets = path.join(root, "assets");
  for (const dir of [home, bin, archive, assets]) fs.mkdirSync(dir);
  fs.copyFileSync(path.join(repo, "scripts/release/install.sh"), path.join(archive, "install.sh"));
  for (const name of ["codewhale", "codew"]) {
    executable(path.join(archive, name), bytes);
    executable(path.join(assets, `${name}-macos-arm64`), bytes);
  }
  const hash = crypto.createHash("sha256").update(bytes).digest("hex");
  fs.writeFileSync(path.join(assets, "codewhale-artifacts-sha256.txt"),
    `${hash}  codewhale-macos-arm64\n${hash}  codew-macos-arm64\n`);
  executable(path.join(bin, "uname"), '#!/bin/sh\ncase "$1" in -s) echo Darwin ;; -m) echo arm64 ;; esac\n');
  executable(path.join(bin, "sudo"), '#!/bin/sh\necho sudo >> "$INSTALL_TEST_EXECUTED"\nexit 97\n');
  executable(path.join(bin, "codewhale"), '#!/bin/sh\necho shadow >> "$INSTALL_TEST_EXECUTED"\nexit 98\n');
  executable(path.join(bin, "curl"), `#!/bin/sh
url=""; output=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o) shift; output="$1" ;;
    http*) url="$1" ;;
  esac
  shift
done
printf '%s\n' "$url" >> "$INSTALL_TEST_DOWNLOADS"
asset="$(basename "$url")"
cp "$INSTALL_TEST_ASSETS/$asset" "$output"
`);
  const destination = path.join(home, ".local", "bin");
  fs.mkdirSync(destination, { recursive: true });
  fs.chmodSync(destination, 0o755);
  const env = { ...process.env, HOME: home, PATH: `${bin}:${process.env.PATH}`, CI: "1",
    CODEWHALE_VERSION: "v0.9.11", CODEWHALE_INSTALL_DIR: destination,
    PREFIX: path.dirname(destination), INSTALL_TEST_ASSETS: assets,
    INSTALL_TEST_EXECUTED: path.join(root, "executed"),
    INSTALL_TEST_DOWNLOADS: path.join(root, "downloads") };
  for (const key of ["CODEWHALE_RELEASE_BASE_URL", "DEEPSEEK_TUI_RELEASE_BASE_URL", "CODEWHALE_SKIP_GLIBC_CHECK", "DEEPSEEK_TUI_SKIP_GLIBC_CHECK", "DEEPSEEK_SKIP_GLIBC_CHECK", "TERMUX_VERSION", "CODEWHALE_HOME", "CODEWHALE_FORCE_REINSTALL", "CODEWHALE_INSTALL_COMPILED_HOST", "SHELL", "ZDOTDIR"]) delete env[key];
  function run() {
    const script = kind === "website" ? path.join(repo, "web/public/install.sh") : path.join(archive, "install.sh");
    return spawnSync(kind === "website" ? "sh" : "bash", [script], { env, encoding: "utf8", timeout: 10000 });
  }
  function prepare() { fs.mkdirSync(env.CODEWHALE_INSTALL_DIR, { recursive: true }); }
  function untouched() {
    const runs = fs.existsSync(env.INSTALL_TEST_EXECUTED) ? fs.readFileSync(env.INSTALL_TEST_EXECUTED, "utf8").trim().split("\n") : [];
    assert.ok(runs.every(line => line === "executed doctor --installs --brief"), `installers must not execute existing files or sudo: ${runs.join(", ")}`);
  }
  return { root, home, bin, archive, assets, destination, env, run, prepare, untouched };
}

for (const kind of ["website", "archive"]) {
  test(`${kind}: fresh installation verifies bytes, executable modes, and PATH shadowing`, t => {
    const f = fixture(t, kind);
    const result = f.run();
    assert.equal(result.status, 0, result.stderr);
    for (const name of ["codewhale", "codew"]) {
      const installed = path.join(f.destination, name);
      assert.equal(fs.readFileSync(installed, "utf8"), bytes);
      assert.ok(fs.statSync(installed).mode & 0o111);
    }
    const receipt = fs.readFileSync(path.join(f.destination, ".codewhale-install.sha256"), "utf8");
    const hash = crypto.createHash("sha256").update(bytes).digest("hex");
    assert.equal(receipt, `${hash}  codewhale
${hash}  codew
`);
    assert.match(result.stdout, /PATH selects/);
    assert.ok(result.stdout.includes(`"${fs.realpathSync(f.destination)}/codewhale" update`), result.stdout);
    if (kind === "website") {
      const downloads = fs.readFileSync(f.env.INSTALL_TEST_DOWNLOADS, "utf8").trim().split("\n");
      assert.equal(downloads.length, 2);
      assert.ok(downloads.some(url => url.endsWith("/codewhale-macos-arm64")));
      assert.ok(!downloads.some(url => url.endsWith("/codew-macos-arm64")));
      assert.ok(downloads.every(url => url.startsWith("https://github.com/codewhale-hq/CodeWhale/releases/download/v0.9.11/")));
    }
    f.untouched();
  });

  test(`${kind}: rerunning an identical installation preserves its files`, t => {
    const f = fixture(t, kind);
    assert.equal(f.run().status, 0);
    const file = path.join(f.destination, "codewhale");
    const before = fs.statSync(file);
    const result = f.run();
    assert.equal(result.status, 0, result.stderr);
    assert.equal(fs.statSync(file).ino, before.ino);
    assert.equal(fs.readFileSync(file, "utf8"), bytes);
    f.untouched();
  });

  test(`${kind}: refuses a non-Codewhale file at the destination without executing it`, t => {
    const f = fixture(t, kind); f.prepare();
    const primary = path.join(f.destination, "codewhale");
    const newer = bytes.replace("0.9.11", "0.9.12");
    executable(primary, newer);
    const result = f.run();
    assert.notEqual(result.status, 0);
    assert.ok(result.stderr.includes(primary), result.stderr);
    assert.match(result.stderr, /fresh (directory|prefix)/);
    assert.equal(fs.readFileSync(primary, "utf8"), newer);
    assert.equal(fs.existsSync(path.join(f.destination, "codew")), false);
    f.untouched();
  });

  for (const name of ["codew", "codewhale-tui"]) {
    test(`${kind}: existing ${name} follows current command ownership`, t => {
      const f = fixture(t, kind); f.prepare();
      const file = path.join(f.destination, name);
      executable(file, "unrelated bytes");
      const result = f.run();
      assert.equal(fs.readFileSync(file, "utf8"), "unrelated bytes");
      if (name === "codewhale-tui") {
        assert.equal(result.status, 0, result.stderr);
        assert.equal(fs.readFileSync(path.join(f.destination, "codewhale"), "utf8"), bytes);
      } else {
        assert.notEqual(result.status, 0);
        assert.ok(result.stderr.includes(file), result.stderr);
        assert.equal(fs.existsSync(path.join(f.destination, "codewhale")), false);
      }
      f.untouched();
    });
  }

  test(`${kind}: refuses a symlink destination and preserves its target`, t => {
    const f = fixture(t, kind); f.prepare();
    const target = path.join(f.root, "foreign");
    executable(target, "unrelated bytes");
    const alias = path.join(f.destination, "codew");
    fs.symlinkSync(target, alias);
    assert.notEqual(f.run().status, 0);
    assert.ok(fs.lstatSync(alias).isSymbolicLink());
    assert.equal(fs.readFileSync(target, "utf8"), "unrelated bytes");
    assert.equal(fs.existsSync(path.join(f.destination, "codewhale")), false);
    f.untouched();
  });

  test(`${kind}: managed directories are refused without invoking sudo`, t => {
    const f = fixture(t, kind);
    f.env.PREFIX = path.join(f.home, ".cargo");
    f.env.CODEWHALE_INSTALL_DIR = path.join(f.env.PREFIX, "bin");
    const result = f.run();
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /managed\/system/);
    assert.equal(fs.existsSync(path.join(f.env.CODEWHALE_INSTALL_DIR, "codewhale")), false);
    f.untouched();
  });

  test(`${kind}: a destination created during publication is never overwritten`, t => {
    const f = fixture(t, kind);
    executable(path.join(f.bin, "ln"), `#!/bin/sh
destination="$2$(basename "$1")"
printf 'another writer' > "$destination"
exec /bin/ln "$@"
`);
    assert.notEqual(f.run().status, 0);
    assert.equal(fs.readFileSync(path.join(f.destination, "codewhale"), "utf8"), "another writer");
    assert.equal(fs.existsSync(path.join(f.destination, "codew")), false);
    assert.equal(fs.readdirSync(f.destination).some(name => name.startsWith(".codewhale-install.")), false);
    f.untouched();
  });

  test(`${kind}: a failed second publication removes the command this run published`, t => {
    const f = fixture(t, kind);
    // Another writer claims codew between preflight and its publication.
    executable(path.join(f.bin, "ln"), `#!/bin/sh
case "$(basename "$1")" in codew) printf 'another writer' > "$2codew" ;; esac
exec /bin/ln "$@"
`);
    const result = f.run();
    assert.notEqual(result.status, 0, result.stdout);
    assert.equal(fs.existsSync(path.join(f.destination, "codewhale")), false, "no half-installed pair is left behind");
    assert.equal(fs.readFileSync(path.join(f.destination, "codew"), "utf8"), "another writer");
    assert.match(result.stderr, /did not complete/);
    assert.equal(fs.readdirSync(f.destination).some(name => name.startsWith(".codewhale-install.")), false);
    f.untouched();
  });

  test(`${kind}: rollback never removes a command that was already installed`, t => {
    const f = fixture(t, kind);
    assert.equal(f.run().status, 0);
    fs.unlinkSync(path.join(f.destination, "codew"));
    const kept = fs.statSync(path.join(f.destination, "codewhale")).ino;
    executable(path.join(f.bin, "ln"), `#!/bin/sh
case "$(basename "$1")" in codew) printf 'another writer' > "$2codew" ;; esac
exec /bin/ln "$@"
`);
    assert.notEqual(f.run().status, 0);
    assert.equal(fs.statSync(path.join(f.destination, "codewhale")).ino, kept);
    f.untouched();
  });

  for (const collision of ["directory", "directory symlink"]) {
    test(`${kind}: a raced ${collision} cannot redirect publication`, t => {
      const f = fixture(t, kind);
      const foreign = path.join(f.root, "foreign-directory");
      fs.mkdirSync(foreign);
      f.env.INSTALL_TEST_FOREIGN = foreign;
      const create = collision === "directory"
        ? 'mkdir "$destination"'
        : '/bin/ln -s "$INSTALL_TEST_FOREIGN" "$destination"';
      executable(path.join(f.bin, "ln"), `#!/bin/sh
 destination="$2$(basename "$1")"
 ${create}
 exec /bin/ln "$@"
 `);
      const result = f.run();
      assert.notEqual(result.status, 0, result.stdout);
      assert.doesNotMatch(result.stdout, /Installed checksummed|Done\. Commands/);
      const target = path.join(f.destination, "codewhale");
      assert.equal(fs.lstatSync(target).isSymbolicLink(), collision === "directory symlink");
      assert.deepEqual(fs.readdirSync(target), []);
      assert.deepEqual(fs.readdirSync(foreign), []);
      assert.equal(fs.existsSync(path.join(f.destination, "codew")), false);
      assert.equal(fs.readdirSync(f.destination).some(name => name.startsWith(".codewhale-install.")), false);
      f.untouched();
    });
  }
}

test("website: a checksum mismatch stops before any installation", t => {
  const f = fixture(t, "website");
  fs.writeFileSync(path.join(f.assets, "codewhale-macos-arm64"), "tampered");
  const result = f.run();
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /checksum mismatch/);
  assert.equal(fs.existsSync(path.join(f.destination, "codewhale")), false);
  f.untouched();
});

test("archive: a missing second binary stops before the first installation", t => {
  const f = fixture(t, "archive");
  fs.unlinkSync(path.join(f.archive, "codew"));
  const result = f.run();
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /not found in archive/);
  assert.equal(fs.existsSync(path.join(f.destination, "codewhale")), false);
  f.untouched();
});

test("website: Termux never downloads a Linux binary", t => {
  const f = fixture(t, "website");
  f.env.TERMUX_VERSION = "fixture";
  const result = f.run();
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /Android\/Termux needs the Android/);
  assert.equal(fs.existsSync(f.env.INSTALL_TEST_DOWNLOADS), false);
  assert.equal(fs.existsSync(path.join(f.destination, "codewhale")), false);
  f.untouched();
});

function addHostPayload(f) {
  const payloads = {
    'codewhale-extension-host': '#!/bin/sh\nexit 0\n',
    'codewhale-extension-host.LICENSES.txt': 'offline runtime notice fixture\n',
    'codewhale-extension-host.relink-source.tar.gz': 'offline relink fixture\n',
    'codewhale-extension-host.release.json': '{"fixture":"already qualified by release producer"}\n',
  };
  let manifest = fs.readFileSync(path.join(f.assets, 'codewhale-artifacts-sha256.txt'), 'utf8');
  for (const [name, content] of Object.entries(payloads)) {
    fs.writeFileSync(path.join(f.archive, name), content, { mode: name === 'codewhale-extension-host' ? 0o755 : 0o644 });
    const asset = name === 'codewhale-extension-host.release.json' ? 'codewhale-extension-hosts.json' : name.replace('codewhale-extension-host', 'codewhale-extension-host-macos-arm64').replace('.LICENSES.txt', '-LICENSES.txt').replace('.relink-source.tar.gz', '-relink-source.tar.gz');
    fs.writeFileSync(path.join(f.assets, asset), content);
    manifest += `${crypto.createHash('sha256').update(content).digest('hex')}  ${asset}\n`;
  }
  fs.writeFileSync(path.join(f.assets, 'codewhale-artifacts-sha256.txt'), manifest);
  return payloads;
}
for (const kind of ['website', 'archive']) {
  test(`${kind}: qualified companion presence stays inert until explicit installer choice`, t => {
    const f = fixture(t, kind), payloads = addHostPayload(f);
    f.env.CODEWHALE_INSTALL_COMPILED_HOST = '0';
    assert.equal(f.run().status, 0);
    assert.equal(fs.existsSync(path.join(f.destination, 'codewhale-extension-host')), false);
    f.env.CODEWHALE_INSTALL_COMPILED_HOST = '1';
    const result = f.run(); assert.equal(result.status, 0, result.stderr);
    for (const [name, content] of Object.entries(payloads)) {
      const file = path.join(f.destination, name);
      assert.equal(fs.readFileSync(file, 'utf8'), content);
      assert.equal(fs.statSync(file).mode & 0o777, name === 'codewhale-extension-host' ? 0o755 : 0o644);
    }
    f.untouched();
  });
  test(`${kind}: a conflicting companion prevents the first command publication`, t => {
    const f = fixture(t, kind); addHostPayload(f); f.prepare(); f.env.CODEWHALE_INSTALL_COMPILED_HOST = '1';
    const foreign = path.join(f.destination, 'codewhale-extension-host.LICENSES.txt'); fs.writeFileSync(foreign, 'foreign');
    const result = f.run(); assert.notEqual(result.status, 0); assert.ok(result.stderr.includes(foreign), result.stderr);
    assert.equal(fs.readFileSync(foreign, 'utf8'), 'foreign'); assert.equal(fs.existsSync(path.join(f.destination, 'codewhale')), false);
    f.untouched();
  });
  test(`${kind}: late companion collision rolls back every new command and image`, t => {
    const f = fixture(t, kind); addHostPayload(f); f.env.CODEWHALE_INSTALL_COMPILED_HOST = '1';
    executable(path.join(f.bin, 'ln'), `#!/bin/sh\ncase "$(basename "$1")" in codewhale-extension-host.LICENSES.txt) printf 'another writer' > "$2codewhale-extension-host.LICENSES.txt" ;; esac\nexec /bin/ln "$@"\n`);
    const result = f.run(); assert.notEqual(result.status, 0, result.stdout);
    for (const name of ['codewhale', 'codew', 'codewhale-extension-host', 'codewhale-extension-host.release.json']) assert.equal(fs.existsSync(path.join(f.destination, name)), false, name);
    assert.equal(fs.readFileSync(path.join(f.destination, 'codewhale-extension-host.LICENSES.txt'), 'utf8'), 'another writer');
    f.untouched();
  });
}
