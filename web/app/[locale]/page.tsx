import { Fragment } from "react";
import Image from "next/image";
import Link from "next/link";
import { HeroInstall } from "@/components/hero-install";
import { Icon } from "@/components/icon";
import { Section } from "@/components/page-header";
import { Strata } from "@/components/strata";
import { ReleaseUpdates } from "@/components/release-updates";
import { NativeTerminalGallery } from "@/components/native-terminal-gallery";
import { LivingHeroWhale } from "@/components/living-hero-whale";
import { getFacts } from "@/lib/facts";
import { CONTRIBUTION_PATHS, SKILL_CONTRIBUTION } from "@/lib/content/contributing";
import { PLUGINS_COPY } from "@/lib/content/plugins";
import { PRODUCT_COPY } from "@/lib/content/product";
import { PARTNER_PROMO } from "@/lib/content/partner-promo";
import { fill, getChrome, getComputerUse, getHome, pickText, splitToken } from "@/lib/i18n/dictionaries";
import { DISCORD_URL } from "@/lib/i18n/links";
import { serializeJsonLd } from "@/lib/json-ld";
import { TERMINAL_SCREENSHOT } from "@/lib/media-manifest";
import { buildSoftwareApplicationJsonLd } from "@/lib/software-application-schema";
import { getReleaseUpdates } from "@/lib/release-updates";

// Resolve published releases at request time; source fetches cache for five
// minutes. `getFacts()` rejects legacy or older KV snapshots.
export const dynamic = "force-dynamic";

/**
 * The whale-road homepage: the promise, capabilities and reading
 * sections sit on paper. The live v2 whale (the desktop app's own Director)
 * rests above the captured terminal with drawn water behind it,
 * and the page ends in the sea with the install action, running into the
 * footer.
 *
 * One memorable thing moves: the whale. It breathes, glances toward the
 * pointer, and performs its existing poses. Everything else is still.
 * Reduced motion shows its poster pose.
 *
 * Public copy resolves through `getHome(locale)`; product names and package
 * channels keep their canonical spelling.
 */
export default async function HomePage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const d = getHome(locale);
  const facts = await getFacts();
  const updates = await getReleaseUpdates(facts.latestPublishedRelease);
  const sourceVersion = facts.version ?? "unknown";
  const publishedRelease = updates.terminal.release;
  const sourceIsPublished = publishedRelease?.version === sourceVersion;
  const showPromo = Date.now() < Date.parse(PARTNER_PROMO.endsAt);

  // The install URL resolves published artifacts, so its structured version
  // must come from the published-release receipt rather than source-candidate
  // facts. When no release is known, the schema omits softwareVersion.
  const jsonLd = buildSoftwareApplicationJsonLd(publishedRelease);

  // The lede typesets the brand in its own span. Splitting on the {brand}
  // token keeps the sentence a single translated unit — no concatenation of
  // fragments around a variable, and a locale may place the brand anywhere.
  const ledeParts = splitToken(d.heroIntro, "brand");

  return (
    <div className="home">
      <script
        type="application/ld+json"
        dangerouslySetInnerHTML={{ __html: serializeJsonLd(jsonLd) }}
      />

      <div className="home-intro">
        <div className="home-water" aria-hidden="true"><Strata /></div>
        {/* PAPER — the promise, one primary action, the install command, and
            the whale resting on the terminal below. */}
        <section className="home-hero" aria-labelledby="home-title">
          <div className="home-hero-inner">
            <div className="home-hero-copy">
              <h1 id="home-title" className="home-title">{d.heroTitle}</h1>
              <p className="home-lede">
                {ledeParts.map((part, index) => (
                  <Fragment key={index}>
                    {index > 0 && <strong>Codewhale</strong>}
                    {part}
                  </Fragment>
                ))}
              </p>
              <HeroInstall locale={locale} ariaLabel={d.heroInstallAria} copyLabel={d.copy} copiedLabel={d.copied} />
              <div className="actions">
                <Link href={`/${locale}/install`} className="btn btn-primary btn-lg">
                  {d.getCodewhale}
                </Link>
                <Link href={`/${locale}/contribute`} className="btn btn-ghost btn-lg">
                  {d.contribute}
                  <Icon name="arrow-right" className="icon icon-flip" />
                </Link>
              </div>
            </div>
            <div className="home-whale">
              <LivingHeroWhale locale={locale} />
            </div>
          </div>
        </section>

        <section className="home-terminal" aria-label={d.shotPreview}>
          <div className="home-terminal-inner">
            <figure className="home-shot figure">
              <div className="figure-frame stage" dir="ltr">
                <NativeTerminalGallery
                  locale={locale}
                  defaultFrame="home"
                  regionLabel={d.shotPreview}
                  label={fill(d.screenshotAlt, { version: TERMINAL_SCREENSHOT.version })}
                />
              </div>
              <figcaption className="figure-caption">{d.shotPreview}</figcaption>
            </figure>
            <div
              className="home-proof"
              data-source-state={sourceIsPublished ? "published release" : "source candidate"}
              data-source-state-label={sourceIsPublished ? d.publishedRelease : d.sourceCandidate}
            >
              <Link href={`/${locale}/changelog`} className="section-link">{getChrome(locale).footerChangelog}</Link>
              <span>{facts.license ?? "MIT"}</span>
            </div>
          </div>
        </section>
      </div>

      {/* PAPER — models, connections, and the getting-started path. */}
      <div className="home-body">
        <ReleaseUpdates key={`${locale}:${updates.fetchedAt}`} locale={locale} initial={updates} />
        <Section
          id="home-capabilities"
          className="home-section"
          title={pickText(PRODUCT_COPY.workHeading, locale)}
          link={<Link href={`/${locale}/features`} className="section-link">{getChrome(locale).navFeatures}</Link>}
        >
          <dl className="home-work-patterns">
            {PRODUCT_COPY.workPatterns.slice(0, 3).map((pattern) => (
              <div key={pattern.href}>
                <dt>{pickText(pattern.title, locale)}</dt>
                <dd>
                  <p>{pickText(pattern.body, locale)}</p>
                  <Link className="section-link" href={pattern.href.startsWith("/") ? `/${locale}${pattern.href}` : pattern.href}><code>{pattern.command}</code></Link>
                </dd>
              </div>
            ))}
          </dl>
        </Section>

      </div>

      <div className="home-computer-use">
        <div className="home-computer-inner">
          <Section
            id="home-computer-use"
            layout="split"
            className="home-section"
            label={pickText(PLUGINS_COPY.cuTitle, locale)}
            title={pickText(PLUGINS_COPY.cuHomeTitle, locale)}
            scope={pickText(PLUGINS_COPY.cuLabel, locale)}
            link={<Link href={`/${locale}/computer-use`} className="btn btn-secondary">{getComputerUse(locale).setup}<Icon name="arrow-right" className="icon icon-flip" /></Link>}
          >
            <dl className="ruled-list">
              {[0, 1, 6].map((index) => {
                const feature = PLUGINS_COPY.cuFeatures[index];
                return <div key={feature.title.en}><dt>{pickText(feature.title, locale)}</dt><dd>{pickText(feature.detail, locale)}</dd></div>;
              })}
            </dl>
          </Section>
        </div>
      </div>

      <div className="home-body">
        <Section
          id="home-models"
          className="home-section"
          title={d.modelsHeading}
          scope={d.modelsBody}
          link={<Link href={`/${locale}/models`} className="section-link">{d.modelsLink}<Icon name="arrow-right" className="icon icon-flip" /></Link>}
        />

        <Section
          id="home-community"
          className="home-section home-community"
          title={d.communityHeading}
          scope={fill(d.communityBody, { count: facts.contributorCount })}
          link={<Link href={`/${locale}/contribute`} className="section-link">{d.contribute}<Icon name="arrow-right" className="icon icon-flip" /></Link>}
        >
          <div className="home-contribution-grid">
            <a href={SKILL_CONTRIBUTION.href} className="home-skill">
              <h3>{pickText(SKILL_CONTRIBUTION.title, locale)}</h3>
              <p>{pickText(SKILL_CONTRIBUTION.body, locale)}</p>
              <code dir="ltr">plugin.json<br />skills/hello/SKILL.md</code>
              <Icon name="arrow-right" className="icon icon-flip" />
            </a>
            <nav aria-label={d.communityLinksAria}>
              <ul className="dir-list" role="list">
                {CONTRIBUTION_PATHS.map((path) => (
                  <li key={path.href}>
                    <a href={path.href} className="dir-row">
                      <span className="dir-mark" aria-hidden="true"><Icon name={path.icon} /></span>
                      <span className="dir-text">
                        <span className="dir-title">{pickText(path.title, locale)}</span>
                      </span>
                      <span className="dir-action" aria-hidden="true"><Icon name="external" /></span>
                    </a>
                  </li>
                ))}
                <li>
                  <a href={DISCORD_URL} className="dir-row">
                    <span className="dir-mark" aria-hidden="true"><Icon name="message" /></span>
                    <span className="dir-text"><span className="dir-title">Discord</span></span>
                    <span className="dir-action" aria-hidden="true"><Icon name="external" /></span>
                  </a>
                </li>
              </ul>
            </nav>
          </div>
        </Section>

        {showPromo && (
          <section className="home-section home-promo" aria-labelledby="home-promo-title">
            <a href={PARTNER_PROMO.href} rel="sponsored" className="home-promo-card">
              <span className="figure-frame">
                <Image
                  src={PARTNER_PROMO.image.src}
                  alt={pickText(PARTNER_PROMO.image.alt, locale)}
                  width={PARTNER_PROMO.image.width}
                  height={PARTNER_PROMO.image.height}
                  unoptimized
                />
              </span>
              <span className="home-promo-text">
                <span className="home-promo-title" id="home-promo-title">{PARTNER_PROMO.title}</span>
                <span className="home-promo-body">{pickText(PARTNER_PROMO.body, locale)}</span>
                <span className="home-promo-body">{pickText(PARTNER_PROMO.disclosure, locale)}</span>
                <span className="section-link">
                  {pickText(PARTNER_PROMO.cta, locale)}
                  <Icon name="external" />
                </span>
              </span>
            </a>
          </section>
        )}
      </div>

      {/* THE SEA AGAIN — the install action where the page ends; the water
          runs on into the footer. */}
      <section className="home-install stage sea-continues" aria-labelledby="home-install">
        <div className="home-install-inner">
          <h2 className="section-title" id="home-install">{d.startHeading}</h2>
          <Link href={`/${locale}/install`} className="btn btn-primary btn-lg">
            {d.getCodewhale}
            <Icon name="arrow-right" className="icon icon-flip" />
          </Link>
        </div>
      </section>
    </div>
  );
}
