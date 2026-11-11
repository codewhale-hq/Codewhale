import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import InstallPage from "@/app/[locale]/install/page";
import { getFacts } from "@/lib/facts";
import { claimAvailability, runtimeCapabilityContext } from "@/lib/capabilities";

async function render(locale: string): Promise<string> {
  return renderToStaticMarkup(await InstallPage({ params: Promise.resolve({ locale }) }));
}

describe("install page", () => {
  it("prints the published release version and never a hand-typed one", async () => {
    const facts = await getFacts();
    const version = facts.latestPublishedRelease?.version;
    expect(version).toBeTruthy();
    const html = await render("en");
    expect(html).toContain(`v${version}`);
    expect(html).toContain(`<code>codewhale ${version}</code>`);
  });

  it("shows the uninstall command only when the published release has it", async () => {
    const facts = await getFacts();
    const released = claimAvailability({ feature: "unix-uninstall" }, runtimeCapabilityContext(facts)) === "released";
    const html = await render("en");
    expect(html.includes("codewhale uninstall")).toBe(released);
  });

  it("states what the installer adds to the profile and never tells people to open a new terminal because it changed PATH", async () => {
    const html = await render("en");
    expect(html).toContain("# &gt;&gt;&gt; codewhale PATH &gt;&gt;&gt;");
    expect(html).toContain("--no-modify-path");
    expect(html).toContain('id="mirrors"');
    expect(html).not.toContain("Open a new terminal if the installer changed your PATH");
  });
});
