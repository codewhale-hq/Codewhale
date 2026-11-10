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
    expect(text).not.toMatch(/\$\d|per month|plan|hours|GPUI|figma/i);
  });

  it("shows the four required surfaces plus the terminal and local browser", () => {
    const ids = AVAILABILITY_ROWS.filter((row) => row.pages.includes("app")).map((row) => row.id);
    expect(ids).toEqual(["terminal", "local-browser", "hosted-web", "desktop", "phone"]);
  });

  it("pairs every capture with alt text and a development-build caption", () => {
    for (const shot of APP_COPY.shots) {
      expect(shot.alt.en.length).toBeGreaterThan(40);
      expect(shot.caption.en).toContain("Development build capture");
    }
  });
});
