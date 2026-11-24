import { describe, expect, it } from "vitest";
import { APP_COPY } from "./app-page";
import { AVAILABILITY_ROWS } from "./availability";

function strings(value: unknown): string[] {
  if (typeof value === "string") return [value];
  if (Array.isArray(value)) return value.flatMap(strings);
  if (value && typeof value === "object") return Object.values(value).flatMap(strings);
  return [];
}

describe("/app copy", () => {
  const text = strings([APP_COPY, AVAILABILITY_ROWS.filter((row) => row.pages.includes("app"))]).join("\n");

  it("offers no download and never links the hosted app", () => {
    expect(text).not.toMatch(/downloads\.codewhale\.net|app\.codewhale\.net/);
    expect(text).not.toMatch(/\.dmg|\.exe|\.msi|waitlist/i);
  });

  it("names no price, plan, hours, date or UI toolkit", () => {
    expect(text).not.toMatch(/\$\d|per month|hours|GPUI|figma/i);
    expect(text).not.toMatch(/\bplans?\b|pricing/);
  });

  it("shows the four required surfaces plus the terminal and local browser", () => {
    const ids = AVAILABILITY_ROWS.filter((row) => row.pages.includes("app")).map((row) => row.id);
    expect(ids).toEqual(["terminal", "local-browser", "hosted-web", "desktop", "phone"]);
  });

  it("pairs every capture with alt text and a caption, under a scope that names the build and the model", () => {
    expect(APP_COPY.shotsScope.en).toContain("development build");
    expect(APP_COPY.shotsScope.en).toContain("DeepSeek V4.1 Flash");
    for (const shot of APP_COPY.shots) {
      expect(shot.alt.en.length).toBeGreaterThan(40);
      expect(shot.caption.en.length).toBeGreaterThan(20);
    }
  });
});
