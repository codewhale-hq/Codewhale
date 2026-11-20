import Link from "next/link";
import { DOCS_PREVIEW_COPY as COPY, DOCS_PREVIEW_NAV } from "@/lib/content/docs-preview";
import { pickText } from "@/lib/i18n/dictionaries";
import { buildPageMetadata } from "@/lib/page-meta";

export async function generateMetadata({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  return buildPageMetadata({ path: "/docs-preview", locale, title: `${pickText(COPY.title, locale)} · Codewhale`, description: pickText(COPY.lead, locale), robots: { index: false, follow: false } });
}

export default async function DocsPreviewHub({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  return (
    <main className="dp-main dp-hub">
      <header className="dp-head">
        <h1>{pickText(COPY.title, locale)}</h1>
        <p className="dp-lede">{pickText(COPY.lead, locale)}</p>
      </header>
      {DOCS_PREVIEW_NAV.map((group) => (
        <section key={group.group.en} aria-labelledby={`dp-group-${group.pages[0].slug}`}>
          <h2 id={`dp-group-${group.pages[0].slug}`}>{pickText(group.group, locale)}</h2>
          <ul className="dp-cards">
            {group.pages.map((page) => (
              <li key={page.slug}>
                <Link href={`/${locale}/docs-preview/${page.slug}`}>
                  <span className="dp-card-title">{pickText(page.label, locale)}</span>
                  <span className="dp-card-body">{pickText(page.description, locale)}</span>
                </Link>
              </li>
            ))}
          </ul>
        </section>
      ))}
    </main>
  );
}
