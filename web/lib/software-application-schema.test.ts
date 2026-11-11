import { describe, expect, it } from "vitest";
import { getHome } from "./i18n/dictionaries";
import { ORGANIZATION_ID } from "./site-schema";
import { buildSoftwareApplicationJsonLd } from "./software-application-schema";

describe("SoftwareApplication structured data", () => {
  it("uses the published version advertised by the release-backed install URL", () => {
    const schema = buildSoftwareApplicationJsonLd({ version: "0.9.3" });

    expect(schema.downloadUrl).toBe("https://codewhale.net/en/install");
    expect(schema.softwareVersion).toBe("0.9.3");
    expect(schema.author).toEqual({ "@id": ORGANIZATION_ID });
  });

  it("omits softwareVersion when no published release receipt is available", () => {
    const schema = buildSoftwareApplicationJsonLd(null);

    expect(schema).not.toHaveProperty("softwareVersion");
    expect(schema.author).toEqual({ "@id": ORGANIZATION_ID });
  });

  it("lists only released operating systems, carries no price or free claim, and links the locale's install page", () => {
    const schema = buildSoftwareApplicationJsonLd({ version: "0.9.3" }, "zh");

    expect(schema.operatingSystem).toBe("macOS, Linux, Windows");
    expect(schema).not.toHaveProperty("isAccessibleForFree");
    expect(schema).not.toHaveProperty("offers");
    expect(schema.downloadUrl).toBe("https://codewhale.net/zh/install");
    expect(schema.description).toBe(getHome("zh").metaDescription);
  });
});
