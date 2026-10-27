#!/usr/bin/env node

const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");

const { allReleaseAssetNames, CHECKSUM_MANIFEST } = require("../../npm/codewhale/scripts/artifacts");
const { run } = require("./verify-release-inventory");

const REPO = "codewhale-hq/CodeWhale";
const TAG = "v0.10.1";

function localAssets(t) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "cw-release-inventory-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  for (const [index, name] of allReleaseAssetNames().entries()) fs.writeFileSync(path.join(dir, name), "x".repeat(index + 1));
  return dir;
}

function uploaded(names, sizeOf = (index) => index + 1) {
  return names.map((name, index) => ({ name, state: "uploaded", size: sizeOf(index),
    digest: `sha256:${crypto.hash("sha256", "x".repeat(sizeOf(index)))}` }));
}

/** A gh double over one repository's releases; records every call. */
function fakeGh(releases, { manifest = "", pages = [releases] } = {}) {
  const calls = [];
  const gh = (args) => {
    calls.push(args.join(" "));
    if (args[0] === "release" && args[1] === "download") return manifest;
    const [, method, endpoint] = args[1] === "-X" ? [null, args[2], args[3]] : [null, "GET", args[1]];
    if (method === "PATCH") {
      const id = Number(endpoint.split("/").pop());
      for (const release of releases) if (release.id === id) release.draft = false;
      return "{}";
    }
    if (endpoint.startsWith(`repos/${REPO}/releases/tags/`)) {
      const found = releases.find((release) => release.tag_name === TAG && !release.draft);
      if (!found) throw new Error("gh api failed: HTTP 404");
      return JSON.stringify(found);
    }
    if (endpoint.startsWith(`repos/${REPO}/releases?`)) {
      assert.ok(args.includes("--paginate") && args.includes("--slurp"));
      return JSON.stringify(pages);
    }
    throw new Error(`unexpected gh call: ${args.join(" ")}`);
  };
  return { gh, calls };
}

test("a complete draft is published at once, only after its inventory verifies", (t) => {
  const dir = localAssets(t);
  const releases = [{ id: 7, tag_name: TAG, draft: true, assets: uploaded(allReleaseAssetNames()) }];
  const { gh, calls } = fakeGh(releases);
  run(["--draft", "--asset-dir", dir, "--publish", REPO, TAG], gh, () => {});
  assert.equal(releases[0].draft, false);
  const patch = calls.findIndex((call) => call.startsWith("api -X PATCH"));
  assert.ok(patch > 0, "publishing follows the draft lookup");
  assert.equal(calls[patch], `api -X PATCH repos/${REPO}/releases/7 -F draft=false`);
});

test("a partial, unfinished or resized draft is never published", (t) => {
  const dir = localAssets(t);
  const names = allReleaseAssetNames();
  const cases = [
    ["missing", uploaded(names.slice(0, -1)), /missing: /],
    ["starter", uploaded(names).map((asset, index) => (index === 3 ? { ...asset, state: "starter" } : asset)), /not fully uploaded/],
    ["size", uploaded(names, () => 1), /bytes; the verified local asset has/],
    ["unexpected", [...uploaded(names), { name: "stray.txt", state: "uploaded", size: 1 }], /unexpected: stray\.txt/],
  ];
  for (const [label, assets, message] of cases) {
    const releases = [{ id: 7, tag_name: TAG, draft: true, assets }];
    const { gh, calls } = fakeGh(releases);
    assert.throws(() => run(["--draft", "--asset-dir", dir, "--publish", REPO, TAG], gh, () => {}), message, label);
    assert.equal(releases[0].draft, true, label);
    assert.equal(calls.some((call) => call.includes("PATCH")), false, label);
  }
});

test("zero or several drafts for the tag need a maintainer, not a guess", () => {
  for (const drafts of [[], [1, 2]]) {
    const releases = drafts.map((id) => ({ id, tag_name: TAG, draft: true, assets: uploaded(allReleaseAssetNames()) }));
    const { gh } = fakeGh(releases);
    assert.throws(() => run(["--draft", REPO, TAG], gh, () => {}), /Expected exactly one draft release/);
  }
});

test("republish derives only from a release carrying exactly its own checksum manifest", () => {
  const names = ["codewhale-linux-x64", "codewhale-macos-arm64"];
  const manifest = names.map((name, index) => `${crypto.hash("sha256", "x".repeat(index + 1))}  ${name}`).join("\n");
  const complete = [{ id: 3, tag_name: TAG, draft: false, assets: uploaded([...names, CHECKSUM_MANIFEST]) }];
  assert.doesNotThrow(() => run(["--manifest", REPO, TAG], fakeGh(complete, { manifest }).gh, () => {}));
  const partial = [{ id: 3, tag_name: TAG, draft: false, assets: uploaded([names[0], CHECKSUM_MANIFEST]) }];
  assert.throws(() => run(["--manifest", REPO, TAG], fakeGh(partial, { manifest }).gh, () => {}), /missing: codewhale-macos-arm64/);
});

test("usage refuses publish without draft and manifest with a local directory", () => {
  assert.throws(() => run(["--publish", REPO, TAG], () => "[]", () => {}), /Usage/);
  assert.throws(() => run(["--draft", "--publish", REPO, TAG], () => "[]", () => {}), /Usage/);
  assert.throws(() => run(["--manifest", "--asset-dir", "x", REPO, TAG], () => "[]", () => {}), /Usage/);
});

test("same-size stale bytes and missing digests cannot publish a draft", (t) => {
  const dir = localAssets(t);
  for (const digest of [`sha256:${crypto.hash("sha256", "y")}`, null]) {
    const assets = uploaded(allReleaseAssetNames());
    assets[0].digest = digest;
    const releases = [{ id: 7, tag_name: TAG, draft: true, assets }];
    const { gh, calls } = fakeGh(releases);
    assert.throws(() => run(["--draft", "--asset-dir", dir, "--publish", REPO, TAG], gh, () => {}), /SHA-256 digest/);
    assert.equal(releases[0].draft, true);
    assert.equal(calls.some((call) => call.includes("PATCH")), false);
  }
});

test("drafts beyond the first release page are verified and duplicates refused", (t) => {
  const dir = localAssets(t);
  const draft = { id: 7, tag_name: TAG, draft: true, assets: uploaded(allReleaseAssetNames()) };
  const older = Array.from({ length: 100 }, (_, id) => ({ id: 100 + id, tag_name: `v0.9.${id}`, draft: false }));
  const { gh } = fakeGh([draft], { pages: [older, [draft]] });
  run(["--draft", "--asset-dir", dir, "--publish", REPO, TAG], gh, () => {});
  assert.equal(draft.draft, false);
  draft.draft = true;
  const duplicate = { ...draft, id: 8 };
  const second = fakeGh([draft, duplicate], { pages: [[draft], [duplicate]] });
  assert.throws(() => run(["--draft", "--asset-dir", dir, "--publish", REPO, TAG], second.gh, () => {}), /found 2/);
  assert.equal(second.calls.some((call) => call.includes("PATCH")), false);
});
