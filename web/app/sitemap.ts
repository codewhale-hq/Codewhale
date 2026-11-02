import type { MetadataRoute } from "next";
import { contentLocalesForPath } from "@/lib/i18n/content-locales";
import { SITE_URL } from "@/lib/page-meta";
import { readCatalogue } from "@/lib/ratatui/catalogue";

// Public, indexable routes (locale-prefixed). /admin and /api are
// intentionally excluded; see app/robots.ts.
const PATHS = ["", "/product", "/features", "/install", "/constitution", "/models", "/plugins", "/runtime", "/docs", "/docs/auth", "/docs/configuration", "/docs/guide", "/docs/hooks", "/docs/mcp", "/docs/modes", "/docs/review", "/docs/fleet", "/docs/runtime-api", "/docs/sandbox", "/docs/subagents", "/docs/tools", "/docs/troubleshooting", "/docs/trust", "/docs/vocabulary", "/docs/web", "/docs/work", "/docs/skills", "/docs/claude-code", "/docs/pet", "/faq", "/roadmap", "/enterprise", "/feed", "/digest", "/changelog", "/contribute", "/community", "/signin", "/legal/terms", "/legal/privacy"];

export default function sitemap(): MetadataRoute.Sitemap {
  const catalogue = readCatalogue();
  const componentPaths = catalogue.entries.map((entry) => `/ratatui/${encodeURIComponent(entry.name)}`);
  return [...PATHS, "/computer-use", "/ratatui", ...componentPaths].flatMap((path) =>
    contentLocalesForPath(path || "/").map((locale) => ({
      url: `${SITE_URL}/${locale}${path}`,
      alternates: {
        languages: Object.fromEntries(
          contentLocalesForPath(path || "/").map((l) => [
            l,
            `${SITE_URL}/${l}${path}`,
          ]),
        ),
      },
    })),
  );
}
