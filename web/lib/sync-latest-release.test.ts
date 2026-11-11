import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it } from "vitest";

const script = fileURLToPath(new URL("../scripts/sync-latest-release.mjs", import.meta.url));
const roots: string[] = [];
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }); });

const OLD = { tag: "v0.10.0", version: "0.10.0", publishedAt: "2026-09-01T00:00:00Z", url: "https://github.com/codewhale-hq/CodeWhale/releases/tag/v0.10.0" };

/** The script in a throwaway web/ + docs/ tree, with GitHub stubbed. */
function run(args: string[], { status = 200, cloud = { release: { latest: "0.10.0" } } }: { status?: number; cloud?: unknown } = {}) {
  const root = mkdtempSync(join(tmpdir(), "cw-sync-release-"));
  roots.push(root);
  mkdirSync(join(root, "web", "scripts"), { recursive: true });
  mkdirSync(join(root, "web", "data"), { recursive: true });
  mkdirSync(join(root, "docs", "cloud-facts"), { recursive: true });
  copyFileSync(script, join(root, "web", "scripts", "sync-latest-release.mjs"));
  writeFileSync(join(root, "web", "data", "latest-published-release.json"), `${JSON.stringify(OLD, null, 2)}\n`);
  writeFileSync(join(root, "docs", "public-surface-facts.json"), `${JSON.stringify({ latestPublishedRelease: { ...OLD, sources: ["x"] } }, null, 2)}\n`);
  writeFileSync(join(root, "docs", "cloud-facts", "stable.json"), `${JSON.stringify(cloud, null, 2)}\n`);
  const release = JSON.stringify({
    tag_name: "v0.10.1",
    published_at: "2026-09-02T00:00:00Z",
    assets: [
      { name: "b-asset", size: 20, state: "uploaded", digest: `sha256:${"b".repeat(64)}` },
      { name: "a-asset", size: 10, state: "uploaded" },
      { name: "partial", size: 5, state: "open", digest: `sha256:${"c".repeat(64)}` },
    ],
  });
  const stub = `globalThis.fetch = async () => new Response(${JSON.stringify(release)}, { status: ${status} });`;
  const result = spawnSync(process.execPath, ["--import", `data:text/javascript,${encodeURIComponent(stub)}`, join(root, "web", "scripts", "sync-latest-release.mjs"), ...args], { encoding: "utf8", env: { ...process.env, GITHUB_TOKEN: "" } });
  const read = (...parts: string[]) => JSON.parse(readFileSync(join(root, ...parts), "utf8"));
  return { result, root, target: () => read("web", "data", "latest-published-release.json"), mirror: () => read("docs", "public-surface-facts.json") };
}

describe("sync-latest-release", () => {
  it("fails --check when GitHub cannot be read instead of reporting current", () => {
    const { result } = run(["--check"], { status: 503 });
    expect(result.status).toBe(1);
    expect(result.stderr).toContain("could not read the latest release");
  });

  it("writes nothing when any mirror is unusable, then moves all three together", () => {
    const broken = run([], { cloud: { notRelease: true } });
    expect(broken.result.status).toBe(1);
    expect(broken.target()).toEqual(OLD);
    expect(broken.mirror().latestPublishedRelease.tag).toBe("v0.10.0");
    expect(readdirSync(join(broken.root, "web", "data"))).toEqual(["latest-published-release.json"]);

    const ok = run([]);
    expect(ok.result.status, ok.result.stderr).toBe(0);
    expect(ok.target()).toMatchObject({
      tag: "v0.10.1",
      assets: [{ name: "a-asset", size: 10, sha256: null }, { name: "b-asset", size: 20, sha256: "b".repeat(64) }],
    });
    expect(Date.parse(ok.target().readAt)).toBeGreaterThan(0);
    expect(ok.mirror().latestPublishedRelease).not.toHaveProperty("assets");
    expect(ok.mirror().latestPublishedRelease).toMatchObject({ tag: "v0.10.1", sources: ["x"] });
    expect(JSON.parse(readFileSync(join(ok.root, "docs", "cloud-facts", "stable.json"), "utf8")).release).toMatchObject({ latest: "0.10.1" });
  });
});
