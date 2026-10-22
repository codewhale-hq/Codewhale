import {
  chmodSync,
  existsSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { describe, expect, it } from "vitest";
import { FACTS } from "./facts.generated";
import { SNIPPETS, VERIFY } from "./install-binary-snippets";
import { INSTALL_GUIDE } from "./install-guide.generated";
import { getChrome, getHome } from "./i18n/dictionaries";
import { footerProjectLinks } from "./i18n/links";
import { TERMINAL_SCREENSHOT } from "./media-manifest";
import { PRODUCT_COPY } from "./content/product";

const root = new URL("../../", import.meta.url);

type PublicSurfaceMatrix = {
  schemaVersion: number;
  product: {
    name: string;
    description: string;
    license: string;
    terminology: Record<string, string>;
  };
  sourceCandidate: {
    version: string;
    providerCount: number;
    toolCount: number;
    sandboxBackends: string[];
  };
  latestPublishedRelease: {
    tag: string;
    version: string;
    publishedAt: string;
    url: string;
  };
  install: {
    recommended: string;
    binaries: string[];
    channels: Record<string, string>;
    androidTermux: {
      status: string;
      npm: string;
      requiresMatchingPublishedAssets: boolean;
      sourceBuild: boolean;
    };
  };
  control: {
    modes: string[];
    permissionPostures: string[];
    shortcuts: {
      mode: { chord: string; when: string };
      permissionPosture: { chord: string; when: string };
    };
  };
  toolSurface: {
    defaultActive: string[];
    schemas: Record<string, string[]>;
    deferred: Record<string, string[]>;
    compatibility: {
      legacyAliases: string;
      modelVisible: boolean;
      toolSearchDiscoverable: boolean;
    };
    activationCache: {
      maximumNames: number;
      maximumSchemaBytes: number;
      scope: string;
    };
    agentConcurrency: {
      defaultConfigured: number;
      maximumConfigured: number;
      maximumAdmitted: number;
    };
  };
  surfaces: { availableInSourceCandidate: string[] };
  trust: Record<string, string>;
  repository: {
    canonical: string;
    mirrors: string[];
    creditSources: string[];
    requiredCandidateCredits: string[];
  };
  screenshot: {
    readme: string;
    website: string;
    sourceVersion: string | null;
    sourceCommit: string | null;
    terminal: string;
    capture: string;
    sources: string[];
  };
  [key: string]: unknown;
};

const matrix = JSON.parse(text("docs/public-surface-facts.json")) as PublicSurfaceMatrix;

function text(path: string): string {
  return readFileSync(new URL(path, root), "utf8");
}

function bytes(path: string): Buffer {
  return readFileSync(new URL(path, root));
}

function pngDimensions(image: Buffer): [number, number] {
  expect(image.subarray(1, 4).toString("ascii")).toBe("PNG");
  return [image.readUInt32BE(16), image.readUInt32BE(20)];
}

/**
 * Dimensions of the canonical product screenshot. It is a lossless VP8L WebP,
 * so the size lives in the 14-bit-packed VP8L header rather than a PNG IHDR;
 * PNG is still accepted so this keeps working if the asset is ever swapped back.
 */
function imageDimensions(image: Buffer): [number, number] {
  if (image.subarray(1, 4).toString("ascii") === "PNG") return pngDimensions(image);
  expect(image.subarray(0, 4).toString("ascii")).toBe("RIFF");
  expect(image.subarray(8, 12).toString("ascii")).toBe("WEBP");
  const chunk = image.subarray(12, 16).toString("ascii");
  expect(chunk, "expected a lossless VP8L screenshot").toBe("VP8L");
  // VP8L: 1 signature byte, then width-1 and height-1 as 14-bit LE fields.
  const bits = image.readUInt32LE(21);
  return [(bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1];
}

function comparableVersion(value: string): number {
  const [major = 0, minor = 0, patch = 0] = value.split(".").map((n) => Number.parseInt(n, 10) || 0);
  return major * 1_000_000 + minor * 1_000 + patch;
}

describe("public surface contracts", () => {
  it("keeps source-candidate and published-release facts distinct and aligned", () => {
    expect(matrix.schemaVersion).toBe(2);
    expect(matrix.sourceCandidate.version).toBe(FACTS.version);
    expect(matrix.sourceCandidate.providerCount).toBe(FACTS.providers.length);
    expect(matrix.sourceCandidate.toolCount).toBe(FACTS.toolCount);
    expect(matrix.sourceCandidate.sandboxBackends).toEqual(FACTS.sandboxBackends);
    expect({
      tag: matrix.latestPublishedRelease.tag,
      version: matrix.latestPublishedRelease.version,
      publishedAt: matrix.latestPublishedRelease.publishedAt,
      url: matrix.latestPublishedRelease.url,
    }).toEqual(FACTS.latestPublishedRelease);
    // The published release may equal the source candidate: that is the normal
    // state in the window between shipping vX.Y.Z and opening the next lane.
    // What must never happen is the site advertising a version that is not
    // published yet, so assert published <= source candidate rather than
    // asserting they differ.
    expect(comparableVersion(matrix.latestPublishedRelease.version)).toBeLessThanOrEqual(
      comparableVersion(matrix.sourceCandidate.version),
    );
    expect(matrix.latestPublishedRelease).not.toHaveProperty("providerCount");
    expect(matrix.latestPublishedRelease).not.toHaveProperty("toolCount");
    expect(matrix.surfaces).not.toHaveProperty("stable");
    expect(matrix.surfaces.availableInSourceCandidate).toContain("Web client");
    // A capture may identify an exact build or explicitly remain an unversioned
    // current session. Never infer release provenance from pixels alone.
    const screenshotSourceVersion = matrix.screenshot.sourceVersion;
    if (screenshotSourceVersion !== null) {
      expect(comparableVersion(screenshotSourceVersion)).toBeLessThanOrEqual(
        comparableVersion(matrix.sourceCandidate.version),
      );
      expect(matrix.screenshot.sourceCommit).toMatch(/^[0-9a-f]{40}$/);
    } else {
      expect(matrix.screenshot.sourceCommit).toBeNull();
      expect(matrix.screenshot.capture).toContain("no published-release or exact-candidate claim");
    }
  });

  it("backs product and install claims with package and documentation content", () => {
    const readme = text("README.md");
    const npmReadme = text("npm/codewhale/README.md");
    const install = text("docs/INSTALL.md");
    const changelog = text("CHANGELOG.md");
    const license = text("LICENSE");
    const npmArtifacts = text("npm/codewhale/scripts/artifacts.js");
    const npmPackage = JSON.parse(text("npm/codewhale/package.json")) as {
      description: string;
      bin: Record<string, string>;
    };

    expect(matrix.product.name).toBe("Codewhale");
    expect(matrix.product.license).toBe("MIT");
    expect(matrix.product.description).toBe(npmPackage.description);
    expect(license).toContain("MIT License");
    expect(matrix.install.recommended).toBe("curl -fsSL https://codewhale.net/install.sh | sh");
    expect(readme).toContain(matrix.install.recommended);
    expect(Object.keys(npmPackage.bin)).toEqual(matrix.install.binaries);
    expect(matrix.install.channels).toEqual({
      npm: "published releases only",
      cargo: "published crates only",
      prebuiltArchives: "published GitHub Releases only",
      cnbMirror: "documented targets only",
    });
    expect(matrix.install.androidTermux).toEqual({
      status: "preview",
      npm: "preview",
      requiresMatchingPublishedAssets: true,
      sourceBuild: true,
    });
    // Pinned to FACTS.version, not a literal: these assertions used to carry
    // the version number by hand, so every release bump broke them and the
    // failure looked like a copy defect rather than a stale test.
    expect(install).toContain(`v${FACTS.version} source candidate`);
    expect(install).toContain("unpublished source candidate");
    const androidRow = install.split("\n").find((line) => line.startsWith("| Android / Termux |"));
    expect(androidRow).toContain("arm64 (aarch64)");
    expect(androidRow).toContain("`codewhale-android-arm64.tar.gz`");
    expect(androidRow).toContain("⚠️⁴ preview");
    expect(install).not.toContain(`wrapper is published at\nv${FACTS.version}`);
    expect(npmReadme).toMatch(/^- Android arm64 \/ Termux \(preview;/m);
    expect(npmReadme).toContain("requires matching Android assets");
    expect(npmArtifacts).toContain("android: {");
    for (const binary of ["codewhale-android-arm64", "codew-android-arm64"]) {
      expect(npmArtifacts).toContain(binary);
    }
    // Matched by string prefix rather than by building a RegExp from
    // FACTS.version: escaping only `.` left backslashes unescaped, which
    // CodeQL flagged as incomplete escaping. The version needs no regex.
    const heading = `## [${FACTS.version}] - `;
    const headingLine = changelog
      .split("\n")
      .find((line) => line.startsWith(heading));
    expect(headingLine, `missing "${heading}" changelog heading`).toBeTruthy();
    const headingSuffix = headingLine?.slice(heading.length) ?? "";
    expect(headingSuffix).toMatch(/^(?:Unreleased candidate|\d{4}-\d{2}-\d{2})$/);
    if (headingSuffix === "Unreleased candidate") {
      // Pre-tag candidate: notes still call it a source candidate, and the
      // version compare link must not claim a tagged endpoint yet.
      expect(changelog).toContain(`v${FACTS.version} source candidate`);
      expect(changelog).not.toContain(`compare/v${FACTS.version}...HEAD`);
    } else {
      // Dated release: intro names the version, and the version's compare
      // link points at a tag range (Unreleased may still use ...HEAD).
      expect(changelog).toContain(`Codewhale v${FACTS.version}`);
      const versionCompare = changelog
        .split("\n")
        .find((line) => line.startsWith(`[${FACTS.version}]: `));
      expect(versionCompare, `missing [${FACTS.version}] compare link`).toBeTruthy();
      expect(versionCompare).toContain(`...v${FACTS.version}`);
      expect(versionCompare).not.toContain("...HEAD");
    }
  });

  it("keeps one runtime and channel-specific command names exact", () => {
    const installDoc = text("docs/INSTALL.md");
    const installPage = text("web/app/[locale]/install/page.tsx");
    const npmReadme = text("npm/codewhale/README.md");
    const cliCargo = text("crates/cli/Cargo.toml");
    const cargoBinaryNames = Array.from(
      cliCargo.matchAll(/\[\[bin\]\]\s+name = "([^"]+)"/g),
      (match) => match[1],
    );

    expect(matrix.install.binaries).toEqual(["codewhale", "codew"]);
    expect(cargoBinaryNames).toEqual(["codewhale"]);
    expect(installDoc).toContain("One Cargo package is required");
    expect(installDoc).toContain("`codewhale-cli` installs the `codewhale` command");
    expect(installDoc).toContain("Cargo does\nnot create that alias");
    expect(installPage).toContain('from "@/lib/install-guide.generated"');
    expect(INSTALL_GUIDE.chunks.some((chunk) => chunk.kind === "code" && chunk.text.includes("cargo install --path crates/cli --locked"))).toBe(true);
    expect(installPage).not.toContain("codewhale-tui");
    expect(npmReadme).toContain("installs `codewhale` plus the `codew` convenience name");
    expect(npmReadme).not.toContain("codewhale-tui");
    for (const platform of [
      "macos-arm64",
      "macos-x64",
      "linux-arm64",
      "linux-x64",
    ] as const) {
      expect(SNIPPETS[platform], platform).toContain(`codew-${platform}`);
      expect(SNIPPETS[platform], platform).toContain(
        `sudo mv codew-${platform} /usr/local/bin/codew`,
      );
      expect(SNIPPETS[platform], platform).not.toContain("codewhale-tui");
      expect(VERIFY[platform], platform).not.toContain("codewhale-tui");
    }
    for (const arch of ["x64", "arm64"] as const) {
      expect(SNIPPETS[`windows-${arch}`], arch).toContain(`codew-windows-${arch}.exe`);
      expect(SNIPPETS[`windows-${arch}`], arch).toContain(
        'Get-FileHash "$dest\\codew.exe"',
      );
      expect(SNIPPETS[`windows-${arch}`], arch).not.toContain("codewhale-tui");
      expect(VERIFY[`windows-${arch}`], arch).not.toContain("codewhale-tui");
    }
  });

  it("checks Unix release assets under their manifest filenames before renaming", () => {
    const scratch = mkdtempSync(join(tmpdir(), "codewhale-install-checksum-"));
    const mockBin = join(scratch, "bin");
    const curlPath = join(mockBin, "curl");
    const checksumPath = join(mockBin, "checksum");

    try {
      const mkdir = spawnSync("/bin/mkdir", ["-p", mockBin]);
      expect(mkdir.status, mkdir.stderr.toString()).toBe(0);

      writeFileSync(
        curlPath,
        `#!/bin/sh
output=""
url=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o) shift; output="$1" ;;
    http*) url="$1" ;;
  esac
  shift
done
[ -n "$output" ] || output=$(basename "$url")
if [ "$output" = codewhale-artifacts-sha256.txt ]; then
  for platform in macos-arm64 macos-x64 linux-arm64 linux-x64; do
    for binary in codewhale codew; do
      printf 'fixture-hash  %s-%s\\n' "$binary" "$platform"
    done
  done > "$output"
else
  printf 'fixture payload for %s\\n' "$output" > "$output"
fi
`,
      );
      writeFileSync(
        checksumPath,
        `#!/bin/sh
while [ "$#" -gt 0 ]; do shift; done
while read -r _hash filename; do
  if [ ! -f "$filename" ]; then
    echo "manifest target missing: $filename" >&2
    exit 1
  fi
done
`,
      );
      chmodSync(curlPath, 0o755);
      chmodSync(checksumPath, 0o755);
      for (const command of ["shasum", "sha256sum"]) {
        const link = spawnSync("/bin/ln", ["-s", checksumPath, join(mockBin, command)]);
        expect(link.status, link.stderr.toString()).toBe(0);
      }

      for (const platform of [
        "macos-arm64",
        "macos-x64",
        "linux-arm64",
        "linux-x64",
      ] as const) {
        const lines = SNIPPETS[platform].split("\n");
        const checksumLine = lines.findIndex((line) => line.includes(" -c -"));
        expect(checksumLine, platform).toBeGreaterThan(-1);
        const result = spawnSync(
          "/bin/bash",
          ["-o", "pipefail", "-eu", "-c", lines.slice(0, checksumLine + 1).join("\n")],
          {
            cwd: scratch,
            env: { ...process.env, PATH: `${mockBin}:${process.env.PATH ?? ""}` },
          },
        );
        expect(result.status, `${platform}: ${result.stderr.toString()}`).toBe(0);
      }
    } finally {
      rmSync(scratch, { recursive: true, force: true });
    }
  });

  it("qualifies the resolved audit path and best-effort persistence", () => {
    const installPage = text("web/app/[locale]/install/page.tsx");

    expect(matrix.trust.audit).toContain("best-effort");
    expect(matrix.trust.audit).toContain("$CODEWHALE_HOME");
    // Install links to configuration instead of duplicating its storage map.
    expect(installPage).toContain("/docs/configuration");
    expect(installPage).not.toContain(
      "audit.log        credential / approval / elevation audit trail",
    );
  });

  it("keeps modes, permission postures, and idle shortcuts exact", () => {
    const modes = text("docs/MODES.md");
    const keys = text("docs/KEYBINDINGS.md");
    const homepage = text("web/app/[locale]/page.tsx");
    const docsMap = text("web/lib/docs-map.ts");
    const matrixText = text("docs/public-surface-facts.json");

    expect(matrix.control.modes).toEqual(["Plan", "Work", "Operate"]);
    expect(matrix.control.permissionPostures).toEqual(["Ask", "Auto-Review", "Full Access"]);
    // Tab is gated on the composer being EMPTY, not idle
    // (crates/tui/src/tui/ui.rs:6978 `if !app.input.is_empty() { continue; }`
    // immediately before `app.cycle_mode()`); Shift+Tab has no composer
    // precondition at all (ui.rs:6363-6367 gates only on the modal stack).
    expect(matrix.control.shortcuts).toEqual({
      mode: { chord: "Tab", when: "composer empty" },
      permissionPosture: {
        chord: "Shift+Tab",
        when: "always (suppressed only under a non-Config modal)",
      },
    });
    for (const label of [...matrix.control.modes, ...matrix.control.permissionPostures]) {
      expect(modes).toContain(label);
    }
    expect(PRODUCT_COPY.modes.map(({ title }) => title.en)).toEqual(matrix.control.modes);
    expect(PRODUCT_COPY.permissions.map(({ title }) => title.en)).toEqual(matrix.control.permissionPostures);
    expect(modes).toContain("when the composer is empty");
    expect(keys).toContain("When the composer is empty, cycle TUI mode");
    expect(keys).toContain("`Shift+Tab`");
    // Detailed key semantics belong to the canonical keybinding/mode docs, not
    // the concise README or every translation.
    expect(keys).not.toContain("When the composer is idle");
    expect(`${modes}\n${homepage}\n${docsMap}`).not.toContain("approval posture");
    expect(matrixText).not.toContain('"approvalPostures"');
  });

  it("keeps public tool facts aligned with the native core and discovery boundaries", () => {
    const toolDoc = text("docs/TOOL_SURFACE.md");
    const toolsPage = text("web/app/[locale]/docs/tools/page.tsx") + text("web/lib/content/tools.ts");
    const registry = text("crates/tui/src/tools/registry.rs");
    const limits = text("crates/tui/src/config/subagent_limits.rs");
    const roadmap = text("web/app/[locale]/roadmap/page.tsx");

    const catalog = text("crates/tui/src/core/engine/tool_catalog.rs");
    const nativeCore = catalog.match(/const DEFAULT_ACTIVE_NATIVE_TOOLS: &[\s\S]*?= &\[([\s\S]*?)\];/);
    expect(nativeCore, "native core declaration must be found").not.toBeNull();
    const nativeNames = [...nativeCore![1].matchAll(/"([^"\n]+)"/g)].map((match) => match[1]);
    expect(nativeNames.length).toBeGreaterThan(0);
    expect(matrix.toolSurface.defaultActive).toEqual(nativeNames);
    expect(matrix.toolSurface.schemas).toEqual({
      read: ["path", "offset?", "limit?"],
      write: ["path", "content"],
      edit: ["path", "edits"],
      bash: ["command", "timeout?"],
    });
    expect(matrix.toolSurface.deferred).toEqual({ Web: ["search", "fetch", "wait"] });
    expect(matrix.toolSurface.compatibility).toEqual({
      legacyAliases: "hidden-exact",
      modelVisible: false,
      toolSearchDiscoverable: false,
    });
    expect(matrix.toolSurface.activationCache).toEqual({
      maximumNames: 8,
      maximumSchemaBytes: 16_384,
      scope: "per conversation; each subagent owns an independent policy-filtered cache",
    });
    expect(matrix.toolSurface.agentConcurrency).toEqual({
      defaultConfigured: 64,
      maximumConfigured: 128,
      maximumAdmitted: 1024,
    });
    expect(limits).toContain("DEFAULT_MAX_SUBAGENTS: usize = 64");
    expect(limits).toContain("MAX_SUBAGENTS: usize = 128");
    expect(limits).toContain("MAX_SUBAGENT_ADMISSION: usize = 1024");
    expect(roadmap).toContain("64 concurrent sessions by default, configurable to 128");
    expect(roadmap.indexOf('{ title: "Local web client"')).toBeLessThan(
      roadmap.indexOf('title: "Underway"'),
    );
    expect(roadmap).toContain("In published releases: `codewhale web`");
    for (const name of matrix.toolSurface.defaultActive) {
      expect(toolDoc, name).toContain(`\`${name}\``);
      expect(toolsPage, name).toContain(name);
    }
    expect(toolDoc).toContain("`Web` is conditional and deferred");
    expect(toolDoc).toContain("Each subagent gets its own policy-filtered deferred catalog");
    expect(toolDoc).toContain("Compatibility is execution compatibility, not fuzzy aliasing");
    expect(registry).toContain("Arc::new(ReadTool)");
    expect(registry).toContain("Arc::new(WriteTool)");
    expect(registry).toContain("Arc::new(EditTool)");
    expect(registry).toContain("Arc::new(LowercaseBashTool)");
    expect(registry).toContain('FileTool::new("File")');
    expect(registry).toContain('BashTool::new("Bash")');
    expect(toolsPage).toContain("8 names / 24 KiB");
    expect(toolsPage).toContain("Web search/fetch");
    expect(toolsPage).not.toContain("docs/TOOL_LIFECYCLE.md");
  });

  it("states the hosted-provider privacy boundary without a false local-only promise", () => {
    const faq = text("web/app/[locale]/faq/page.tsx");
    const roadmap = text("web/app/[locale]/roadmap/page.tsx");
    const providers = text("docs/PROVIDERS.md");
    const runtime = text("docs/RUNTIME_API.md");

    expect(matrix.trust.hostedProviderBoundary).toContain("selected hosted provider");
    expect(matrix.trust.localInference).toContain("loopback local-model route");
    // The source candidate counts by default and says so. Disclosure alone
    // is never acceptance, every opt-out stays authoritative, and the
    // published release's earlier opt-in behavior is named, not blurred.
    expect(matrix.trust.telemetry).toContain(`Codewhale ${matrix.sourceCandidate.version} counts anonymous usage by default`);
    expect(matrix.trust.telemetry).toContain("discloses it at first launch");
    expect(matrix.trust.telemetry).toContain("policy notice version 5");
    expect(matrix.trust.telemetry).toContain("Codewhale and PostHog");
    expect(matrix.trust.telemetry).toContain("earlier 0.9.11 release asked first");
    expect(matrix.trust.telemetry).toContain("never records any acceptance");
    expect(matrix.trust.telemetry).toContain("opt-out recorded under the earlier opt-in policy stays off");
    expect(matrix.trust.telemetry).not.toContain("requires explicit consent");
    expect(matrix.trust.telemetry).toContain("does not collect conversations");
    expect(matrix.trust.telemetry).toContain("per-turn/per-tool timelines");
    // The destination is now named, and named exactly — a trust claim that says
    // "an endpoint" without saying which one is not a trust claim.
    expect(matrix.trust.telemetry).toContain(
      "https://telemetry.codewhale.net/v1/telemetry",
    );
    expect(matrix.trust.telemetry).toContain("no IP, country, or geo column");
    expect(matrix.trust.telemetry).toContain("three-month retention");
    // The local dry-run path stays documented, because it is what lets a user
    // audit the schema against their own traffic.
    expect(matrix.trust.telemetry).toContain("local dry-run file");
    expect(matrix.trust.telemetry).toContain("no mandatory hosted relay");
    expect(faq).toContain("The hosted");
    expect(faq).toContain("provider you select receives the prompt");
    expect(faq).toContain("keep model inference local");
    expect(faq).toContain("你选择的托管 provider 会收到");
    expect(faq).not.toContain("No telemetry, no cloud processing of your code");
    expect(faq).not.toContain("不会将你的代码上传到云端处理");
    expect(roadmap).not.toContain("what happens there stays there");
    expect(roadmap).not.toContain("你的数据不会离开");
    expect(providers).toMatch(/Hosted\s+routes/);
    expect(runtime).toContain("No hosted relay");
  });

  it("backs product vocabulary, contributor credit, and the exact MIT footer", () => {
    const fleet = text("docs/FLEET.md");
    const changelog = text("CHANGELOG.md");
    const contributors = text("docs/CONTRIBUTORS.md");
    const releaseCredits = text("web/lib/release-credits.ts");
    const footer = text("web/components/footer.tsx");

    expect(matrix.product.terminology).toEqual({
      Fleet: "the user's model inventory: who is in the roster and which member is selected",
      Workflow: "what order the work follows",
      Lane: "one running Workflow instance",
      Runtime: "where, how, and with what authority selected work executes",
    });
    for (const [term, definition] of Object.entries(matrix.product.terminology)) {
      expect(fleet).toContain(`**${term}** = ${definition}`);
    }
    expect(matrix.repository.requiredCandidateCredits).not.toHaveLength(0);
    expect(matrix.repository.mirrors.some((mirror) => mirror.includes("gitee"))).toBe(false);
    for (const handle of matrix.repository.requiredCandidateCredits) {
      expect(changelog).toContain(handle);
      expect(contributors).toContain(`github.com/${handle.slice(1)}`);
      expect(releaseCredits).toContain(`"${handle}"`);
    }
    // The footer link sets are generated from the locale dictionaries
    // (lib/i18n/links.ts) rather than hardcoded per-locale arrays, so the
    // exact MIT pairing is asserted on the rendered contract for both the
    // English and Chinese editions — same guarantee, one source.
    expect(footerProjectLinks("en", getChrome("en")).at(-1)).toEqual({
      label: "MIT license",
      href: "https://github.com/codewhale-hq/CodeWhale/blob/main/LICENSE",
    });
    expect(footerProjectLinks("zh", getChrome("zh")).at(-1)).toEqual({
      label: "MIT 许可证",
      href: "https://github.com/codewhale-hq/CodeWhale/blob/main/LICENSE",
    });
    expect(footer).toContain("href={REPO_RELEASES_URL}");
    expect(text("web/lib/i18n/links.ts")).toContain(
      'export const REPO_RELEASES_URL = `${REPO_URL}/releases`',
    );
    expect(text("web/lib/i18n/links.ts")).toContain(
      'export const REPO_URL = "https://github.com/codewhale-hq/CodeWhale"',
    );
    expect(footer).toContain("GITEE_ENABLED &&");
  });

  it("keeps supplied terminal screenshots and website dimensions truthful", () => {
    // The site and README use the same measured native capture. Demo route
    // and development-source provenance remain explicit in capture metadata.
    const readmeImage = bytes(matrix.screenshot.readme);
    const websiteImage = bytes(matrix.screenshot.website);

    expect(imageDimensions(websiteImage)).toEqual([TERMINAL_SCREENSHOT.width, TERMINAL_SCREENSHOT.height]);
    expect(readmeImage).toEqual(websiteImage);
    expect(statSync(new URL(matrix.screenshot.readme, root)).size).toBeLessThan(500_000);
    expect(statSync(new URL(matrix.screenshot.website, root)).size).toBeLessThan(500_000);
    expect(matrix.screenshot.terminal).toContain("real PTY cell capture");
    // A development-build capture, never a release claim.
    expect(matrix.screenshot.capture).toContain("development build");
    const capture = JSON.parse(text("web/lib/terminal-captures/manifest.json"));
    expect(capture.baseCommit).toBe(matrix.screenshot.sourceCommit);
    expect(capture.binarySha256).toMatch(/^[0-9a-f]{64}$/);
    expect(capture.sourceDiffSha256).toMatch(/^[0-9a-f]{64}$/);
    expect(capture.sourceDirty).toBe(true);
    expect(capture.conditions.provider).toContain("loopback");
    expect(capture.conditions.model).toBe("website-demo");
    expect(capture.conditions.submittedPrompts).toBeGreaterThan(0);
    expect(capture.conditions.fixtureHistory).toBe(false);

    const readme = text("README.md");
    const productPage = text("web/app/[locale]/product/page.tsx");
    expect(readme).toContain(matrix.screenshot.readme);
    expect(`web/public${TERMINAL_SCREENSHOT.src}`).toBe(matrix.screenshot.website);
    expect(imageDimensions(websiteImage)).toEqual([TERMINAL_SCREENSHOT.width, TERMINAL_SCREENSHOT.height]);
    expect(productPage).toContain("<NativeTerminalGallery");
    expect(TERMINAL_SCREENSHOT.capture).toBe("web/lib/terminal-captures/website-home-100x24.json");
    expect(matrix.screenshot.sources).toContain(TERMINAL_SCREENSHOT.capture);
    // Every locale describes the actual capture; build identity comes from
    // the media manifest instead of a stale version embedded in translations.
    expect(productPage).toContain("label={fill(home.screenshotAlt, { version: TERMINAL_SCREENSHOT.version })}");
    for (const locale of ["en", "zh", "ja", "vi", "ko", "ru", "uk", "es", "pt-BR", "id", "fr", "de", "ca", "hi", "tr", "it", "pl", "ar"]) {
      const home = getHome(locale);
      expect(home.screenshotAlt, `${locale} alt`).toContain("{version}");
    }
  });

  it("keeps the homepage free of fabricated demo panels", () => {
    const homepage = text("web/app/[locale]/page.tsx");

    expect(homepage).not.toContain("TerminalPlayer");
    expect(homepage).not.toContain("paper-decides");
    expect(homepage).not.toContain("product-receipt");
  });

  it("keeps every fact-matrix source resolvable in the repository", () => {
    const sources = new Set<string>();
    const visit = (value: unknown) => {
      if (Array.isArray(value)) {
        value.forEach(visit);
      } else if (value && typeof value === "object") {
        for (const [key, child] of Object.entries(value)) {
          if (key === "sources" || key === "creditSources") {
            (child as string[]).forEach((source) => sources.add(source));
          } else {
            visit(child);
          }
        }
      }
    };
    visit(matrix);

    for (const source of sources) {
      expect(existsSync(new URL(source, root)), source).toBe(true);
    }
  });
});
