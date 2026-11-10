import Link from "next/link";
import { HeroInstall } from "@/components/hero-install";
import { Icon } from "@/components/icon";
import { InstallCodeBlock } from "@/components/install-code-block";
import { WhalePose } from "@/components/whale-pose";
import { INSTALL_COPY } from "@/lib/content/install";
import { INSTALL_GUIDES } from "@/lib/install-guide.generated";
import { getHome, pickText, pickTextLocale } from "@/lib/i18n/dictionaries";
import { buildPageMetadata } from "@/lib/page-meta";

export async function generateMetadata({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  return buildPageMetadata({
    path: "/install",
    locale,
    title: pickText(INSTALL_COPY.metaTitle, locale),
    description: pickText(INSTALL_COPY.metaDescription, locale),
  });
}

/**
 * /install — the quick install plate first (the install commands the
 * home page offers), then the reviewed checked-in guide for this language.
 * Both guides retain the actual historical scope of their installation receipts.
 */
export default async function InstallPage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const home = getHome(locale);
  const copyLocale = pickTextLocale(locale);
  const { guide, lang, sourcePath } = INSTALL_GUIDES[copyLocale];
  return (
    <>
      <div className="install-head" lang={copyLocale}>
        <div className="install-head-inner">
          <WhalePose pose="run" className="install-head-pose" priority />
          <div className="install-head-text">
            <h1 className="page-title">{pickText(INSTALL_COPY.heading, locale)}</h1>
            <p className="page-lede">{pickText(INSTALL_COPY.lead, locale)}</p>
            <HeroInstall locale={locale} ariaLabel={home.heroInstallAria} copyLabel={home.copy} copiedLabel={home.copied} />
            <p className="install-head-source">
              <Icon name="check" className="icon" />
              <a href={`https://github.com/codewhale-hq/CodeWhale/blob/main/docs/${sourcePath}`} className="link">
                {pickText(INSTALL_COPY.source, locale)}
              </a>
            </p>
             <p className="install-head-note">{pickText(INSTALL_COPY.translationNotice, locale)}</p>
          </div>
        </div>
      </div>
      <section className="install-guide" lang={copyLocale}>
        <h2 className="section-title">{pickText(INSTALL_COPY.verify, locale)}</h2>
        <p>{pickText(INSTALL_COPY.verifyBody, locale)}</p>
        <InstallCodeBlock cmd="codewhale --version" copyLabel={home.copy} copiedLabel={home.copied} copyLocale={locale} />
        <div className="actions">
          <Link href={`/${locale}/docs/guide`} className="btn btn-primary">{pickText(INSTALL_COPY.firstTask, locale)}<Icon name="arrow-right" className="icon icon-flip" /></Link>
          <Link href={`/${locale}/docs/troubleshooting`} className="section-link">{pickText(INSTALL_COPY.troubleshooting, locale)}</Link>
        </div>
        <p>{pickText(INSTALL_COPY.firstTaskBody, locale)}</p>
      </section>
      <details className="install-guide" lang={lang}>
        <summary className="section-title">{pickText(INSTALL_COPY.fullGuide, locale)}</summary>
        <article>
        {guide.chunks.map((chunk, index) => chunk.kind === "code" ? (
          <InstallCodeBlock key={index} cmd={chunk.text} copyLabel={home.copy} copiedLabel={home.copied} copyLocale={locale} />
        ) : (
          // HTML is generated at build time with strict raw-HTML and URL guards.
          <div key={index} dangerouslySetInnerHTML={{ __html: chunk.text }} />
        ))}
        <p lang={copyLocale}>
          <Link href={`/${locale}/docs/configuration`} className="section-link">
            {pickText(INSTALL_COPY.configuration, locale)}
            <Icon name="arrow-right" className="icon icon-flip" />
          </Link>
        </p>
        </article>
      </details>
    </>
  );
}
