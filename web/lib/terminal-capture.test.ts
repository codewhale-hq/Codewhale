import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { createHash } from "node:crypto";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { TERMINAL_SCREENSHOT } from "./media-manifest";
import { TERMINAL_CAPTURE_FRAMES, TERMINAL_CAPTURE_META } from "./terminal-capture.generated";

// The product page terminal is live text drawn from real PTY cells. These checks
// keep the generated runs tied to the capture files, cell for cell.
const webRoot = new URL("../", import.meta.url);

describe("terminal capture", () => {
  it("keeps the generated module current with the capture files", () => {
    const out = execFileSync(process.execPath, ["scripts/render-terminal-capture.mjs", "--check"], {
      cwd: webRoot,
      encoding: "utf8",
    });
    expect(out).toContain("is current");
  });

  it("keeps every frame's text identical to its captured cells", () => {
    for (const frame of Object.values(TERMINAL_CAPTURE_FRAMES)) {
      const capture = JSON.parse(readFileSync(new URL(`../../${frame.file}`, import.meta.url), "utf8")) as {
        rows: number;
        cols: number;
        cells: { text: string }[][];
      };
      expect(frame.lines).toHaveLength(capture.rows);
      frame.lines.forEach((runs, row) => {
        const text = runs.map(([t]) => t).join("");
        expect(text, `${frame.file} row ${row}`).toBe(capture.cells[row].map((c) => c.text || " ").join(""));
        expect([...text]).toHaveLength(capture.cols);
      });
    }
  });

  it("names its source and the build it came from", () => {
    expect(TERMINAL_CAPTURE_META.source).toContain("website_current_terminal_capture");
    expect(TERMINAL_CAPTURE_META.baseCommit).toBe(TERMINAL_SCREENSHOT.sourceCommit);
    expect(`web/lib/terminal-captures/website-home-100x24.json`).toBe(TERMINAL_SCREENSHOT.capture);
  });

  it("keeps the native workbar and every recorded frame tied to their capture hashes", () => {
    const manifest = JSON.parse(readFileSync(new URL("./terminal-captures/manifest.json", import.meta.url), "utf8"));
    expect(manifest.binarySha256).toMatch(/^[0-9a-f]{64}$/);
    expect(manifest.conditions.submittedPrompts).toBe(1);
    expect(manifest.conditions.fixtureHistory).toBe(false);
    expect(manifest.conditions.provider).toContain("loopback");
    expect(manifest.conditions.model).toBe("website-demo");
    expect(manifest.sourceDiffSha256).toMatch(/^[0-9a-f]{64}$/);
    for (const frame of manifest.frames) {
      const bytes = readFileSync(new URL(`./terminal-captures/${frame.file}`, import.meta.url));
      expect(createHash("sha256").update(bytes).digest("hex"), frame.file).toBe(frame.sha256);
      const capture = JSON.parse(bytes.toString());
      expect(capture.rows).toBe(frame.rows);
      expect(capture.cols).toBe(frame.cols);
    }
    const workbar = TERMINAL_CAPTURE_FRAMES.workbar.lines.map((runs) => runs.map(([text]) => text).join("")).join("\n");
    expect(workbar).toContain("--json");
    expect(workbar).not.toContain("no to-dos yet");
    expect(manifest.conditions.workspace).toBe("~/my-project inside a sealed HOME");
    expect(workbar).not.toContain(".tmp");
    const fleet = TERMINAL_CAPTURE_FRAMES["workbar-fleet"].lines.map((runs) => runs.map(([text]) => text).join("")).join("\n");
    expect(fleet).toContain("export-review");
    expect(fleet).not.toContain("no agents have run this session");
  });

  it("exports native geometry and every whale braille dot without relying on font fallback", () => {
    const directory = mkdtempSync(join(tmpdir(), "native-terminal-svg-"));
    try {
      const output = join(directory, "home.svg");
      execFileSync(process.execPath, ["scripts/render-terminal-capture.mjs", "--svg", output], { cwd: webRoot });
      const svg = readFileSync(output, "utf8");
      expect(svg).toContain('viewBox="0 0 1000 480"');
      const text = TERMINAL_CAPTURE_FRAMES.home.lines.flatMap((runs) => runs.map(([text]) => text)).join("");
      let dots = 0;
      for (const glyph of text) {
        const code = glyph.codePointAt(0)!;
        if (code >= 0x2800 && code <= 0x28ff) dots += (code - 0x2800).toString(2).replaceAll("0", "").length;
      }
      expect(svg.match(/<circle /g) ?? []).toHaveLength(dots);
      expect(svg).not.toContain("<script");
    } finally {
      rmSync(directory, { recursive: true, force: true });
    }
  });

  it("renders as one labelled image inside a keyboard-scrollable region", () => {
    const component = readFileSync(new URL("../components/terminal-capture.tsx", import.meta.url), "utf8");
    expect(component).toContain('role="region"');
    expect(component).toContain("tabIndex={0}");
    expect(component).toContain('role="img"');
    expect(component).toContain('dir="ltr"');
    expect(component).toContain('lang="en"');
    expect(component).toContain("aria-label={label}");
  });
});
