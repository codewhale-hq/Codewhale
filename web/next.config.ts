import type { NextConfig } from "next";
import { execFileSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const webRoot = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(webRoot, "..");

function git(args: string[]): string | null {
  try {
    return execFileSync("git", args, {
      cwd: repoRoot,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    }).trim() || null;
  } catch {
    return null;
  }
}

const requestedSourceRevision =
  process.env.CODEWHALE_SOURCE_REVISION?.trim() ||
  process.env.GITHUB_SHA?.trim();
const gitSourceRevision = git(["rev-parse", "HEAD"]);
const sourceRevision = /^[0-9a-f]{40}$/i.test(requestedSourceRevision ?? "")
  ? requestedSourceRevision ?? ""
  : /^[0-9a-f]{40}$/i.test(gitSourceRevision ?? "")
    ? gitSourceRevision ?? ""
    : "";
const sourceCommittedAt =
  process.env.CODEWHALE_SOURCE_COMMITTED_AT?.trim() ||
  (sourceRevision ? git(["show", "-s", "--format=%cI", sourceRevision]) : null) ||
  "";

const factsGeneratedAt = new Date().toISOString();

// Security headers are set in middleware.ts (more reliable under OpenNext on
// Cloudflare than next.config.ts headers(), which doesn't always apply to
// prerendered/cached responses).
const nextConfig: NextConfig = {
  outputFileTracingRoot: webRoot,
  poweredByHeader: false,
  reactStrictMode: true,
  // Public, non-secret provenance for the exact source used to build the
  // deployed worker. Next inlines these values into server output, allowing a
  // credential-free post-deploy comparison without dirtying tracked facts.
  env: {
    NEXT_PUBLIC_CODEWHALE_SOURCE_REVISION: sourceRevision,
    NEXT_PUBLIC_CODEWHALE_SOURCE_COMMITTED_AT: sourceCommittedAt,
    NEXT_PUBLIC_CODEWHALE_FACTS_GENERATED_AT: factsGeneratedAt,
  },
  images: {
    remotePatterns: [
      { protocol: "https", hostname: "avatars.githubusercontent.com" },
    ],
  },
  typedRoutes: false,
};

export default nextConfig;

if (process.env.NODE_ENV === "development") {
  // Initialize Cloudflare bindings (KV, etc.) when running `next dev`.
  // No-op in production builds.
  void import("@opennextjs/cloudflare").then(({ initOpenNextCloudflareForDev }) => {
    initOpenNextCloudflareForDev({ configPath: ".cloudflare/opennext.json" });
  }).catch(() => { /* dev-only convenience */ });
}
