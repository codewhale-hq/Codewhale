import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createHash } from "node:crypto";
import { writeAgentDocs } from "./docs-sources-lib.mjs";

test("generates a missing snapshot and refuses invalid sources without replacing it", () => {
  const root = mkdtempSync(join(tmpdir(), "agent-docs-"));
  try {
    const web = join(root, "web");
    mkdirSync(join(web, "lib"), { recursive: true });
    mkdirSync(join(web, "data"));
    const registry = join(web, "lib", "docs-map.ts");
    const snapshot = join(web, "data", "agent-docs.generated.json");
    const content = "# Current route inventory\n";
    writeFileSync(join(root, "routes.md"), content);
    const topic = (source) => `{ id: "runtime", slug: "runtime", repoSource: ["${source}"] }`;
    writeFileSync(registry, topic("routes.md"));
    writeAgentDocs(web, root);
    const generated = readFileSync(snapshot, "utf8");
    assert.deepEqual(JSON.parse(generated), [{ id: "runtime", slug: "runtime", sources: [{
      path: "routes.md", content, sha256: createHash("sha256").update(content).digest("hex"),
    }] }]);
    for (const source of ["missing.md", "../outside.md"]) {
      writeFileSync(registry, topic(source));
      assert.throws(() => writeAgentDocs(web, root));
      assert.equal(readFileSync(snapshot, "utf8"), generated);
    }
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
