import { spawnSync } from "node:child_process";
import { convertToWranglerConfig, loadAndParseConfig } from "@cloudflare/config";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

// fileURLToPath, not `.pathname`: the latter stays percent-encoded, so a
// checkout under a path with non-ASCII characters spawns a filename that
// does not exist and every case here fails on a module-not-found exit.
const script = fileURLToPath(new URL("../scripts/check-cloudflare-deploy-env.mjs", import.meta.url));

function run(overrides: Record<string, string>, args: string[] = []) {
  return spawnSync(process.execPath, [script, ...args], {
    encoding: "utf8",
    env: {
      ...process.env,
      GITHUB_ACTIONS: "",
      GITHUB_EVENT_NAME: "",
      GITHUB_REF: "",
      GITHUB_SHA: "",
      CLOUDFLARE_ACCOUNT_ID: "",
      CLOUDFLARE_API_TOKEN: "",
      ...overrides,
    },
  });
}

describe("Cloudflare deploy preflight", () => {
  it("reports intentionally withheld credentials without deploying", () => {
    const result = run({}, ["--preflight"]);

    expect(result.status).toBe(0);
    expect(result.stdout).toContain("credentialState\":\"withheld");
    expect(result.stdout).toContain("deploymentStarted\":false");
  });

  it("rejects malformed supplied values even in credential-free preflight mode", () => {
    const result = run(
      {
        CLOUDFLARE_ACCOUNT_ID: "not-an-account-id",
        CLOUDFLARE_API_TOKEN: "not-a-token",
      },
      ["--preflight"],
    );

    expect(result.status).toBe(1);
    expect(result.stderr).toContain("malformed credential placeholders");
    expect(result.stdout).toContain("credentialState\":\"invalid");
  });

  it("keeps the normal deploy check fail-closed when credentials are missing", () => {
    const result = run({});

    expect(result.status).toBe(1);
    expect(result.stderr).toContain("Cloudflare deploy configuration is incomplete");
  });

  it("requires an exact manual-main context inside GitHub Actions", () => {
    const result = run({
      GITHUB_ACTIONS: "true",
      GITHUB_EVENT_NAME: "push",
      GITHUB_REF: "refs/heads/main",
      GITHUB_SHA: "a".repeat(40),
      CLOUDFLARE_ACCOUNT_ID: "a".repeat(32),
      CLOUDFLARE_API_TOKEN: "token-" + "b".repeat(32),
    });

    expect(result.status).toBe(1);
    expect(result.stderr).toContain("workflow_dispatch on refs/heads/main");
  });

  it("rejects a dispatch on a non-main ref", () => {
    const result = run({
      GITHUB_ACTIONS: "true",
      GITHUB_EVENT_NAME: "workflow_dispatch",
      GITHUB_REF: "refs/heads/release",
      GITHUB_SHA: "a".repeat(40),
      CLOUDFLARE_ACCOUNT_ID: "a".repeat(32),
      CLOUDFLARE_API_TOKEN: "token-" + "b".repeat(32),
    });

    expect(result.status).toBe(1);
    expect(result.stderr).toContain("workflow_dispatch on refs/heads/main");
  });

  it("rejects a dispatch without an exact 40-hex revision", () => {
    const result = run({
      GITHUB_ACTIONS: "true",
      GITHUB_EVENT_NAME: "workflow_dispatch",
      GITHUB_REF: "refs/heads/main",
      GITHUB_SHA: "main",
      CLOUDFLARE_ACCOUNT_ID: "a".repeat(32),
      CLOUDFLARE_API_TOKEN: "token-" + "b".repeat(32),
    });

    expect(result.status).toBe(1);
    expect(result.stderr).toContain("exact SHA");
  });

  it("accepts a manual dispatch on main at an exact SHA", () => {
    const result = run({
      GITHUB_ACTIONS: "true",
      GITHUB_EVENT_NAME: "workflow_dispatch",
      GITHUB_REF: "refs/heads/main",
      GITHUB_SHA: "a".repeat(40),
      CLOUDFLARE_ACCOUNT_ID: "a".repeat(32),
      CLOUDFLARE_API_TOKEN: "token-" + "b".repeat(32),
    });

    expect(result.status).toBe(0);
    expect(result.stdout).toContain("Cloudflare deploy environment is present");
  });
});

// Minimal, dependency-free reader for the two-space-indented job blocks in
// .github/workflows/web.yml. A real YAML parser is not a web dependency, and
// this file only needs the `on:` triggers plus the deploy job's `if:` guard.
function readWebWorkflow() {
  const path = new URL("../../.github/workflows/web.yml", import.meta.url);
  return readFileSync(path, "utf8");
}

function jobBlock(source: string, job: string) {
  const lines = source.split("\n");
  const start = lines.findIndex((line) => line === `  ${job}:`);
  expect(start, `job ${job} not found in web.yml`).toBeGreaterThanOrEqual(0);
  const rest = lines.slice(start + 1);
  const end = rest.findIndex((line) => /^ {2}\S/.test(line));
  return (end === -1 ? rest : rest.slice(0, end)).join("\n");
}

describe("web workflow deploy trigger contract", () => {
  const workflow = readWebWorkflow();
  const deploy = jobBlock(workflow, "deploy");
  const deployReminder = jobBlock(workflow, "deploy-reminder");

  it("runs lint only for manually requested final validation", () => {
    const triggerBlock = workflow.slice(
      workflow.indexOf("on:\n"),
      workflow.indexOf("\n\npermissions:"),
    );
    expect(triggerBlock).toMatch(/^on:\n  workflow_dispatch:(?:\n    .*)*$/);
    expect(jobBlock(workflow, "lint")).not.toContain("if:");
  });

  it("gates deploy on a manual dispatch of main only", () => {
    const guard = deploy
      .slice(deploy.indexOf("if:"))
      .split("\n")
      .slice(0, 3)
      .join(" ")
      .replace(/\s+/g, " ");

    expect(guard).toContain("github.event_name == 'workflow_dispatch'");
    expect(guard).toContain("github.ref == 'refs/heads/main'");
    // The preflight script fails closed on any non-dispatch event, so a push
    // trigger here could only ever produce a red deploy job (#4907).
    expect(guard).not.toContain("'push'");
    expect(deploy).toContain("needs: lint");
  });

  it("surfaces an actionable deployment reminder after a green main push", () => {
    expect(deployReminder).toContain("needs: lint");
    expect(deployReminder).toContain(
      "github.event_name == 'push' && github.ref == 'refs/heads/main'",
    );
    expect(deployReminder).toContain("::notice title=Web deployment approval needed::");
    expect(deployReminder).toContain("gh workflow run web.yml");
    expect(deployReminder).not.toContain("npm run deploy");
  });

  it("checks out the exact dispatched revision before deploying", () => {
    expect(deploy).toContain("ref: ${{ github.sha }}");
    expect(deploy).toContain('--expected-revision "$GITHUB_SHA"');
  });

  it("verifies an optional expected_sha input right after checkout", () => {
    expect(workflow).toMatch(
      /^  workflow_dispatch:\n    inputs:\n      expected_sha:\n        description: .*\n        required: false\n        default: ''\n        type: string$/m,
    );
    for (const job of [jobBlock(workflow, "lint"), deploy]) {
      expect(job).toContain("EXPECTED_SHA: ${{ inputs.expected_sha }}");
      expect(job).toContain('actual="$(git rev-parse HEAD)"');
      expect(job.indexOf("Verify expected commit")).toBeGreaterThan(job.indexOf("actions/checkout@"));
      expect(job.indexOf("Verify expected commit")).toBeLessThan(job.indexOf("actions/setup-node@"));
    }
  });

  it("builds and caches one bundle before cf deploy consumes it", () => {
    const { scripts } = JSON.parse(
      readFileSync(new URL("../package.json", import.meta.url), "utf8"),
    ) as { scripts: Record<string, string> };
    const bundler = readFileSync(new URL("../wrangler.config.ts", import.meta.url), "utf8");
    expect(scripts["build:cloudflare"].split("npm run build:opennext")).toHaveLength(2);
    expect(scripts.deploy.indexOf("npm run build:cloudflare")).toBeLessThan(
      scripts.deploy.indexOf("populateCache remote"),
    );
    expect(scripts.deploy).toMatch(/populateCache remote .* && cf deploy --prebuilt$/);
    expect(scripts.preview).toMatch(/^npm run build:cloudflare && .*populateCache local/);
    expect(bundler).not.toMatch(/build\s*:/);
    expect(deploy).toContain("run: npm run deploy");
    expect(deploy).not.toContain("npm run build");
  });

  it("preserves the website's existing storage and Worker identities in cf config", async () => {
    const { result } = await loadAndParseConfig(
      fileURLToPath(new URL("../cloudflare.config.ts", import.meta.url)),
      { isPreview: false, mode: undefined },
    );
    if (!result.success) throw new Error(String(result.error));
    const config = convertToWranglerConfig(result.data);
    expect(config.name).toBe("codewhale-web");
    expect(config.kv_namespaces).toEqual([
      { binding: "CURATED_KV", id: "abaa6a753c9d45bfa5c0afaf26dc67b3" },
      { binding: "NEXT_INC_CACHE_KV", id: "a2e6f324db9b4b03bbc940a4ba246985" },
    ]);
    expect(config.durable_objects?.bindings).toEqual([
      { name: "DRAFT_CLAIM_LOCK", class_name: "DraftClaimLock", script_name: "codewhale-web" },
    ]);
    expect(config.exports).toEqual({ DraftClaimLock: { type: "durable-object", storage: "sqlite" } });
    expect(config.routes).toEqual([
      { pattern: "codewhale.net", custom_domain: true },
      { pattern: "www.codewhale.net", custom_domain: true },
    ]);
    expect(config.services).toEqual([{ binding: "WORKER_SELF_REFERENCE", service: "codewhale-web" }]);
  });
});
