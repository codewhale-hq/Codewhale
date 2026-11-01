#!/usr/bin/env node

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const { execFileSync } = require("node:child_process");

const repoRoot = path.resolve(__dirname, "..", "..");
const {
  allAssetNames,
  allReleaseAssetNames,
  BUNDLE_ASSET_NAMES,
  LEGACY_TUI_BRIDGE_ASSET_NAMES,
} = require(path.join(repoRoot, "npm", "codewhale", "scripts", "artifacts"));

function read(relativePath) {
  return fs.readFileSync(path.join(repoRoot, relativePath), "utf8");
}

function valuesForKey(source, key) {
  const expression = new RegExp(`^\\s+${key}:\\s+([^#\\s]+)\\s*$`, "gm");
  return [...source.matchAll(expression)].map((match) => match[1]);
}

function namedStep(source, name) {
  const marker = `      - name: ${name}\n`;
  const start = source.indexOf(marker);
  assert.notEqual(start, -1, `missing workflow step: ${name}`);
  const next = source.indexOf("\n      - ", start + marker.length);
  return source.slice(start, next === -1 ? source.length : next);
}

const ci = read(".github/workflows/ci.yml");
const nightly = read(".github/workflows/nightly.yml");
const candidate = read(".github/workflows/release-candidate.yml");
const artifacts = read(".github/workflows/release-artifacts.yml");
const release = read(".github/workflows/release.yml");
const parityWorkflow = read(".github/workflows/release-parity.yml");
const republish = read(".github/workflows/release-republish.yml");
const releaseDockerfile = read("packaging/docker/Dockerfile.release");
const cnb = read(".cnb.yml");
const cnbSync = read(".github/workflows/sync-cnb.yml");
const bundles = read("scripts/release/create-release-bundles.sh");
const archiveInstaller = read("scripts/release/install.sh");
const cliDispatcher = read("crates/cli/src/lib.rs");
const runbook = read("docs/RELEASE_RUNBOOK.md");

const ciTestJob = ci.slice(ci.indexOf("\n  test:\n"));
assert.match(
  ciTestJob,
  /matrix\.os == 'ubuntu-latest'.*github\.event_name == 'pull_request'/,
  "heavy pull requests must select the real Ubuntu test lane",
);
assert.match(
  ciTestJob,
  /cargo nextest run --workspace --all-features --locked --profile ci/,
  "the Ubuntu pull-request lane must run workspace nextest",
);
assert.match(
  ciTestJob,
  /name: Linux test location \(CNB\)/,
  "the non-PR CNB fallback must be named explicitly",
);

assert.match(
  namedStep(ciTestJob, "Build canonical executable for acceptance tests"),
  /cargo build -p codewhale-cli --bin codewhale --all-features --locked/,
  "Engine acceptance must build the canonical executable first",
);
const npmSmokeJob = ci.match(/^  npm-wrapper-smoke:\n([\s\S]*?)(?=^  \S)/m)?.[1];
assert.ok(npmSmokeJob, "CI must retain the required npm-wrapper job");
assert.match(
  namedStep(npmSmokeJob, "Build wrapper binaries"),
  /run: cargo build --release --locked -p codewhale-cli --bin codewhale/,
);
assert.match(
  namedStep(npmSmokeJob, "Smoke wrapper install and delegated entrypoints"),
  /run: node scripts\/release\/npm-wrapper-smoke\.js/,
);
const npmSmokeSteps = npmSmokeJob.split(/(?=^      - )/m).slice(1);
// Exercise the workflow's actual Boolean guards. A successful location echo
// must never substitute for the build/install smoke on a heavy pull request.
const npmSmokeCases = [
  // name, event, heavy, OS, trusted, cache success, execute, Linux deps, CNB
  ["own PR", "pull_request", true, "ubuntu-latest", true, true, true, true, false],
  ["fork PR", "pull_request", true, "ubuntu-latest", false, true, true, true, false],
  ["PR cache failure", "pull_request", true, "ubuntu-latest", false, false, true, true, false],
  ["light PR", "pull_request", false, "ubuntu-latest", true, true, false, false, false],
  ["manual Ubuntu", "workflow_dispatch", true, "ubuntu-latest", true, true, true, true, false],
  ["main Ubuntu", "push", true, "ubuntu-latest", true, true, false, false, true],
  ["manual macOS", "workflow_dispatch", true, "macos-latest", true, true, true, false, false],
  ["main Windows", "push", true, "windows-latest", true, true, true, false, false],
  ["main cache failure", "push", true, "windows-latest", true, false, true, false, false],
  ["light main", "push", false, "ubuntu-latest", true, true, false, false, false],
  ["schedule", "schedule", true, "ubuntu-latest", true, true, false, false, false],
];
// The sccache GitHub Actions backend is main-only, mirroring rust-cache's
// save-if: pull requests never install or enable it (cache bloat, PLAN D).
const sccacheInstallStep = "mozilla-actions/sccache-action@v0.0.11";
for (const [label, event, heavy, os, trusted, cache, execute, linuxDeps, cnb] of npmSmokeCases) {
  const ref = event === "pull_request" ? "refs/pull/1/merge" : "refs/heads/main";
  const onMain = ref === "refs/heads/main";
  const installed = execute && onMain;
  const context = {
    needs: { changes: { outputs: { heavy: String(heavy), trusted: String(trusted) } } },
    github: { event_name: event, ref },
    matrix: { os },
    steps: { sccache: { outcome: installed ? (cache ? "success" : "failure") : "skipped" } },
  };
  const jobGuard = npmSmokeJob.match(/^    if: (.+)$/m)?.[1];
  assert.ok(jobGuard, "the wrapper job must retain its event guard");
  const jobEnabled = vm.runInNewContext(jobGuard, context);
  for (const step of npmSmokeSteps) {
    const name = step.match(/^      - (?:name|uses): (.+)$/m)?.[1];
    const guard = step.match(/^        if: (.+)$/m)?.[1];
    assert.ok(name && guard, "every wrapper step must have an explicit guard");
    let expected = execute;
    if (name === "Skip npm wrapper smoke for light change") expected = !heavy;
    else if (name === "Install Linux system dependencies") expected = linuxDeps;
    else if (name === "Linux smoke location") expected = cnb;
    else if (name === sccacheInstallStep) expected = installed;
    else if (name === "Enable sccache" || name === "sccache stats") expected = installed && cache;
    assert.equal(
      Boolean(jobEnabled && vm.runInNewContext(guard, context)),
      expected,
      `${label}: ${name} must ${expected ? "execute" : "stay skipped"}`,
    );
  }
}
// The matrix itself: pull requests keep the single required Ubuntu context,
// main pushes skip the hosted-macOS leg (5-job concurrency cap), and a manual
// full-CI dispatch still covers all three platforms.
const npmSmokeMatrix = npmSmokeJob.match(/^        os: \$\{\{ fromJSON\((.+)\) \}\}$/m)?.[1];
assert.ok(npmSmokeMatrix, "the wrapper job must keep an event-keyed OS matrix");
for (const [event, expected] of [
  ["pull_request", ["ubuntu-latest"]],
  ["push", ["ubuntu-latest", "windows-latest"]],
  ["workflow_dispatch", ["ubuntu-latest", "macos-latest", "windows-latest"]],
]) {
  assert.deepEqual(
    JSON.parse(vm.runInNewContext(npmSmokeMatrix, { github: { event_name: event } })),
    expected,
    `npm wrapper smoke matrix for ${event}`,
  );
}
// Change detection: a path Rust embeds or its tests read must never classify
// light, or a "docs-only" edit skips the Rust gates that consume it. Run the
// workflow's own `case` block in bash against every include_str!/include_bytes!
// target in the tree plus the files Rust tests read at runtime.
const heavyCase = ci.match(
  /\n( +)case "\$\{path\}" in\n[\s\S]*?\n\1  \*\)\n\1    heavy=true\n\1    ;;\n\1esac\n/,
)?.[0];
assert.ok(heavyCase, "ci.yml must keep the heavy/light change classification case block");
function classify(paths) {
  const script = `while IFS= read -r path; do heavy=false\n${heavyCase}\nprintf '%s\\t%s\\n' "$heavy" "$path"; done`;
  const out = execFileSync("bash", ["-c", script], { input: `${paths.join("\n")}\n`, encoding: "utf8" });
  return new Map(out.trim().split("\n").map((line) => line.split("\t").reverse()));
}
const rustSources = execFileSync("git", ["ls-files", "-z", "--", "*.rs"], { cwd: repoRoot, encoding: "utf8" })
  .split("\0")
  .filter((file) => file && fs.existsSync(path.join(repoRoot, file)));
const includeTargets = new Set();
const includePattern =
  /include_(?:str|bytes)!\s*\(\s*(?:concat!\s*\(\s*(?:env!\s*\(\s*"(\w+)"\s*\)\s*,\s*)?)?"([^"]+)"/g;
for (const file of rustSources) {
  const text = fs.readFileSync(path.join(repoRoot, file), "utf8");
  for (const [, env, target] of text.matchAll(includePattern)) {
    let base = path.dirname(file);
    if (env === "CARGO_MANIFEST_DIR") {
      while (base !== "." && !fs.existsSync(path.join(repoRoot, base, "Cargo.toml"))) base = path.dirname(base);
    } else if (env) {
      continue; // OUT_DIR and friends are build outputs, not tracked inputs.
    }
    const resolved = path.posix.normalize(path.posix.join(base, target.replace(/^\//, "")));
    if (!resolved.endsWith(".rs") && fs.existsSync(path.join(repoRoot, resolved))) includeTargets.add(resolved);
  }
}
assert.ok(includeTargets.size > 60, `include_str! scan found only ${includeTargets.size} targets`);
assert.ok(
  [...includeTargets].some((target) => !target.startsWith("crates/")),
  "include_str! scan must see the targets outside crates/ (docs/HOOKS.md, ...)",
);
// Read with fs at test time, not embedded; keep in sync with the ci.yml arm.
const rustTestReads = [
  "docs/FLEET_WORKFLOW_TUTORIAL.md",
  "docs/examples/fleet-dogfood.toml",
  "docs/2512.24601v2.pdf",
];
const mustBeHeavy = [...includeTargets, ...rustTestReads];
const heavyVerdicts = classify(mustBeHeavy);
for (const target of mustBeHeavy) {
  assert.equal(heavyVerdicts.get(target), "true", `${target} feeds Rust and must classify heavy`);
}
const lightVerdicts = classify(["README.md", "docs/ARCHITECTURE.md", "CHANGELOG.md", ".github/ISSUE_TEMPLATE/bug.yml"]);
for (const [target, verdict] of lightVerdicts) {
  assert.equal(verdict, "false", `${target} is docs-only and must stay light`);
}
console.log(`Change detection OK: ${mustBeHeavy.length} Rust-consumed non-.rs paths classify heavy.`);

console.log(`Wrapper CI guards OK: ${npmSmokeCases.length} event cases, ${npmSmokeSteps.length} steps each.`);

assert.match(ci, /^  workflow_dispatch:\n    inputs:\n      expected_sha:/m);
const manualForceBlock = ci.match(
  /if \[\[ "\$\{EVENT_NAME\}" == "workflow_dispatch" \]\]; then([\s\S]*?)\n\s+if \[\[ "\$\{EVENT_NAME\}" == "schedule" \]\]; then/,
);
assert.ok(manualForceBlock, "CI must have a dedicated manual-dispatch force-full branch");
for (const output of ["heavy", "workflow", "mobile", "actions"]) {
  assert.match(manualForceBlock[1], new RegExp(`echo "${output}=true"`));
}
assert.match(manualForceBlock[1], /#EXPECTED_SHA.*-ne 40/s);
assert.match(manualForceBlock[1], /actual.*EXPECTED_SHA/s);
const expectedNightlyTargets = [
  "x86_64-unknown-linux-gnu",
  "aarch64-unknown-linux-musl",
  "x86_64-apple-darwin",
  "aarch64-apple-darwin",
  "x86_64-pc-windows-msvc",
  "aarch64-pc-windows-msvc",
].sort();
assert.deepEqual([...new Set(valuesForKey(nightly, "target"))].sort(), expectedNightlyTargets);
assert.deepEqual(
  [
    ...valuesForKey(nightly, "primary_artifact"),
    ...valuesForKey(nightly, "alias_artifact"),
  ].sort(),
  [
    "codewhale-linux-x64",
    "codew-linux-x64",
    "codewhale-linux-arm64",
    "codew-linux-arm64",
    "codewhale-macos-x64",
    "codew-macos-x64",
    "codewhale-macos-arm64",
    "codew-macos-arm64",
    "codewhale-windows-x64.exe",
    "codew-windows-x64.exe",
    "codewhale-windows-arm64.exe",
    "codew-windows-arm64.exe",
  ].sort(),
);
assert.match(
  nightly,
  /cargo build --release --locked --target \$\{\{ matrix\.target \}\} -p codewhale-cli/,
);
assert.match(nightly, /startsWith\(matrix\.target, 'x86_64-'\).*runner\.arch == 'X64'/s);
assert.match(nightly, /startsWith\(matrix\.target, 'aarch64-'\).*runner\.arch == 'ARM64'/s);
const nightlyArmMuslSetup = namedStep(nightly, "Install Linux ARM64 musl toolchain");
assert.match(nightlyArmMuslSetup, /matrix\.target == 'aarch64-unknown-linux-musl'/);
assert.match(nightlyArmMuslSetup, /apt-get install -y binutils musl-tools/);
assert.match(nightlyArmMuslSetup, /rustup target add --toolchain stable aarch64-unknown-linux-musl/);
const nightlyArmStaticSmoke = namedStep(
  nightly,
  "Verify static Linux ARM64 binary and launch",
);
assert.match(
  nightlyArmStaticSmoke,
  /matrix\.target == 'aarch64-unknown-linux-musl' && runner\.arch == 'ARM64'/,
);
assert.match(nightlyArmStaticSmoke, /readelf -l "\$\{bin_path\}"/);
assert.match(nightlyArmStaticSmoke, /grep -Fq 'INTERP'/);
assert.match(nightlyArmStaticSmoke, /"\$\{bin_path\}" --version/);
assert.doesNotMatch(nightly, /codewhale-tui/);
assert.doesNotMatch(nightly, /target\/[^\n]*\/codew(?:\.exe)?/);
assert.match(nightly, /cp "\$\{bin_path\}" "\$\{dir\}\/\$\{artifact\}"/);
assert.match(nightly, /cmp -s[\s\S]*nightly-primary[\s\S]*nightly-alias/);
assert.equal((nightly.match(/retention-days: 14/g) || []).length, 2);

assert.match(candidate, /^  workflow_dispatch:\n    inputs:\n      expected_sha:/m);
assert.doesNotMatch(candidate, /^  (push|pull_request|schedule):/m);
assert.match(candidate, /uses: \.\/\.github\/workflows\/release-artifacts\.yml/);
assert.match(candidate, /source_sha: \$\{\{ needs\.resolve\.outputs\.sha \}\}/);
assert.match(candidate, /^  web:\n/m);
assert.doesNotMatch(
  candidate,
  /ref: \$\{\{ needs\.resolve\.outputs\.sha \}\}/,
  "candidate jobs must checkout GITHUB_SHA, not interpolate the dispatch SHA into ref",
);
assert.match(candidate, /cache-dependency-path: web\/package-lock\.json/);
assert.match(candidate, /package-manager-cache: false/);
assert.match(candidate, /working-directory: web/);
for (const workflow of [candidate, read(".github/workflows/web.yml")]) {
  for (const command of ["npm ci", "npm test", "npm run check"]) {
    assert.ok(workflow.includes(`run: ${command}\n`), `missing web gate: ${command}`);
  }
}
// Both workflows use this gate. Preserve every check and its order: checking
// committed facts after prebuild could silently repair drift before testing it.
assert.deepEqual(JSON.parse(read("web/package.json")).scripts.check.split(" && "), [
  "npm run check:facts",
  "npm run check:latest-release",
  "npm run prebuild",
  "npm run check:docs",
  "npm run check:tokens",
  "npm run lint",
  "tsc --noEmit",
  "npm run build",
]);
assert.match(candidate, /^    needs: \[resolve, web\]$/m);
assert.match(candidate, /needs\.web\.result == 'success'/);

for (const [label, workflow] of [
  ["release candidate", candidate],
  ["shared artifact", artifacts],
]) {
  for (const forbidden of [
    /contents:\s*write/,
    /packages:\s*write/,
    /softprops\/action-gh-release/,
    /docker\/login-action/,
    /docker\/build-push-action/,
    /\bgh release\b/,
    /\bnpm publish\b/,
    /\bcargo publish\b/,
    /\bgit push\b/,
  ]) {
    assert.doesNotMatch(workflow, forbidden, `${label} workflow contains publication capability`);
  }
}

for (const [label, workflow] of [
  ["release candidate", candidate],
  ["shared artifact", artifacts],
  ["public release", release],
  ["release republish", republish],
]) {
  const remoteActions = [...workflow.matchAll(/^\s+(?:-\s+)?uses:\s+([^@\s]+)@([^#\s]+)/gm)]
    .map((match) => ({ action: match[1], ref: match[2] }))
    .filter(({ action }) => !action.startsWith("./"));
  assert.ok(remoteActions.length > 0, `${label} workflow must exercise pinned actions`);
  for (const { action, ref } of remoteActions) {
    assert.match(
      ref,
      /^[0-9a-f]{40}$/,
      `${label} action ${action} must use an audited full commit SHA`,
    );
  }
}

const republishHomebrewJob = republish.match(/\n  homebrew:\n([\s\S]*)$/);
assert.ok(republishHomebrewJob, "republish must retain a Homebrew recovery job");
const republishHomebrewCheckout = namedStep(
  republishHomebrewJob[0],
  "Checkout release infrastructure",
);
assert.match(
  republishHomebrewCheckout,
  /ref: \$\{\{ github\.event\.repository\.default_branch \}\}/,
  "Homebrew recovery must use the repaired default-branch infrastructure",
);
assert.doesNotMatch(
  republishHomebrewCheckout,
  /needs\.resolve\.outputs\.sha/,
  "Homebrew recovery must not resurrect release-tag infrastructure",
);
assert.match(republishHomebrewJob[0], /gh release download "\$\{\{ needs\.resolve\.outputs\.tag \}\}"/);
assert.match(republishHomebrewJob[0], /MANIFEST: \/tmp\/codewhale-artifacts-sha256\.txt/);

assert.match(artifacts, /^  workflow_call:/m);
assert.match(artifacts, /^permissions:\n  contents: read$/m);
const expectedTargets = [
  "x86_64-unknown-linux-musl",
  "aarch64-unknown-linux-musl",
  "aarch64-linux-android",
  "aarch64-unknown-linux-ohos",
  "x86_64-apple-darwin",
  "aarch64-apple-darwin",
  "x86_64-pc-windows-msvc",
  "aarch64-pc-windows-msvc",
].sort();
assert.deepEqual([...new Set(valuesForKey(artifacts, "target"))].sort(), expectedTargets);

const releaseMuslBuild = namedStep(artifacts, "Build static Linux binaries (musl)");
assert.match(releaseMuslBuild, /endsWith\(matrix\.target, '-unknown-linux-musl'\)/);
assert.match(releaseMuslBuild, /apt-get install -y binutils musl-tools/);
assert.match(releaseMuslBuild, /rustup target add --toolchain stable \$\{\{ matrix\.target \}\}/);
assert.match(
  releaseMuslBuild,
  /cargo build --profile dist --locked --target \$\{\{ matrix\.target \}\} -p codewhale-cli/,
);
const releaseStaticSmoke = namedStep(
  artifacts,
  "Verify static Linux binaries and launch on matching native runners",
);
assert.match(releaseStaticSmoke, /endsWith\(matrix\.target, '-unknown-linux-musl'\)/);
assert.match(
  releaseStaticSmoke,
  /startsWith\(matrix\.target, 'aarch64-'\) && runner\.arch == 'ARM64'/,
);
assert.match(releaseStaticSmoke, /readelf -l "\$\{bin_path\}"/);
assert.match(releaseStaticSmoke, /grep -Fq 'INTERP'/);
assert.match(releaseStaticSmoke, /"\$\{bin_path\}" --version/);

const builtAssetNames = [
  ...valuesForKey(artifacts, "cli_artifact"),
  ...valuesForKey(artifacts, "shim_artifact"),
  ...valuesForKey(artifacts, "compat_tui_artifact"),
];
assert.equal(builtAssetNames.length, 24);
assert.deepEqual(
  [...new Set(builtAssetNames)].sort(),
  [
    ...allAssetNames().filter((name) => name !== "codewhale.bat"),
    ...LEGACY_TUI_BRIDGE_ASSET_NAMES,
  ].sort(),
);
assert.match(
  artifacts,
  /stage_binary "\$\{\{ matrix\.cli_binary \}\}" "\$\{\{ matrix\.compat_tui_artifact \}\}"/,
  "legacy TUI bridge assets must be staged from the one compiled codewhale binary",
);
const bundleInvocations = [...bundles.matchAll(
  /^bundle (\S+) \\\n\s+\S+ \S+ (tar\.gz|zip) (""|portable)$/gm,
)].map((match) => {
  const variant = match[3] === "portable" ? "-portable" : "";
  return `codewhale-${match[1]}${variant}.${match[2]}`;
});
assert.deepEqual(bundleInvocations.sort(), [...BUNDLE_ASSET_NAMES].sort());
assert.match(artifacts, /aarch64-pc-windows-msvc/);
assert.match(artifacts, /aarch64-linux-android/);
// HarmonyOS bytes cannot be shipped unsigned: the device refuses to execute an
// unsigned ELF, so the leg must self-sign and prove the signature before
// staging. Assert the order too — signing after staging would publish the
// unsigned copy.
assert.match(artifacts, /aarch64-unknown-linux-ohos/);
const ohosSdk = namedStep(artifacts, "Install OpenHarmony native SDK");
assert.match(ohosSdk, /components: 'native;toolchains'/, "the SDK must include the binary signer");
assert.match(ohosSdk, /cache: false/, "a release must download the SDK, not publish a new cache entry");
// A step header glued onto the previous line because a `run:` block lost its
// trailing newline parses as valid YAML (the header becomes script text) and
// silently drops the step. Every `- name:` in these workflows must start its
// own line.
for (const [file, source] of [
  ["release-artifacts.yml", artifacts],
  ["release.yml", release],
  ["nightly.yml", nightly],
]) {
  for (const match of source.matchAll(/- name:/g)) {
    const lineStart = source.lastIndexOf("\n", match.index) + 1;
    assert.match(
      source.slice(lineStart, match.index),
      /^\s*$/,
      `${file}: a step header must start its own line, not follow other content`,
    );
  }
}

const ohosBuild = namedStep(artifacts, "Cross-compile and code-sign the OpenHarmony binary");
assert.match(ohosBuild, /matrix\.platform == 'ohos-arm64'/);
assert.match(ohosBuild, /binary-sign-tool\.jar/);
assert.match(ohosBuild, /-selfSign 1/);
assert.match(ohosBuild, /code signature is self-sign/);
assert.match(ohosBuild, /scripts\/ohos-env\.sh/);
assert.ok(
  ohosBuild.indexOf("codesign") === -1 ||
    ohosBuild.indexOf("binary-sign-tool.jar") < ohosBuild.indexOf("display-sign"),
  "the signature must be applied before it is verified",
);
assert.match(artifacts, /codew-windows-arm64\.exe/);
assert.match(artifacts, /CodeWhaleSetup\.exe/);
assert.match(artifacts, /assemble-release-assets\.js --verify release-assets/);
assert.match(artifacts, /CODEWHALE_SMOKE_ASSETS_DIR/);
assert.match(artifacts, /^  pin:\n/m);
assert.match(artifacts, /Require source_sha equals github\.sha/);
assert.doesNotMatch(
  artifacts,
  /ref: \$\{\{ inputs\.source_sha \}\}/,
  "artifact jobs must checkout GITHUB_SHA, not interpolate the caller SHA into ref",
);
assert.match(artifacts, /prefix-key: v1-\$\{\{ runner\.os \}\}-\$\{\{ runner\.arch \}\}-stable/);
assert.equal(
  (artifacts.match(/package-manager-cache: false/g) || []).length,
  2,
  "assemble and smoke must disable setup-node's implicit npm cache",
);
const bundleStep = namedStep(artifacts, "Create and checksum platform archives");
assert.match(bundleStep, /SOURCE_SHA: \$\{\{ github\.sha \}\}/);
assert.match(bundleStep, /git show -s --format=%ct "\$\{SOURCE_SHA\}"/);
assert.match(
  bundleStep,
  /SOURCE_DATE_EPOCH="\$\{source_date_epoch\}"[\s\\]+bash scripts\/release\/create-release-bundles\.sh artifacts bundles/,
);
assert.doesNotMatch(bundleStep, /inputs\.source_sha/);
assert.doesNotMatch(bundleStep, /\bdate\b/, "bundle timestamps must come from the pinned source commit, not wall-clock time");

const rustCacheBlocks = [...artifacts.matchAll(/uses: Swatinem\/rust-cache@[\s\S]*?(?=\n      - )/g)].map(
  (match) => match[0],
);
assert.ok(rustCacheBlocks.length >= 1, "shared artifact workflow must pin rust-cache");
for (const block of rustCacheBlocks) {
  assert.doesNotMatch(block, /github\.(event|ref|sha)|inputs\./);
}

// One parity gate, called by the release candidate and the public release,
// and the release refuses a tag without a green RC receipt for its exact SHA.
// The release gate must lint at least as strictly as the merge gate.
function clippyAllows(source, label) {
  const command = source.match(/cargo clippy --workspace --all-targets --all-features --locked -- \\\n([\s\S]*?)\n(?! +-)/)?.[1];
  assert.ok(command, `${label} must run workspace all-targets clippy`);
  return [...command.matchAll(/-A (clippy::\w+)/g)].map((match) => match[1]).sort();
}
assert.deepEqual(
  clippyAllows(parityWorkflow, "release-parity.yml"),
  clippyAllows(ci, "ci.yml"),
  "release-parity.yml clippy allowances must match ci.yml",
);
const parity = parityWorkflow.match(/\n  parity:\n([\s\S]*)$/);
assert.ok(parity, "release-parity.yml must define the parity job");
assert.match(parityWorkflow, /^on:\n  workflow_call:\n/m, "parity must be a reusable workflow");
for (const [name, source] of [["release.yml", release], ["release-candidate.yml", candidate]]) {
  const caller = source.match(/\n  parity:\n([\s\S]*?)\n\n/);
  assert.ok(caller, `${name} must run the parity job`);
  assert.match(caller[1], /name: Parity\n/, `${name}: the RC receipt check matches the "Parity" job name`);
  assert.match(caller[1], /uses: \.\/\.github\/workflows\/release-parity\.yml/, `${name} must call the shared parity gate`);
}
assert.match(
  namedStep(release, "Require a green release-candidate receipt for this exact SHA"),
  /require-rc-receipt\.sh "\$\{GITHUB_REPOSITORY\}" "\$\{SHA\}"/,
);
assert.match(release, /^  resolve:\n(?:.*\n)*?      actions: read\n/m, "resolve needs actions: read for the RC receipt");
assert.doesNotMatch(
  parity[1],
  /ref: \$\{\{ needs\.resolve\.outputs\.sha \}\}/,
  "parity must checkout GITHUB_SHA after resolve, not interpolate the tag SHA into ref",
);
assert.match(parity[1], /prefix-key: v1-\$\{\{ runner\.os \}\}-\$\{\{ runner\.arch \}\}-stable/);
const parityRustCache = [...parity[1].matchAll(/uses: Swatinem\/rust-cache@[\s\S]*?(?=\n      - )/g)].map(
  (match) => match[0],
);
assert.equal(parityRustCache.length, 1, "parity must pin exactly one rust-cache");
assert.doesNotMatch(parityRustCache[0], /github\.(event|ref|sha)|inputs\./);

assert.equal(allReleaseAssetNames().length, 38);
assert.match(release, /^  artifacts:\n/m);
assert.match(release, /uses: \.\/\.github\/workflows\/release-artifacts\.yml/);
assert.doesNotMatch(release, /^  (build|bundle|windows-installer):/m);
assert.match(release, /name: codewhale-release-assets\n\s+path: artifacts/);
assert.match(release, /files: artifacts\/\*/);
assert.equal(
  (release.match(/ensure-release-assets-absent\.js/g) || []).length,
  2,
  "public release must refuse existing assets before work and immediately before upload",
);
assert.match(release, /overwrite_files:\s*false/);
assert.match(release, /fail_on_unmatched_files:\s*true/);
// Assets land in a draft; the whole verified set becomes public at once, and
// derived channels (the tagged container manifest) follow the canonical release.
assert.match(release, /files: artifacts\/\*\n\s+draft: true\n/);
assert.match(
  namedStep(release, "Verify the draft's exact asset set, then publish it at once"),
  /verify-release-inventory\.js \\\n\s+--draft --asset-dir artifacts --publish/,
);
const releaseJob = release.match(/\n  release:\n([\s\S]*?)\n  npm:\n/);
assert.ok(releaseJob, "public release must retain its release job");
assert.match(releaseJob[1], /^    needs: \[artifacts, docker-build, resolve\]$/m);
const dockerJob = release.match(/\n  docker:\n([\s\S]*?)\n  release:\n/);
assert.ok(dockerJob, "public release must retain its container manifest job");
assert.match(dockerJob[1], /^    needs: \[docker-build, release, resolve\]$/m);
assert.match(dockerJob[1], /needs\.release\.result == 'success'/);
const cnbJob = release.match(/\n  cnb:\n([\s\S]*?)\n  npm:\n/);
assert.ok(cnbJob, "CNB tag publication must follow the canonical release");
assert.match(cnbJob[1], /^    needs: \[release, resolve\]$/m);
assert.match(cnbJob[1], /needs\.release\.result == 'success'/);
assert.match(cnbJob[1], /uses: \.\/\.github\/workflows\/sync-cnb\.yml/);
assert.doesNotMatch(cnbSync, /^\s+tags:/m, "a tag push alone must not start CNB publication");
assert.match(cnbSync, /^  workflow_call:/m);
const cnbPush = namedStep(cnbSync, "Push triggering ref to CNB");
const cnbPublicGate = cnbPush.indexOf("node scripts/release/verify-release-inventory.js --manifest");
const cnbTagPush = cnbPush.indexOf('scripts/release/mirror-push.sh cnb "tag ${TAG}"');
assert.ok(cnbPublicGate >= 0 && cnbTagPush > cnbPublicGate,
  "manual and called tag mirrors must verify the published inventory before pushing");
assert.match(cnbPush, /verify-remote-tag\.sh/);
assert.doesNotMatch(cnbPush, /\+refs\/tags\//, "published mirror tags must not be rewritten");
assert.match(
  namedStep(republish, "Require a complete published release"),
  /verify-release-inventory\.js --manifest/,
);

assert.match(release, /^  docker-build:\n/m);
assert.match(release, /^  docker:\n/m);
assert.match(release, /runner: ubuntu-latest\n\s+platform: linux\/amd64/);
assert.match(release, /runner: ubuntu-24\.04-arm\n\s+platform: linux\/arm64/);
assert.match(release, /cli_artifact: codewhale-linux-x64/);
assert.match(release, /cli_artifact: codewhale-linux-arm64/);
assert.match(release, /shim_artifact: codew-linux-x64/);
assert.match(release, /shim_artifact: codew-linux-arm64/);
assert.doesNotMatch(
  release,
  /docker\/setup-qemu-action/,
  "public container publication must not funnel both architectures through QEMU",
);
const releaseDockerBytes = namedStep(release, "Verify native release bytes");
assert.match(releaseDockerBytes, /CLI_ARTIFACT: \$\{\{ matrix\.cli_artifact \}\}/);
assert.match(releaseDockerBytes, /SHIM_ARTIFACT: \$\{\{ matrix\.shim_artifact \}\}/);
assert.match(
  releaseDockerBytes,
  /mv -- "docker-context\/bin\/\$\{CLI_ARTIFACT\}" docker-context\/bin\/codewhale/,
);
assert.match(
  releaseDockerBytes,
  /mv -- "docker-context\/bin\/\$\{SHIM_ARTIFACT\}" docker-context\/bin\/codew/,
);
assert.match(releaseDockerBytes, /cmp docker-context\/bin\/codewhale docker-context\/bin\/codew/);
const releaseDockerBuild = namedStep(release, "Assemble and push native image by digest");
assert.match(releaseDockerBuild, /context: docker-context/);
assert.match(releaseDockerBuild, /file: infra\/packaging\/docker\/Dockerfile\.release/);
assert.match(releaseDockerBuild, /platforms: \$\{\{ matrix\.platform \}\}/);
assert.match(releaseDockerBuild, /provenance: mode=max/);
assert.match(releaseDockerBuild, /sbom: true/);
assert.match(releaseDockerBuild, /push-by-digest=true/);
const releaseDockerManifest = namedStep(release, "Publish multi-architecture manifest");
assert.match(releaseDockerManifest, /Expected exactly two native image digests/);
assert.match(releaseDockerManifest, /docker buildx imagetools create/);
const releaseDockerSmoke = namedStep(release, "Verify and smoke published container");
assert.match(releaseDockerSmoke, /linux\/amd64/);
assert.match(releaseDockerSmoke, /linux\/arm64/);
assert.match(releaseDockerSmoke, /--entrypoint codewhale/);
assert.match(releaseDockerSmoke, /--entrypoint codew/);

const npmJob = release.match(/\n  npm:\n([\s\S]*?)\n  homebrew:\n/);
assert.ok(npmJob, "public release must retain a dedicated npm publication job");
assert.match(npmJob[1], /^    needs: \[release, resolve\]$/m);
assert.match(npmJob[1], /needs\.release\.result == 'success'/);
assert.match(npmJob[1], /^      contents: read$/m);
assert.match(npmJob[1], /^      id-token: write$/m);
assert.match(npmJob[1], /ref: \$\{\{ needs\.resolve\.outputs\.sha \}\}/);
assert.match(npmJob[1], /fetch-depth: 0/);
assert.match(npmJob[1], /node-version: 24/);
assert.match(npmJob[1], /registry-url: https:\/\/registry\.npmjs\.org/);
assert.match(npmJob[1], /package-manager-cache: false/);
assert.match(npmJob[1], /npm install --global npm@12\.0\.2/);
const npmTagGate = namedStep(release, "Revalidate release tag before npm publish");
const npmAssetGate = namedStep(release, "Revalidate public release assets");
const npmPublish = namedStep(release, "Publish npm wrapper with trusted publishing");
assert.match(npmTagGate, /verify-remote-tag\.sh/);
assert.match(npmAssetGate, /verify-release-assets\.sh/);
assert.match(npmAssetGate, /GH_TOKEN: \$\{\{ github\.token \}\}/);
assert.match(npmPublish, /working-directory: npm\/codewhale/);
assert.match(npmPublish, /GH_TOKEN: \$\{\{ github\.token \}\}/);
assert.match(npmPublish, /npm publish --access public/);
assert.doesNotMatch(npmJob[1], /NPM_TOKEN|NODE_AUTH_TOKEN|secrets\./);
assert.ok(
  release.indexOf("Revalidate public release assets") <
    release.indexOf("Publish npm wrapper with trusted publishing"),
  "npm publication must follow the public exact-asset gate",
);

assert.match(releaseDockerfile, /^FROM debian:bookworm-slim$/m);
assert.match(releaseDockerfile, /ca-certificates/);
assert.match(releaseDockerfile, /libdbus-1-3/);
assert.match(releaseDockerfile, /COPY .*bin\/codewhale \/usr\/local\/bin\/codewhale/);
assert.match(releaseDockerfile, /COPY .*bin\/codew \/usr\/local\/bin\/codew/);
assert.match(releaseDockerfile, /^USER codewhale$/m);
assert.doesNotMatch(
  releaseDockerfile,
  /\bcargo\s+build\b|^FROM\s+rust:/m,
  "release container assembly must reuse the already-verified release binaries",
);

assert.match(runbook, /release[- ]candidate/i);
assert.match(runbook, /expected_sha/);
assert.match(runbook, /38/);
assert.match(runbook, /does not create a tag/i);
assert.match(runbook, /explicit.*approval/i);
assert.match(runbook, /last[- ]useful[- ]log/i, "runbook must document the last-useful-log rule (#5496)");
assert.match(runbook, /404 logs/i, "runbook must document the 404-log cancellation rule (#5496)");

const cnbRustGates = cnb.match(
  /\.rust_workspace_gates_stage: &rust_workspace_gates_stage([\s\S]*?)\n\.linux_rust_gates:/,
);
assert.ok(cnbRustGates, "CNB must retain the shared Rust workspace gate");
assert.match(
  cnbRustGates[1],
  /timeout: 45m[\s\S]*export CARGO_BUILD_JOBS=1[\s\S]*export CARGO_PROFILE_TEST_DEBUG=0[\s\S]*cargo check --workspace --all-targets --locked[\s\S]*cargo clippy --workspace --all-targets --all-features --locked -- -D warnings[\s\S]*RUST_MIN_STACK=16777216 sh scripts\/with-hermetic-test-home.sh cargo test --workspace --all-features --locked/,
  "CNB must serialize the memory-heavy Rust gate and preserve the workspace test stack contract",
);
assert.doesNotMatch(
  cnbRustGates[1],
  /export (?:HOME|USERPROFILE|CODEWHALE_HOME)=/,
  "CNB must reuse the shared test-home boundary without overriding legacy migration fixtures",
);

// Cover every test invocation, including named parity and narrow crate gates.
// These launchers protect production dependencies as well as cfg(test) code.
// `release parity` is 4 rather than 3: parity runs the workspace under nextest
// for the same one-process-per-test isolation CI's lanes use, and keeps a
// separate doctest invocation because nextest does not run doctests. release.yml
// itself runs none: its parity job calls release-parity.yml.
let hermeticInvocations = 0;
for (const [label, workflow, expected] of [
  ["CI", ci, 6],
  ["release", release, 0],
  ["release parity", parityWorkflow, 4],
  ["CNB", cnb, 3],
]) {
  const commands = workflow.split("\n").filter((line) =>
    !line.trimStart().startsWith("#") &&
    // Exclude only this standalone quoted receipt argument. A printf line
    // with command substitution or any appended execution still counts.
    line.trim() !== "'command=sh scripts/with-hermetic-test-home.sh cargo test --workspace --all-features --locked -- --format=pretty'" &&
    /\bcargo (?:test|nextest run)\b/.test(line),
  );
  assert.equal(commands.length, expected, `${label} must retain every Rust test invocation`);
  for (const command of commands) {
    assert.match(command, /sh scripts\/with-hermetic-test-home.sh cargo (?:test|nextest run)\b/,
      `${label} Rust tests must use the shared test-home boundary`);
  }
  hermeticInvocations += commands.length;
}
for (const name of ["Run tests", "Run doctests"]) {
  const step = namedStep(ciTestJob, name);
  assert.match(step, /shell: bash/, `${name} must invoke the POSIX helper on Windows too`);
  assert.match(step, /RUST_MIN_STACK: '16777216'/);
}
console.log(`Hermetic Rust workflow invocations OK: ${hermeticInvocations} checks passed.`);

const nextest = read(".config/nextest.toml");
const integrationGroup = nextest.search(/^filter = 'binary\(integration\)'$/m);
const telemetryGroup = nextest.indexOf(
  "filter = 'binary(integration) & test(/^telemetry_contract::/)'",
);
const execGroup = nextest.indexOf(
  "filter = 'binary(integration) & test(/^exec_persistent_service::/)'",
);
assert.ok(integrationGroup >= 0, "nextest must bound the integration binary");
assert.ok(
  telemetryGroup >= 0 && telemetryGroup < integrationGroup,
  "telemetry-contract override must precede binary(integration); first matching group wins",
);
assert.ok(
  execGroup >= 0 && execGroup < integrationGroup,
  "exec_persistent_service override must precede binary(integration); first matching group wins",
);
assert.match(nextest, /exec-persistent-service = \{ max-threads = 1 \}/);
assert.equal(
  (cnb.match(/^\s+- \*rust_workspace_gates_stage$/gm) || []).length,
  2,
  "both CNB Rust pipelines must reuse the constrained workspace gate",
);

const cnbPreflight = cnb.match(
  /\.linux_release_preflight: &linux_release_preflight([\s\S]*?)\nmain:/,
);
assert.ok(cnbPreflight, "CNB must retain a dedicated release preflight");
const cnbBuild = cnbPreflight[1].indexOf(
  "cargo build --jobs 2 --release --locked -p codewhale-cli",
);
const cnbAlias = cnbPreflight[1].indexOf(
  "cp target/release/codewhale target/release/codew",
);
const cnbSmoke = cnbPreflight[1].indexOf("node scripts/release/npm-wrapper-smoke.js");
assert.ok(cnbBuild >= 0, "CNB release preflight must build the consolidated runtime");
assert.ok(cnbAlias > cnbBuild, "CNB release preflight must materialize codew after the build");
assert.ok(cnbSmoke > cnbAlias, "CNB release preflight must materialize codew before smoke");

const cnbTagRelease = cnb.match(/\$:\n  tag_push:\n([\s\S]*)$/);
assert.ok(cnbTagRelease, "CNB must retain a tag release pipeline");
const cnbTagStamp = cnbTagRelease[1].indexOf(
  'export CODEWHALE_BUILD_SHA="$commit_sha"',
);
const cnbTagBuild = cnbTagRelease[1].indexOf(
  "cargo build --jobs 2 --release --locked \\",
);
const cnbTagVersionCheck = cnbTagRelease[1].indexOf(
  "./scripts/release/check-versions.sh --require-dated-release",
);
assert.ok(cnbTagVersionCheck >= 0, "CNB publication must reject undated source candidates");
for (const [label, workflow] of [["release-candidate.yml", candidate], ["release.yml", release]]) {
  assert.match(
    workflow,
    /\.\/scripts\/release\/check-versions\.sh --require-dated-release/,
    `${label} must reject undated source candidates before building`,
  );
}
assert.ok(cnbTagVersionCheck < cnbTagBuild, "CNB must validate release notes before building public assets");
assert.match(cnbTagRelease[1], /checkout_sha="\$\(git rev-parse 'HEAD\^\{commit\}'\)"/);
assert.match(cnbTagRelease[1], /commit_sha="\$\{CNB_COMMIT:-\$\{checkout_sha\}\}"/);
assert.match(cnbTagRelease[1], /CNB_COMMIT[\s\S]*does not match checkout[\s\S]*exit 1/);
assert.ok(cnbTagStamp >= 0, "CNB tag releases must stamp the consolidated runtime");
assert.ok(cnbTagBuild > cnbTagStamp, "CNB tag releases must stamp before compiling");

assert.doesNotMatch(
  archiveInstaller,
  /cargo install codewhale --locked/,
  "glibc recovery must name the published codewhale-cli crate",
);
assert.equal(
  (archiveInstaller.match(/cargo install codewhale-cli --locked/g) || []).length,
  2,
  "both glibc recovery branches must name codewhale-cli",
);
assert.doesNotMatch(
  archiveInstaller,
  /install_binary "\$SCRIPT_DIR\/[^"]+" "\$BIN_DIR\/codewhale-tui"/,
  "archive installs must not write the retired codewhale-tui command",
);
assert.doesNotMatch(
  cliDispatcher,
  /codewhale_config::auto_model::classify/,
  "the CLI dispatcher must leave auto routing to the provider-aware runtime",
);

// #5496: every release-lane job carries an explicit `timeout-minutes`.
//
// GitHub's default is 360 minutes, so an assigned-but-dead runner sits for six
// hours before anything reclaims it — observed on the v0.9.9 train as a job
// stuck `in_progress` with 404 logs. Timeouts are containment, not recovery:
// the runbook keeps the 404-log cancel/rerun rule for infrastructure failures.
//
// A job that calls a reusable workflow (`uses:`) cannot carry the key at all —
// GitHub rejects it — so the callee owns its own caps. That is why the artifact
// bounds live in release-artifacts.yml rather than in its callers.
function jobsWithoutTimeout(source) {
  const lines = source.split("\n");
  const jobsAt = lines.findIndex((line) => /^jobs:\s*$/.test(line));
  assert.notEqual(jobsAt, -1, "workflow must declare jobs");
  const offenders = [];
  for (let i = jobsAt + 1; i < lines.length; i += 1) {
    const header = lines[i].match(/^  ([A-Za-z0-9_-]+):\s*$/);
    if (!header) continue;
    let reusable = false;
    let capped = false;
    for (let j = i + 1; j < lines.length; j += 1) {
      if (/^  [A-Za-z0-9_-]+:\s*$/.test(lines[j])) break;
      if (/^    uses:/.test(lines[j])) reusable = true;
      if (/^    timeout-minutes:\s*\d+\s*$/.test(lines[j])) capped = true;
    }
    if (!reusable && !capped) offenders.push(header[1]);
  }
  return offenders;
}

assert.deepEqual(
  jobsWithoutTimeout("jobs:\n  uncapped:\n    runs-on: ubuntu-latest\n"),
  ["uncapped"],
  "jobsWithoutTimeout must detect an uncapped job",
);
assert.deepEqual(
  jobsWithoutTimeout("jobs:\n  reusable:\n    uses: ./.github/workflows/reusable.yml\n"),
  [],
  "jobsWithoutTimeout must skip reusable workflow callers",
);
assert.deepEqual(
  jobsWithoutTimeout("jobs:\n  capped:\n    runs-on: ubuntu-latest\n    timeout-minutes: 15\n"),
  [],
  "jobsWithoutTimeout must accept a capped job",
);

for (const [name, source] of [
  ["release-candidate.yml", candidate],
  ["release-artifacts.yml", artifacts],
  ["release.yml", release],
  ["release-parity.yml", parityWorkflow],
  ["release-republish.yml", republish],
  ["ci.yml", ci],
  ["nightly.yml", nightly],
]) {
  assert.deepEqual(
    jobsWithoutTimeout(source),
    [],
    `${name}: every job must set timeout-minutes (#5496)`,
  );
}

// The Windows artifact build historically runs 40-45 minutes, so its cap has to
// keep real margin — a tight bound here fails healthy releases.
const buildTimeout = artifacts.match(/^  build:\n(?:.*\n)*?    timeout-minutes: (\d+)$/m);
assert.ok(buildTimeout, "release-artifacts build job must be capped");
assert.ok(
  Number(buildTimeout[1]) >= 60,
  `artifact build cap ${buildTimeout[1]}m leaves no margin over a healthy 40-45m Windows build`,
);

function jobTimeout(source, job) {
  const match = source.match(
    new RegExp(`^  ${job}:\\n(?:.*\\n)*?    timeout-minutes: (\\d+)$`, "m"),
  );
  assert.ok(match, `${job} must declare timeout-minutes`);
  return Number(match[1]);
}

// Fast setup and packaging fail quickly. Native builds need the same finite
// cold-build allowance as full CI: both 0.10.1 macOS artifacts hit the old
// 90m compilation cap before reaching their launch and inventory checks.
assert.equal(jobTimeout(candidate, "resolve"), 10);
assert.equal(jobTimeout(candidate, "web"), 15);
assert.equal(jobTimeout(artifacts, "pin"), 10);
assert.ok(
  jobTimeout(artifacts, "build") >= jobTimeout(ci, "test"),
  "native artifacts must allow the full CI cold-build budget",
);
for (const job of ["bundle", "windows-installer", "assemble", "smoke"]) {
  assert.equal(jobTimeout(artifacts, job), 15, `${job} must keep the 15m packaging cap`);
}
assert.equal(
  jobTimeout(nightly, "build"), jobTimeout(artifacts, "build"),
  "nightly and release must share the native cold-build allowance",
);
assert.equal(jobTimeout(release, "resolve"), 10);
// The 0.10.1 cold parity build exhausted 45 minutes before tests started.
// Reuse the full CI suite's budget rather than pinning an older release's cap.
assert.ok(
  jobTimeout(parityWorkflow, "parity") >= jobTimeout(ci, "test"),
  "release parity must allow the full CI suite's cold-build budget",
);

console.log(
  "Workflow contracts OK: 6-target/12-asset single-runtime nightly and exact-head 8-target/38-asset release candidate.",
);
