import { expect, it } from "vitest";
import { REFERENCE_GUIDES } from "./content/reference-guides";
import { getReferenceGuide, getReferenceSearchSections } from "./reference-guides";

it("canonical references render their source and expose only real section destinations", () => {
  for (const spec of REFERENCE_GUIDES) {
    const { guide } = getReferenceGuide(spec.slug);
    expect(guide.chunks.length).toBeGreaterThan(0);
    expect(guide.searchSections.length).toBeGreaterThan(0);
    const html = guide.chunks.filter((chunk) => chunk.kind === "html").map((chunk) => chunk.text).join("\n");
    for (const section of guide.searchSections) expect(html).toContain(`id="${section.id}"`);
  }
  const results = getReferenceSearchSections("zh");
  expect(results.every((result) => result.lang === "en")).toBe(true);
  expect(results.some((result) => result.text.includes("previous_seq"))).toBe(true);
  expect(results.some((result) => result.href.endsWith("#overview") && result.text.includes("screen-scraping"))).toBe(true);
});
