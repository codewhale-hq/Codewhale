import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { TERMINAL_SCREENSHOT } from "./media-manifest";
import { RELEASED_CAPTURE_FRAMES } from "./released-capture.generated";

describe("terminal capture", () => {
  it("ties the README image to a frame of the real released-capture set", () => {
    const frame = RELEASED_CAPTURE_FRAMES[TERMINAL_SCREENSHOT.frame];
    const text = frame.lines.map((runs) => runs.map(([t]) => t).join("")).join("\n");
    expect(text).toContain(TERMINAL_SCREENSHOT.model);
    expect(text).toContain("accept edits");
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
