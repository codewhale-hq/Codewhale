import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

/**
 * Derive the untracked build inputs before any test file imports them.
 * `lib/changelog.generated.ts`, `lib/install-guide.generated.ts` and
 * `data/agent-docs.generated.json` are not committed (see web/.gitignore), so
 * a bare `vitest run` on a fresh checkout must write them from CHANGELOG.md,
 * docs/INSTALL.md and the registered docs sources first. Facts stay out
 * of this list on purpose: `facts.generated.ts` is committed, and rewriting it
 * here would hide the drift `npm run check:facts` exists to catch.
 */
export default function setup() {
  for (const script of ["derive-changelog.mjs", "derive-install.mjs", "derive-agent-docs.mjs"]) {
    execFileSync(process.execPath, [fileURLToPath(new URL(`./scripts/${script}`, import.meta.url))], {
      stdio: "inherit",
    });
  }
}
