import Link from "next/link";
import { notFound } from "next/navigation";
import { DocsPreviewPageActions } from "@/components/docs-preview/page-actions";
import { DocsPreviewToc } from "@/components/docs-preview/toc";
import { InstallCodeBlock } from "@/components/install-code-block";
import { DOCS_PREVIEW_COPY as COPY } from "@/lib/content/docs-preview";
import { DOCS_PREVIEW_ENTRIES, DOCS_PREVIEW_ROUTE_COUNT, docsPreviewRouteGroups, getDocsPreviewPage, isDocsPreviewSlug } from "@/lib/docs-preview";
import { locales } from "@/lib/i18n/config";
import { fill, getDocsShell, getHome, pickText, pickTextLocale } from "@/lib/i18n/dictionaries";
import { REPO_URL } from "@/lib/i18n/links";
import { buildPageMetadata } from "@/lib/page-meta";

export const dynamicParams = false;

export function generateStaticParams() {
  return locales.flatMap((locale) => DOCS_PREVIEW_ENTRIES.map((entry) => ({ locale, slug: entry.slug })));
}

export async function generateMetadata({ params }: { params: Promise<{ locale: string; slug: string }> }) {
  const { locale, slug } = await params;
  if (!isDocsPreviewSlug(slug)) return {};
  const entry = DOCS_PREVIEW_ENTRIES.find((page) => page.slug === slug)!;
  return buildPageMetadata({ path: `/docs-preview/${slug}`, locale, title: `${getDocsPreviewPage(slug, locale).title} · Codewhale`, description: pickText(entry.description, locale), robots: { index: false, follow: false } });
}

export default async function DocsPreviewPage({ params }: { params: Promise<{ locale: string; slug: string }> }) {
  const { locale, slug } = await params;
  if (!isDocsPreviewSlug(slug)) notFound();
  const page = getDocsPreviewPage(slug, locale);
  const index = DOCS_PREVIEW_ENTRIES.findIndex((entry) => entry.slug === slug);
  const entry = DOCS_PREVIEW_ENTRIES[index];
  const previous = DOCS_PREVIEW_ENTRIES[index - 1];
  const next = DOCS_PREVIEW_ENTRIES[index + 1];
  const home = getHome(locale);
  const shell = getDocsShell(locale);
  const language = pickTextLocale(locale);
  const t = (key: keyof typeof COPY) => pickText(COPY[key], locale);
  const groups = slug === "runtime-routes" ? docsPreviewRouteGroups() : [];

  return (
    <>
      <main className="dp-main">
        <p className="dp-banner">{t("banner")}</p>
        <header className="dp-head">
          <p className="dp-eyebrow">{pickText(entry.label, locale)}</p>
          <h1 lang={page.lang}>{page.title}</h1>
          <p className="dp-lede">{pickText(entry.description, locale)}</p>
          <DocsPreviewPageActions
            markdownHref={`/docs-preview/${slug}${language === "en" || page.lang === "en" ? "" : `.${language}`}.md`}
            sourceHref={`${REPO_URL}/blob/main/${page.sourcePath}`}
            labels={{ copyPage: t("copyPage"), copied: home.copied.replace(/\s*✓\s*$/u, ""), viewMarkdown: t("viewMarkdown"), source: t("source") }}
          />
          {!page.translated && <p className="dp-note">{t("english")}</p>}
        </header>

        {groups.length > 0 ? (
          <div className="install-guide dp-prose">
            <p>{fill(t("routesLead"), { count: DOCS_PREVIEW_ROUTE_COUNT, groups: groups.length })}</p>
            {groups.map((group) => (
              <section key={group.name} aria-labelledby={group.name}>
                <h2 id={group.name}>{group.name}</h2>
                <ul className="dp-routes">
                  {group.routes.flatMap((route) => route.methods.map((method) => (
                    <li key={`${method} ${route.path}`}>
                      <span className="dp-method" data-method={method}>{method}</span>
                      <code>{route.path}</code>
                    </li>
                  )))}
                </ul>
              </section>
            ))}
          </div>
        ) : (
          <div className="install-guide dp-prose" lang={page.lang}>
            {page.chunks.map((chunk, i) => chunk.kind === "code" ? (
              <div key={i} className="dp-code">
                {chunk.lang && chunk.lang !== "text" && <p className="dp-code-lang">{chunk.lang}</p>}
                <InstallCodeBlock cmd={chunk.text} html={chunk.html ?? undefined} copyLabel={home.copy} copiedLabel={home.copied} copyLocale={locale} />
              </div>
            ) : <div key={i} dangerouslySetInnerHTML={{ __html: chunk.text.replaceAll('href="/docs-preview/', `href="/${locale}/docs-preview/`) }} />)}
          </div>
        )}

        <nav className="dp-pager" aria-label={shell.nextHeading}>
          {previous ? (
            <Link href={`/${locale}/docs-preview/${previous.slug}`} rel="prev">
              <span>{t("previous")}</span>
              <strong>{pickText(previous.label, locale)}</strong>
            </Link>
          ) : <span />}
          {next && (
            <Link href={`/${locale}/docs-preview/${next.slug}`} rel="next" className="dp-pager-next">
              <span>{t("next")}</span>
              <strong>{pickText(next.label, locale)}</strong>
            </Link>
          )}
        </nav>
      </main>
      <DocsPreviewToc label={shell.onThisPage} headings={page.headings} />
    </>
  );
}
