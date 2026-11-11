import type { MetadataRoute } from "next";
import { DOC_TOPICS } from "@/lib/docs-map";
import { contentLocalesForPath } from "@/lib/i18n/content-locales";
import { SITE_URL } from "@/lib/page-meta";
import { readCatalogue } from "@/lib/ratatui/catalogue";

// Public, indexable routes (locale-prefixed). /admin and /api are
// intentionally excluded; see app/robots.ts.
const PATHS = ["", "/product", "/app", "/features", "/runtime", "/docs", "/faq", "/roadmap", "/enterprise", "/feed", "/digest", "/changelog", "/contribute", "/community", "/signin", "/legal/terms", "/legal/privacy"];

export default function sitemap(): MetadataRoute.Sitemap {
  const catalogue = readCatalogue();
  const componentPaths = catalogue.entries.map((entry) => `/ratatui/${encodeURIComponent(entry.name)}`);
  const docsPaths = DOC_TOPICS.filter((topic) => topic.hasPage).map((topic) => topic.sitePath ? `/${topic.sitePath}` : `/docs/${topic.slug}`);
  return [...new Set([...PATHS, ...docsPaths]), "/computer-use", "/ratatui", ...componentPaths].flatMap((path) =>
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
