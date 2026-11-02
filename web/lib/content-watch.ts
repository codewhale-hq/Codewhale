/**
 * content-watch.ts — two daily watchers that catch site drift the mechanical
 * facts pipeline misses:
 *
 *   runLinkCheck     — pings every external URL referenced in the site copy,
 *                      writes a draft per broken link (4xx/5xx). Stores a
 *                      `linkcheck:last` summary so /admin can show last status.
 *
 *   runSemanticDrift — reads recent CHANGELOG / commits, asks deepseek-flash
 *                      whether any specific claims on the site look out of
 *                      date, writes review-required drafts.
 *
 * Both surface as drafts in CURATED_KV under `draft:linkcheck:<...>` and
 * `draft:semantic-drift:<...>`, picked up by the existing /admin listing.
 */
import { fetchBoundedText } from "./bounded-body";
import { agentChat, draftStorageKey, getDraft, saveDraft, type AgentDraft, type DeepSeekEnv, type DraftClaimStores, VOICE_CONSTRAINTS } from "./community-agent";

// --- Canonical draft identities ---
//
// Watcher draft IDs are deterministic: a readable slug plus a ~64-bit
// SHA-256 suffix over the finding's full identity, capped at 80 characters.
// Re-running a watcher over an unchanged finding reproduces the same key (so
// the open-draft dedup check skips it); any change to the finding's identity
// produces a new key (so changed findings are drafted again instead of being
// silently swallowed by a truncated-prefix collision).
const WATCH_DRAFT_ID_MAX = 80;
const WATCH_DRAFT_HASH_HEX = 16; // 64 bits of SHA-256

async function sha256Hex(input: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(input));
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

function slugify(input: string): string {
  return input.replace(/[^a-z0-9]+/gi, "-").replace(/^-+|-+$/g, "").toLowerCase();
}

export async function watchDraftId(slugSource: string, identity: string): Promise<string> {
  const hash = (await sha256Hex(identity)).slice(0, WATCH_DRAFT_HASH_HEX);
  const slug = slugify(slugSource).slice(0, WATCH_DRAFT_ID_MAX - WATCH_DRAFT_HASH_HEX - 1);
  return `${slug}-${hash}`;
}

interface WatchEnv extends DraftClaimStores {
  DEEPSEEK_API_KEY?: string;
  DEEPSEEK_BASE_URL?: string;
  DEEPSEEK_MODEL?: string;
  GITHUB_TOKEN?: string;
}

function dsEnv(env: WatchEnv): DeepSeekEnv {
  return {
    baseUrl: env.DEEPSEEK_BASE_URL ?? process.env.DEEPSEEK_BASE_URL,
    model: env.DEEPSEEK_MODEL ?? process.env.DEEPSEEK_MODEL,
  };
}

// --- Link checker ---

// Targets to probe daily. For registries that block bot HEAD/GET (npm, crates.io)
// we hit the public JSON API instead — same upstream, doesn't 403.
const LINK_TARGETS: { url: string; label: string }[] = [
  { url: "https://github.com/codewhale-hq/CodeWhale", label: "Main repo" },
  { url: "https://github.com/codewhale-hq/CodeWhale/issues", label: "Issues" },
  { url: "https://github.com/codewhale-hq/CodeWhale/pulls", label: "Pull Requests" },
  { url: "https://github.com/codewhale-hq/CodeWhale/discussions", label: "Discussions" },
  { url: "https://github.com/codewhale-hq/CodeWhale/releases", label: "Releases" },
  { url: "https://github.com/codewhale-hq/CodeWhale/blob/main/LICENSE", label: "License file" },
  { url: "https://github.com/codewhale-hq/CodeWhale/blob/main/.github/CODE_OF_CONDUCT.md", label: "Code of Conduct" },
  { url: "https://github.com/codewhale-hq/CodeWhale/blob/main/.github/SECURITY.md", label: "Security policy" },
  { url: "https://github.com/codewhale-hq/CodeWhale/blob/main/CONTRIBUTING.md", label: "Contributing guide" },
  { url: "https://github.com/codewhale-hq/CodeWhale/blob/main/.github/PULL_REQUEST_TEMPLATE.md", label: "PR template" },
  { url: "https://github.com/Hmbown/homebrew-deepseek-tui", label: "Homebrew tap" },
  { url: "https://github.com/sponsors/Hmbown", label: "Support link (GitHub Sponsors)" },
  { url: "https://buymeacoffee.com/hmbown", label: "Support link (BMC)" },
  { url: "https://registry.npmjs.org/codewhale", label: "npm package (registry API)" },
  // crates.io intentionally not in this list — both their HTML and JSON API return 403 to
  // Cloudflare Workers, so the check produces false positives. The crate links on the site
  // still work for human users.
];

export interface LinkCheckResult {
  url: string;
  label: string;
  status: number | "error";
  ok: boolean;
  ms: number;
}

const PROBE_TIMEOUT_MS = 10_000;

async function probe(target: { url: string; label: string }): Promise<LinkCheckResult> {
  const start = Date.now();
  try {
    // Use HEAD where possible; fall back to GET on 405/403 since some hosts
    // (e.g. Cloudflare-protected) reject HEAD.
    // Each request has its own deadline; only the status matters, so a GET
    // body is discarded unread.
    let r = await fetch(target.url, { method: "HEAD", redirect: "follow", signal: AbortSignal.timeout(PROBE_TIMEOUT_MS) });
    if (r.status === 405 || r.status === 403 || r.status === 404) {
      // Some sites return 404 to HEAD but 200 to GET (e.g. NPM)
      r = await fetch(target.url, { method: "GET", redirect: "follow", signal: AbortSignal.timeout(PROBE_TIMEOUT_MS) });
      await r.body?.cancel().catch(() => undefined);
    }
    return { url: target.url, label: target.label, status: r.status, ok: r.ok, ms: Date.now() - start };
  } catch {
    return { url: target.url, label: target.label, status: "error", ok: false, ms: Date.now() - start };
  }
}

export async function runLinkCheck(env: WatchEnv): Promise<{ ok: boolean; checked: number; broken: number; results?: LinkCheckResult[] }> {
  if (!env.CURATED_KV || !env.DRAFT_CLAIM_LOCK) return { ok: false, checked: 0, broken: 0 };

  const results = await Promise.all(LINK_TARGETS.map(probe));
  const broken = results.filter((r) => !r.ok);

  await env.CURATED_KV.put("linkcheck:last", JSON.stringify({
    at: new Date().toISOString(),
    checked: results.length,
    broken: broken.length,
    results,
  }), { expirationTtl: 60 * 60 * 24 * 14 });

  // Write drafts ONLY for new breakages — dedup on the canonical draft key,
  // derived from the full URL identity (the hash suffix keeps long URLs with
  // identical slug prefixes distinct).
  for (const b of broken) {
    const id = await watchDraftId(b.url, `linkcheck\n${b.url}`);
    const draft: AgentDraft = {
      id,
      type: "linkcheck",
      targetUrl: b.url,
      bodyEn: `**Broken link** (auto-detected by daily watch cron)\n\n- Label: **${b.label}**\n- URL: ${b.url}\n- HTTP status: ${b.status}\n- Latency: ${b.ms}ms\n\nThis URL is referenced in codewhale.net copy. Update the source page or fix the destination.\n\n— drafted by community assistant, pending maintainer review`,
      bodyZh: `**链接失效**（每日巡检自动发现）\n\n- 名称：**${b.label}**\n- 地址：${b.url}\n- HTTP 状态：${b.status}\n- 延迟：${b.ms}ms\n\n该地址被 codewhale.net 文案引用，请更新源页面或修复目标。\n\n— 由社区助理草拟，待维护者审阅`,
      generatedAt: new Date().toISOString(),
      posted: false,
    };
    const existing = await getDraft(env.CURATED_KV, draftStorageKey(draft));
    if (existing) continue; // already flagged; don't churn

    await saveDraft(env, draft);
  }

  return { ok: true, checked: results.length, broken: broken.length, results: broken };
}

// --- Semantic drift ---

const SEMANTIC_DRIFT_PROMPT = `You are reviewing copy on a community website (codewhale.net) for the open-source Codewhale project.

Given:
1. The CHANGELOG entries below (most recent first)
2. The current homepage and docs page text below
3. Recent commit messages

Identify any factual claims on the site that are CONTRADICTED by recent changes. Be conservative — only flag claims you can directly tie to a CHANGELOG line or commit. Don't speculate.

Return ONLY this JSON shape (no prose, no markdown fences):
{
  "drifts": [
    {
      "page": "homepage" | "docs" | "install" | "contribute" | "roadmap",
      "claim": "exact text on the site that is now inaccurate",
      "evidence": "the CHANGELOG line or commit hash that contradicts it",
      "suggested_replacement": "what the site should say instead"
    }
  ]
}

If nothing is drifted, return { "drifts": [] }.

${VOICE_CONSTRAINTS}`;

function startsWithAsciiCI(input: string, index: number, needle: string): boolean {
  if (index + needle.length > input.length) return false;
  return input.slice(index, index + needle.length).toLowerCase() === needle;
}

function isWhitespace(c: string | undefined): boolean {
  return c === " " || c === "\n" || c === "\r" || c === "\t" || c === "\f";
}

function tagNameBoundary(input: string, index: number): boolean {
  const c = input[index];
  return c === undefined || c === ">" || c === "/" || isWhitespace(c);
}

function findClosingRawTextTag(input: string, from: number, tagName: "script" | "style"): number {
  const closePrefix = `</${tagName}`;
  for (let i = from; i < input.length; i += 1) {
    if (startsWithAsciiCI(input, i, closePrefix) && tagNameBoundary(input, i + closePrefix.length)) {
      const close = input.indexOf(">", i + closePrefix.length);
      return close === -1 ? input.length : close + 1;
    }
  }
  return input.length;
}

function collapseWhitespace(input: string): string {
  let out = "";
  let pendingSpace = false;
  for (const c of input) {
    if (isWhitespace(c)) {
      pendingSpace = out.length > 0;
      continue;
    }
    if (pendingSpace) out += " ";
    out += c;
    pendingSpace = false;
  }
  return out.trim();
}

function stripHtmlForPrompt(input: string): string {
  let out = "";
  for (let i = 0; i < input.length;) {
    if (input[i] !== "<") {
      out += input[i];
      i += 1;
      continue;
    }

    if (startsWithAsciiCI(input, i, "<script") && tagNameBoundary(input, i + "<script".length)) {
      out += " ";
      const openEnd = input.indexOf(">", i + 1);
      i = openEnd === -1 ? input.length : findClosingRawTextTag(input, openEnd + 1, "script");
      continue;
    }
    if (startsWithAsciiCI(input, i, "<style") && tagNameBoundary(input, i + "<style".length)) {
      out += " ";
      const openEnd = input.indexOf(">", i + 1);
      i = openEnd === -1 ? input.length : findClosingRawTextTag(input, openEnd + 1, "style");
      continue;
    }

    out += " ";
    const tagEnd = input.indexOf(">", i + 1);
    i = tagEnd === -1 ? input.length : tagEnd + 1;
  }
  return collapseWhitespace(out).slice(0, 8000);
}

// The model's drift list is untrusted input: bound it before any KV fanout so
// a runaway or hostile response cannot flood the draft queue, and drop entries
// whose shape does not match the prompt's contract.
const MAX_DRIFT_DRAFTS_PER_RUN = 10;
const DRIFT_FIELD_MAX = 2_000;
const DRIFT_PAGES = new Set(["homepage", "docs", "install", "contribute", "roadmap"]);

interface DriftFinding {
  page: string;
  claim: string;
  evidence: string;
  suggested_replacement: string;
}

function validateDriftFindings(raw: unknown): DriftFinding[] {
  if (!Array.isArray(raw)) return [];
  const findings: DriftFinding[] = [];
  for (const entry of raw) {
    if (findings.length >= MAX_DRIFT_DRAFTS_PER_RUN) break;
    if (typeof entry !== "object" || entry === null) continue;
    const { page, claim, evidence, suggested_replacement } = entry as Record<string, unknown>;
    if (typeof page !== "string" || !DRIFT_PAGES.has(page)) continue;
    if (typeof claim !== "string" || !claim.trim()) continue;
    if (typeof evidence !== "string" || !evidence.trim()) continue;
    if (typeof suggested_replacement !== "string" || !suggested_replacement.trim()) continue;
    findings.push({
      page,
      claim: claim.slice(0, DRIFT_FIELD_MAX),
      evidence: evidence.slice(0, DRIFT_FIELD_MAX),
      suggested_replacement: suggested_replacement.slice(0, DRIFT_FIELD_MAX),
    });
  }
  return findings;
}

export async function runSemanticDrift(env: WatchEnv): Promise<{ ok: boolean; drafted: number; reason?: string }> {
  if (!env.CURATED_KV || !env.DRAFT_CLAIM_LOCK || !env.DEEPSEEK_API_KEY) {
    return { ok: false, drafted: 0, reason: "missing CURATED_KV, DRAFT_CLAIM_LOCK or DEEPSEEK_API_KEY" };
  }

  const ghHeaders: Record<string, string> = {
    Accept: "application/vnd.github+json",
    "User-Agent": "codewhale-web-semantic-drift",
  };
  if (env.GITHUB_TOKEN) ghHeaders["Authorization"] = `Bearer ${env.GITHUB_TOKEN}`;

  // Fetch CHANGELOG (truncated), recent commits, and live homepage HTML.
  const [changelog, commits, homepageHtml, docsHtml] = await Promise.all([
    fetchBoundedText("https://raw.githubusercontent.com/codewhale-hq/CodeWhale/main/CHANGELOG.md", { headers: ghHeaders }).then((r) => r.text).catch(() => ""),
    fetchBoundedText("https://api.github.com/repos/codewhale-hq/CodeWhale/commits?per_page=30", { headers: ghHeaders }).then((r) => r.ok ? JSON.parse(r.text) as { commit: { message: string }; sha: string }[] : []).catch(() => []),
    fetchBoundedText("https://codewhale.net/en", { headers: { "User-Agent": "codewhale-watch" } }).then((r) => r.text).catch(() => ""),
    fetchBoundedText("https://codewhale.net/en/docs", { headers: { "User-Agent": "codewhale-watch" } }).then((r) => r.text).catch(() => ""),
  ]);

  if (!changelog && (!commits || commits.length === 0)) {
    return { ok: false, drafted: 0, reason: "no changelog or commits available" };
  }

  const homepageText = stripHtmlForPrompt(homepageHtml);
  const docsText = stripHtmlForPrompt(docsHtml);
  const changelogHead = changelog.slice(0, 4000);
  const commitMsgs = commits.slice(0, 30).map((c) => `- ${c.sha.slice(0, 7)}: ${c.commit.message.split("\n")[0]}`).join("\n");

  const userMessage = `## Recent CHANGELOG entries
${changelogHead || "(no CHANGELOG.md fetched)"}

## Last 30 commits
${commitMsgs || "(no commits fetched)"}

## Homepage text (HTML stripped)
${homepageText}

## Docs page text (HTML stripped)
${docsText}`;

  let response: { content: string; usage: { input: number; output: number } };
  try {
    response = await agentChat(
      [
        { role: "system", content: SEMANTIC_DRIFT_PROMPT },
        { role: "user", content: userMessage },
      ],
      env.DEEPSEEK_API_KEY,
      true,
      dsEnv(env),
    );
  } catch (e) {
    return { ok: false, drafted: 0, reason: `LLM call failed: ${e}` };
  }

  // Extract JSON (jsonMode usually returns clean JSON, but defend against fences)
  let parsed: { drifts?: unknown };
  try {
    const trimmed = response.content.replace(/^```(?:json)?\s*/i, "").replace(/\s*```\s*$/i, "").trim();
    parsed = JSON.parse(trimmed);
  } catch {
    return { ok: false, drafted: 0, reason: "LLM returned non-JSON" };
  }

  const drifts = validateDriftFindings(parsed.drifts);
  let drafted = 0;
  for (const d of drifts) {
    // Identity covers the full finding — page, claim, evidence, and
    // replacement — so a re-run over an unchanged finding dedups, while a
    // changed finding lands under a new key as a fresh draft.
    const identity = [d.page, d.claim, d.evidence, d.suggested_replacement].join("\n");
    const id = await watchDraftId(`${d.page}-${d.claim.slice(0, 40)}`, `semantic-drift\n${identity}`);
    const body = `Page: **${d.page}**\n\nClaim that may be drifted:\n> ${d.claim}\n\nEvidence:\n> ${d.evidence}\n\nSuggested replacement:\n> ${d.suggested_replacement}\n\n— drafted by community assistant, pending maintainer review`;
    const draft: AgentDraft = {
      id,
      type: "semantic-drift",
      targetUrl: `https://codewhale.net/en/${d.page === "homepage" ? "" : d.page}`,
      bodyEn: body,
      bodyZh: body,
      generatedAt: new Date().toISOString(),
      posted: false,
    };
    const existing = await getDraft(env.CURATED_KV, draftStorageKey(draft));
    if (existing) continue;

    // saveDraft refuses findings the maintainer already discarded.
    if (await saveDraft(env, draft)) drafted++;
  }

  return { ok: true, drafted };
}
