import Link from "next/link";
import { Icon } from "@/components/icon";
import { PageHeader, Section } from "@/components/page-header";
import { getContribute, pickText } from "@/lib/i18n/dictionaries";
import { buildPageMetadata } from "@/lib/page-meta";
import { getMerchCopy } from "@/lib/content/contributor-merch";
import { CONTRIBUTION_PATHS, SKILL_CONTRIBUTION } from "@/lib/content/contributing";

export async function generateMetadata({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const t = getContribute(locale);
  return buildPageMetadata({
    path: "/contribute",
    locale,
    title: t.metaTitle,
    description: t.metaDescription,
  });
}

export default async function ContributePage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const t = getContribute(locale);
  const merch = getMerchCopy(locale);

  return (
    <>
      <PageHeader
        title={t.title}
        lede={t.lede}
        pose="write"
        actions={
          <>
            <Link href={SKILL_CONTRIBUTION.href} className="btn btn-primary btn-lg">
              {pickText(SKILL_CONTRIBUTION.title, locale)}
              <Icon name="external" className="icon" />
            </Link>
            <Link href="https://github.com/codewhale-hq/CodeWhale/blob/main/CONTRIBUTING.md" className="btn btn-secondary btn-lg">
              {t.fullGuide}
              <Icon name="external" className="icon" />
            </Link>
          </>
        }
      />

      <div className="page-body">
        <Section id="contribute-paths" title={t.pathsTitle}>
          <div className="grid-2">
            {CONTRIBUTION_PATHS.map((path) => (
              <Link key={path.href} href={path.href} className="tile">
                <h3>{pickText(path.title, locale)}</h3>
                <p>{pickText(path.body, locale)}</p>
              </Link>
            ))}
          </div>
        </Section>

        <Section id="contributor-merch" title={merch.title} scope={merch.lede}>
          <Link href={`/${locale}/merch`} className="btn btn-secondary">{merch.cta}</Link>
        </Section>
      </div>
    </>
  );
}
