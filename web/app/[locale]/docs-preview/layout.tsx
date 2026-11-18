import Link from "next/link";
import { DocsPreviewSearch } from "@/components/docs-preview/search-palette";
import { DocsPreviewSidebar } from "@/components/docs-preview/sidebar";
import { DOCS_PREVIEW_COPY as COPY } from "@/lib/content/docs-preview";
import { getDocsShell, pickText, pickTextLocale } from "@/lib/i18n/dictionaries";

export default async function DocsPreviewLayout({ children, params }: { children: React.ReactNode; params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const shell = getDocsShell(locale);
  const t = (key: keyof typeof COPY) => pickText(COPY[key], locale);
  return (
    <div className="dp portal-container">
      <aside className="dp-side">
        <Link href={`/${locale}/docs-preview`} className="dp-side-title">{t("title")}</Link>
        <DocsPreviewSearch
          locale={locale}
          indexHref={`/docs-preview/search.${pickTextLocale(locale)}.json`}
          labels={{ search: t("search"), placeholder: shell.searchPlaceholder, empty: t("searchEmpty"), none: t("searchNone"), failed: t("searchFailed"), close: t("close") }}
        />
        <DocsPreviewSidebar locale={locale} label={shell.sidebarAria} />
      </aside>
      {children}
    </div>
  );
}
