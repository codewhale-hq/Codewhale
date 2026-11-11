import { describe, expect, it } from "vitest";
import { funnelExit, funnelPath } from "./funnel";

describe("bounded website acquisition categories", () => {
  it("recognizes all mapped documentation without collecting paths", () => {
    for (const locale of ["en", "zh", "pt-BR", "pt-br", "ar"]) {
      for (const path of ["install", "models", "plugins", "docs/constitution", "docs/runtime-reference"]) expect(funnelPath(`/${locale}/${path}`)).toMatchObject({ documentation: true });
      expect(funnelPath(`/${locale}`)).toEqual({ view: "home_view", documentation: false });
      expect(funnelPath(`/${locale}/docs/guide`)).toEqual({ view: "quickstart_view", documentation: true });
      expect(funnelPath(`/${locale}/contribute`)).toEqual({ view: "contribute_view", documentation: false });
    }
    expect(funnelPath("/en/community/docs-club")).toEqual({ view: null, documentation: false });
    expect(funnelPath("/unknown/install")).toEqual({ view: null, documentation: false });
  });
  it("returns only bounded categories for public contribution destinations", () => {
    expect(funnelExit("https://github.com/codewhale-hq/CodeWhale/issues/new?template=bug_report.yml")).toBe("issue_exit");
    expect(funnelExit("https://github.com/codewhale-hq/Codewhale/issues/new/choose")).toBe("issue_exit");
    expect(funnelExit("https://github.com/codewhale-hq/CodeWhale/issues?q=is%3Aopen+label%3A%22help+wanted%22")).toBe("help_wanted_exit");
    expect(funnelExit("https://github.com/codewhale-hq/CodeWhale/blob/main/CONTRIBUTING.md#standing-projects")).toBe("guide_exit");
    for (const href of ["https://github.com.evil.test/codewhale-hq/Codewhale/issues/new", "http://github.com/codewhale-hq/Codewhale/issues/new", "javascript:alert(1)", "https://github.com/other/private/issues/new", "/en/docs/guide", "https://github.com/codewhale-hq/Codewhale/issues/123"]) expect(funnelExit(href)).toBeNull();
  });
});
