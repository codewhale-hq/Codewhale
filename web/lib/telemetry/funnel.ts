import { DOC_TOPICS } from "../docs-map";
import { locales } from "../i18n/config";
import type { FunnelCounter } from "./product-usage";

const languages = new Set(locales.map((locale) => locale.toLowerCase()));
const documentationRoots = new Set(DOC_TOPICS.filter((topic) => topic.hasPage && topic.sitePath).map((topic) => `/${topic.sitePath}`));

export function funnelPath(pathname: string): { view: FunnelCounter | null; documentation: boolean } {
  const parts = pathname.split("/").filter(Boolean);
  if (languages.has(parts[0]?.toLowerCase())) parts.shift();
  const path = `/${parts.join("/")}`;
  const views: Record<string, FunnelCounter> = {
    "/": "home_view", "/install": "install_view", "/docs/guide": "quickstart_view", "/contribute": "contribute_view",
  };
  return {
    view: views[path] ?? null,
    documentation: path === "/docs" || path.startsWith("/docs/") || documentationRoots.has(path),
  };
}

export function funnelExit(href: string): FunnelCounter | null {
  let url: URL;
  try { url = new URL(href, "https://codewhale.net"); } catch { return null; }
  if (url.hostname !== "github.com" || url.protocol !== "https:") return null;
  const path = url.pathname.toLowerCase().replace(/\/$/, "");
  if (path === "/codewhale-hq/codewhale/issues/new" || path === "/codewhale-hq/codewhale/issues/new/choose") return "issue_exit";
  if (path === "/codewhale-hq/codewhale/blob/main/contributing.md") return "guide_exit";
  if (path === "/codewhale-hq/codewhale/issues" && /label:(?:"help wanted"|help wanted)(?:\s|$)/.test(url.searchParams.get("q") ?? "")) return "help_wanted_exit";
  return null;
}
