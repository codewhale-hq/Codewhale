import { test } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { checkReleaseDocs, RELEASE_DOCS_FORMAT, releaseDocsIndex } from "./release-docs-lib.mjs";
import { BASELINE_FORMAT, checkReleaseBaseline } from "./release-baseline-lib.mjs";

const release = { tag: "v0.10.3", version: "0.10.3" };

function sampleSnapshot(content = "# Stale doc\n") {
  return {
    format: RELEASE_DOCS_FORMAT,
    tag: release.tag,
    version: release.version,
    topics: [
      {
        id: "runtime",
        slug: "runtime",
        sources: [
          {
            path: "docs/RUNTIME.md",
            sha256: createHash("sha256").update(content).digest("hex"),
            content,
          },
        ],
        missing: [],
      },
    ],
    omitted: [],
  };
}

function sampleBaseline(toolCount = 70) {
  return {
    format: BASELINE_FORMAT,
    tag: release.tag,
    version: release.version,
    features: [],
    providers: [],
    models: [],
    toolCount,
  };
}

test("checkReleaseDocs fails closed when derived is null even for a self-consistent snapshot", () => {
  const snapshot = sampleSnapshot();
  const registry = [{ slug: "runtime" }];
  const index = releaseDocsIndex(snapshot);

  const problems = checkReleaseDocs(snapshot, release, registry, null, index);
  assert.equal(problems.length, 1);
  assert.match(problems[0], /could not be verified/);

  assert.deepEqual(checkReleaseDocs(snapshot, release, registry, snapshot, index), []);
  assert.deepEqual(checkReleaseDocs(snapshot, release, registry, null, index, { allowShallow: true }), []);
  assert.equal(
    checkReleaseDocs(snapshot, release, registry, sampleSnapshot("# Tagged doc\n"), index).length,
    1,
  );
});

test("checkReleaseBaseline fails closed when derived baseline is null even for a tag-matching baseline", () => {
  const committed = sampleBaseline(70);

  const problems = checkReleaseBaseline(committed, release, null);
  assert.equal(problems.length, 1);
  assert.match(problems[0], /could not be verified/);

  assert.deepEqual(checkReleaseBaseline(committed, release, sampleBaseline(70)), []);
  assert.deepEqual(checkReleaseBaseline(committed, release, null, { allowShallow: true }), []);
  assert.equal(checkReleaseBaseline(committed, release, sampleBaseline(78)).length, 1);
});
