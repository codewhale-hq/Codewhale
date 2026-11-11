import { describe, expect, it } from "vitest";
import { truncateChars } from "./truncate";

describe("truncateChars", () => {
  it("leaves text at or under the limit untouched", () => {
    expect(truncateChars("short", 70)).toBe("short");
    expect(truncateChars("x".repeat(70), 70)).toBe("x".repeat(70));
  });

  it("never splits an astral character in half", () => {
    // `"…🐋".slice(0, 70)` cuts between the surrogates and emits a lone
    // U+D83D, which renders as U+FFFD next to the ellipsis.
    const title = `${"x".repeat(69)}🐋 whale support`;
    const cut = truncateChars(title, 70);
    expect(cut).toBe(`${"x".repeat(69)}🐋…`);
    // No surrogate survives that is not half of a well-formed pair.
    const unpaired = cut.replace(/[\uD800-\uDBFF][\uDC00-\uDFFF]/g, "");
    expect(/[\uD800-\uDFFF]/.test(unpaired), "lone surrogate in output").toBe(false);
    expect(title.slice(0, 70).charCodeAt(69)).toBe(0xd83d);
  });

  it("counts code points, not UTF-16 code units", () => {
    // Forty whales are forty characters and eighty code units. The old
    // `title.length > 70` test called this an 80-character title and cut it.
    const whales = "🐋".repeat(40);
    expect(whales.length).toBe(80);
    expect(truncateChars(whales, 70)).toBe(whales);
  });

  it("supports a keep budget below the limit", () => {
    expect(truncateChars("y".repeat(139), 140, 137)).toBe("y".repeat(139));
    expect(truncateChars("y".repeat(141), 140, 137)).toBe(`${"y".repeat(137)}…`);
  });
});
