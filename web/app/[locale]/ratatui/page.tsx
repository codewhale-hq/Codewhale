import Link from "next/link";
import { PageHeader } from "@/components/page-header";
import { RatatuiExplorer } from "@/components/ratatui/explorer";
import { RatatuiHero } from "@/components/ratatui/hero";
import { RatatuiProjectLinks } from "@/components/ratatui/project-links";
import { RatatuiQuickstart } from "@/components/ratatui/quickstart";
import { getRatatuiCopy } from "@/lib/content/ratatui";
import { buildPageMetadata } from "@/lib/page-meta";
import { readCatalogue } from "@/lib/ratatui/catalogue";
import { fallbackAttrs } from "@/lib/i18n/fallback";

const REPOSITORY = "https://github.com/codewhale-hq/codewhale-ratatui";

export const revalidate = 3600;

export async function generateMetadata({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const copy = getRatatuiCopy(locale);
  return buildPageMetadata({
    path: "/ratatui",
    locale,
    title: copy.metaTitle,
    description: copy.metaDescription,
  });
}

export default async function RatatuiPage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const copy = getRatatuiCopy(locale);
  const catalogue = await readCatalogue();

  return (
    <div className="ratatui-page" {...fallbackAttrs("/ratatui", locale)}>
      <PageHeader
        title={copy.title}
        lede={copy.intro}
        actions={
          <>
            <a href="#ratatui-install" className="btn btn-primary">{copy.install}</a>
            <Link href={REPOSITORY} className="btn btn-secondary">{copy.repository}</Link>
          </>
        }
      />
      <RatatuiHero locale={locale} copy={copy} />
      <RatatuiQuickstart copy={copy} />
      <RatatuiExplorer catalogue={catalogue} locale={locale} copy={copy} />
      <RatatuiProjectLinks revision={catalogue.source.revision} copy={copy} />
    </div>
  );
}
