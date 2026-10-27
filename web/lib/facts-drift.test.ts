import { afterEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { isRepoFacts } from "./facts";
import { deriveFactsFromRemote, runFactsDrift } from "./facts-drift";

const REVISION = "b".repeat(40);

function response(body: string, status = 200): Response {
  return new Response(body, { status });
}

const VALID_GENERATED_FACTS =
  'export const FACTS: RepoFacts = {"toolCount":73,"contributorCount":390,"models":[]};';

function installGitHubFixture(
  toolCountSource: string | null,
  releaseHtmlUrl = "https://github.com/codewhale-hq/CodeWhale/releases/tag/v0.9.0",
  sourceOverrides: Record<string, string> = {},
): void {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: string | URL | Request) => {
      const url = String(input);
      if (url.endsWith("/commits/main")) {
        return response(
          JSON.stringify({
            sha: REVISION,
            commit: { committer: { date: "2026-07-21T23:00:00Z" } },
          }),
        );
      }
      if (url.endsWith("/releases/latest")) {
        return response(
          JSON.stringify({
            tag_name: "v0.9.0",
            published_at: "2026-07-16T20:05:39Z",
            html_url: releaseHtmlUrl,
          }),
        );
      }
      const rawPath = url.split(`/${REVISION}/`)[1];
      const sources: Record<string, string> = {
        "Cargo.toml": 'version = "0.9.2"\nmembers = ["crates/tui"]',
        "crates/config/assets/provider_descriptors.json": JSON.stringify({
          schema_version: 3,
          providers: [{ kind: "Deepseek", id: "deepseek", label: "Remote DeepSeek", default_model: "remote-model", env_vars: ["REMOTE_KEY"], retired: false, selectable: true, web: { order: 0 }, tui_wire_tag: "deepseek", catalog_id: "deepseek", catalog_source_id: "deepseek" }],
          constant_refs: { DEFAULT_TEXT_MODEL: { provider: "deepseek", field: "default_model" } },
        }),
        "crates/config/assets/models_dev.bundled.json": JSON.stringify({ _reviewed: {
          revision: "remote-reviewed", intrinsic: { "deepseek-v4-pro": { source: "exact remote fixture", context_window: 1000000, max_output: 128000, reasoning: true } },
          public_models: [{ id: "deepseek-v4-pro", label: "DeepSeek", added_at: "2026-07-01", aliases: [] }],
        } }),
        "crates/sandbox/src/lib.rs": `
          pub const PUBLIC_SANDBOX_BACKENDS: &[&str] = &[
            "seatbelt (macOS, when available)",
            "bubblewrap (Linux, opt-in when installed)",
          ];
        `,
        "npm/codewhale/package.json": JSON.stringify({ engines: { node: ">=18" } }),
        LICENSE: "MIT License\n",
        ...sourceOverrides,
      };
      if (rawPath === "web/lib/facts.generated.ts") {
        return toolCountSource === null ? response("not found", 404) : response(toolCountSource);
      }
      return rawPath && rawPath in sources
        ? response(sources[rawPath])
        : response("not found", 404);
    }),
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("deriveFactsFromRemote", () => {
  it("derives counts and model rows from the same exact remote revision", async () => {
    installGitHubFixture(
      'export const FACTS: RepoFacts = {"toolCount":73,"contributorCount":390,"models":[' +
        '{"id":"deepseek-v4-pro","provider":"DeepSeek","contextWindow":1000000,' +
        '"maxOutput":128000,"reasoning":true,"addedAt":"2026-07-01"}' +
        "]};",
    );

    const facts = await deriveFactsFromRemote();

    expect(facts?.sourceRevision).toBe(REVISION);
    expect(facts?.version).toBe("0.9.2");
    expect(facts?.toolCount).toBe(73);
    expect(facts?.contributorCount).toBe(390);
    expect(facts?.defaultModel).toBe("remote-model");
    expect(facts?.providers).toEqual([{ id: "deepseek", label: "Remote DeepSeek", env: "REMOTE_KEY" }]);
    expect(facts?.models).toEqual([
      {
        id: "deepseek-v4-pro",
        provider: "DeepSeek",
        contextWindow: 1000000,
        maxOutput: 128000,
        reasoning: true,
        addedAt: "2026-07-01",
      },
    ]);
    expect(facts?.sandboxBackends).toEqual([
      "seatbelt (macOS, when available)",
      "bubblewrap (Linux, opt-in when installed)",
    ]);
    const fetchMock = vi.mocked(fetch);
    expect(
      fetchMock.mock.calls.some(([input]) => String(input).includes("/contents/")),
    ).toBe(false);
  });

  it("refuses missing, malformed or incompatible exact-revision provider metadata", async () => {
    for (const metadata of ["not json", "{}", '{"schema_version":1,"providers":[]}']) {
      installGitHubFixture(VALID_GENERATED_FACTS, undefined, {
        "crates/config/assets/provider_descriptors.json": metadata,
      });
      await expect(deriveFactsFromRemote()).resolves.toBeNull();
    }
  });

  it("fails derivation when the exact revision has no valid tool count", async () => {
    installGitHubFixture(null);

    await expect(deriveFactsFromRemote()).resolves.toBeNull();
  });

  it("refuses missing or malformed model metadata from the exact source revision", async () => {
    for (const catalog of ["not json", "{}", '{"_reviewed":{"revision":"x","intrinsic":{},"public_models":[{"id":42}]}}']) {
      installGitHubFixture(VALID_GENERATED_FACTS, undefined, { "crates/config/assets/models_dev.bundled.json": catalog });
      await expect(deriveFactsFromRemote()).resolves.toBeNull();
    }
  });

  it("stores a canonical release URL when GitHub answers with the repo's other casing", async () => {
    installGitHubFixture(
      VALID_GENERATED_FACTS,
      "https://github.com/codewhale-hq/Codewhale/releases/tag/v0.9.0",
    );

    const facts = await deriveFactsFromRemote();

    expect(facts?.latestPublishedRelease?.url).toBe(
      "https://github.com/codewhale-hq/CodeWhale/releases/tag/v0.9.0",
    );
    expect(isRepoFacts(facts)).toBe(true);
  });
});

describe("runFactsDrift", () => {
  it("writes a KV snapshot that getFacts() accepts", async () => {
    installGitHubFixture(
      VALID_GENERATED_FACTS,
      "https://github.com/codewhale-hq/Codewhale/releases/tag/v0.9.0",
    );
    const store = new Map<string, string>();
    const kv = {
      get: async (key: string) => store.get(key) ?? null,
      put: async (key: string, value: string) => {
        store.set(key, value);
      },
    };

    const result = await runFactsDrift({ CURATED_KV: kv });

    expect(result.ok).toBe(true);
    expect(isRepoFacts(JSON.parse(store.get("facts:current") ?? "null"))).toBe(true);
  });

  it("never replaces a snapshot from a newer source commit with an older one", async () => {
    installGitHubFixture(VALID_GENERATED_FACTS);
    const newer = JSON.stringify({ sourceCommittedAt: "2026-07-22T00:00:00Z", version: "9.9.9" });
    const store = new Map<string, string>([["facts:current", newer]]);
    const kv = {
      get: async (key: string) => store.get(key) ?? null,
      put: async (key: string, value: string) => { store.set(key, value); },
    };
    // The fixture's source commit is 2026-07-21T23:00:00Z: an overlapping,
    // slower run finishing after a newer one.
    expect(await runFactsDrift({ CURATED_KV: kv })).toEqual({ ok: true, changed: false });
    expect(store.get("facts:current")).toBe(newer);
  });

  // The scheduled handler discards the result, so the log line is the only
  // signal that the cron stopped refreshing KV.
  it("warns and writes nothing when the derived facts fail validation", async () => {
    installGitHubFixture(VALID_GENERATED_FACTS, undefined, {
      // A non-string engines.node survives derivation but fails isRepoFacts.
      "npm/codewhale/package.json": JSON.stringify({ engines: { node: 18 } }),
    });
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const store = new Map<string, string>();
    const kv = {
      get: async (key: string) => store.get(key) ?? null,
      put: async (key: string, value: string) => {
        store.set(key, value);
      },
    };

    const result = await runFactsDrift({ CURATED_KV: kv });

    expect(result).toEqual({ ok: false, reason: "remote facts failed validation" });
    expect(store.size).toBe(0);
    expect(warn).toHaveBeenCalledWith(
      expect.stringContaining("[facts-drift] remote facts failed validation"),
    );
    warn.mockRestore();
  });
});

describe("complete remote provider roster", () => {
  it("keeps all public providers with the labels and credentials from that exact revision", async () => {
    const metadata = JSON.parse(readFileSync(new URL("../../crates/config/assets/provider_descriptors.json", import.meta.url), "utf8"));
    for (const row of metadata.providers) {
      if (row.web && !row.retired) {
        row.web.label = `Pinned ${row.id}`;
        row.web.env = `PINNED_${row.id}`;
      }
    }
    installGitHubFixture(VALID_GENERATED_FACTS, undefined, {
      "crates/runtime/src/config.rs": readFileSync(new URL("../../crates/runtime/src/config.rs", import.meta.url), "utf8"),
      "crates/config/assets/provider_descriptors.json": JSON.stringify(metadata),
    });
    const facts = await deriveFactsFromRemote();
    expect(facts?.sourceRevision).toBe(REVISION);
    expect(facts?.providers).toHaveLength(50);
    expect(new Set(facts?.providers.map(row => row.id)).size).toBe(50);
    expect(facts?.providers.every(row => row.label === `Pinned ${row.id}` && row.env === `PINNED_${row.id}`)).toBe(true);
  });
});
