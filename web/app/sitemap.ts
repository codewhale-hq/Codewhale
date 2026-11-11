import type { MetadataRoute } from "next";
import { DOC_TOPICS } from "@/lib/docs-map";
import { contentLocalesForPath } from "@/lib/i18n/content-locales";
import { SITE_URL } from "@/lib/page-meta";
import { REDIRECTED_ROUTES } from "@/lib/route-redirects";

const PATHS = ["", "/app", "/computer-use", "/docs", "/faq", "/changelog", "/contribute", "/ratatui", "/legal/terms", "/legal/privacy"];

function buildTime(): Date {
  const committed = Date.parse(process.env.NEXT_PUBLIC_CODEWHALE_SOURCE_COMMITTED_AT ?? "");
  return new Date(Number.isFinite(committed) ? committed : Date.now());
}

export default function sitemap(): MetadataRoute.Sitemap {
  const lastModified = buildTime();
  const docsPaths = DOC_TOPICS
    .filter((topic) => topic.hasPage)
    .map((topic) => topic.sitePath ? `/${topic.sitePath}` : `/docs/${topic.slug}`);
  const paths = [...new Set([...PATHS, ...docsPaths])]
    .filter((path) => !REDIRECTED_ROUTES.includes(path.split("/")[1] ?? ""));
  return paths.flatMap((path) =>
    contentLocalesForPath(path || "/").map((locale) => ({
      url: `${SITE_URL}/${locale}${path}`,
      lastModified,
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
