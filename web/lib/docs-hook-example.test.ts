import { execFileSync } from "node:child_process";
import { expect, it } from "vitest";
import { getDocsHooks } from "./i18n/dictionaries";

it("the documented shell gate emits an unconditional valid deny verdict in both languages", () => {
  for (const locale of ["en", "zh"]) {
    const section = getDocsHooks(locale).sections.find((entry) => entry.id === "gate")!;
    const script = section.blocks.find((block) => "code" in block && block.code.startsWith("#!/bin/sh"));
    expect(script && "code" in script).toBe(true);
    if (!script || !("code" in script)) throw new Error("Missing hook script");
    for (const command of ["git push --force", "git push origin main --force", "git push --force-with-lease", "echo benign"]) {
      const output = execFileSync("sh", ["-c", script.code], { encoding: "utf8", env: { NODE_ENV: "test", DEEPSEEK_TOOL_ARGS: JSON.stringify({ command }) } });
      expect(JSON.parse(output)).toEqual({ decision: "deny", reason: "Shell calls are blocked by this hook." });
    }
  }
});
