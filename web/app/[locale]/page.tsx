import { Fragment } from "react";
import Image from "next/image";
import Link from "next/link";
import { GettingStartedSteps } from "@/components/getting-started-steps";
import { HeroInstall } from "@/components/hero-install";
import { Icon } from "@/components/icon";
import { InstallCodeBlock } from "@/components/install-code-block";
import { Section } from "@/components/page-header";
import { Status, type StatusTone } from "@/components/status-badge";
import { NativeTerminalGallery } from "@/components/native-terminal-gallery";
import { WhaleLive } from "@/components/whale-live";
import { getFacts } from "@/lib/facts";
import { compareVersions } from "@/lib/features";
import { CONTRIBUTION_PATHS } from "@/lib/content/contributing";
import { GETTING_STARTED_STEPS } from "@/lib/content/getting-started";
import { PLUGINS_COPY } from "@/lib/content/plugins";
import { PRODUCT_COPY } from "@/lib/content/product";
import { PARTNER_PROMO } from "@/lib/content/partner-promo";
import { RELEASE_HIGHLIGHTS } from "@/lib/content/release-highlights";
import { fill, getChrome, getHome, pickText, splitToken } from "@/lib/i18n/dictionaries";
import { DISCORD_URL } from "@/lib/i18n/links";
import { serializeJsonLd } from "@/lib/json-ld";
import { TERMINAL_SCREENSHOT } from "@/lib/media-manifest";
import { buildSoftwareApplicationJsonLd } from "@/lib/software-application-schema";

// Revalidate against source-proven runtime facts without giving up static edge
// caching. `getFacts()` rejects legacy or older KV snapshots.
export const revalidate = 300;

// Released · GUI available · in development · development build.
const AVAILABILITY_TONES: StatusTone[] = ["ready", "ready", "idle", "idle"];

/**
 * The whale-road homepage: the promise, capabilities and reading
 * sections sit on paper. The live v2 whale (the desktop app's own Director)
 * rests on the top edge of a real terminal capture just under the promise,
 * and the page ends in the sea with the install command, running into the
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
  const sourceVersion = facts.version ?? "unknown";
  const publishedRelease = facts.latestPublishedRelease;
  const sourceIsPublished = publishedRelease?.version === sourceVersion;
  const highlightsPublished = publishedRelease?.version === RELEASE_HIGHLIGHTS.version;
  const highlightsAhead = publishedRelease ? (compareVersions(RELEASE_HIGHLIGHTS.version, publishedRelease.version) ?? 0) > 0 : false;
  const showHighlights = highlightsPublished || highlightsAhead || sourceVersion === RELEASE_HIGHLIGHTS.version;
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
            <WhaleLive locale={locale} perform />
          </div>
        </div>
      </section>

      <figure className="home-terminal figure">
        <div className="figure-frame" dir="ltr">
          <NativeTerminalGallery
            locale={locale}
            defaultFrame="home"
            regionLabel={d.shotPreview}
            label={fill(d.screenshotAlt, { version: TERMINAL_SCREENSHOT.version })}
          />
        </div>
        <figcaption className="figure-caption">
          {`${d.shotPreview} · ${d.sourceCandidate} v${TERMINAL_SCREENSHOT.version}`}
        </figcaption>
      </figure>

      <div
        className="home-proof"
        data-source-state={sourceIsPublished ? "published release" : "source candidate"}
        data-source-state-label={sourceIsPublished ? d.publishedRelease : d.sourceCandidate}
      >
        <Status tone={publishedRelease ? "ready" : "idle"}>
          {publishedRelease
            ? <a href={publishedRelease.url}>{fill(d.latestRelease, { tag: publishedRelease.tag })}</a>
            : d.releaseUnavailable}
        </Status>
        <span>{`${sourceIsPublished ? d.currentSource : d.sourceCandidate} v${sourceVersion}`}</span>
        <span>{facts.license ?? "MIT"}</span>
        <span>{fill(d.proofContributors, { count: facts.contributorCount })}</span>
      </div>

      {/* PAPER — models, connections, and the getting-started path. */}
      <div className="home-body">
        <Section
          id="home-capabilities"
          layout="split"
          className="home-section"
          title={pickText(PRODUCT_COPY.workHeading, locale)}
          scope={pickText(PRODUCT_COPY.workLede, locale)}
          link={<Link href={`/${locale}/features`} className="section-link">{getChrome(locale).navFeatures}</Link>}
        >
          <dl className="ruled-list">
            {PRODUCT_COPY.workPatterns.map((pattern) => (
              <div key={pattern.href}>
                <dt>{pickText(pattern.title, locale)}</dt>
                <dd>{pickText(pattern.body, locale)} <Link className="section-link" href={pattern.href.startsWith("/") ? `/${locale}${pattern.href}` : pattern.href}><code>{pattern.command}</code></Link></dd>
              </div>
            ))}
          </dl>
        </Section>

        <Section
          id="home-computer-use"
          layout="split"
          className="home-section home-computer-use"
          title={pickText(PLUGINS_COPY.cuTitle, locale)}
          scope={pickText(PLUGINS_COPY.cuLead, locale)}
          link={<Link href={`/${locale}/computer-use`} className="section-link">{pickText(PLUGINS_COPY.cuTitle, locale)}<Icon name="arrow-right" className="icon icon-flip" /></Link>}
        >
          <dl className="ruled-list">
            {[0, 1, 6].map((index) => {
              const feature = PLUGINS_COPY.cuFeatures[index];
              return <div key={feature.title.en}><dt>{pickText(feature.title, locale)}</dt><dd>{pickText(feature.detail, locale)}</dd></div>;
            })}
          </dl>
        </Section>

        <Section
          id="home-models"
          layout="split"
          className="home-section"
          title={d.modelsHeading}
          scope={d.modelsBody}
          link={
            <Link href={`/${locale}/models`} className="section-link">
              {d.modelsLink}
              <Icon name="arrow-right" className="icon icon-flip" />
            </Link>
          }
        >
          <dl className="ruled-list">
            {d.modelsFacts.map(([kind, description]) => (
              <div key={kind}>
                <dt>{kind}</dt>
                <dd>{description}</dd>
              </div>
            ))}
          </dl>
        </Section>

        <section className="home-section home-community" aria-labelledby="home-community">
          <div className="section-split">
            <div className="section-head-text">
              <h2 className="section-title" id="home-community">{d.communityHeading}</h2>
              <p className="section-scope">{fill(d.communityBody, { count: facts.contributorCount })}</p>
              <Link href={`/${locale}/contribute`} className="section-link">{d.contribute}<Icon name="arrow-right" className="icon icon-flip" /></Link>
            </div>
            <nav aria-label={d.communityLinksAria}>
              <ul className="dir-list" role="list">
                {CONTRIBUTION_PATHS.map((path) => (
                  <li key={path.href}>
                    <a href={path.href} className="dir-row">
                      <span className="dir-mark" aria-hidden="true"><Icon name={path.icon} /></span>
                      <span className="dir-text">
                        <span className="dir-title">{pickText(path.title, locale)}</span>
                        <span className="dir-desc">{pickText(path.body, locale)}</span>
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
        </section>

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

        {/* The start is a real sequence, so its steps keep their numbers. */}
        <Section
          id="home-start"
          className="home-section product-start"
          title={d.startHeading}
          scope={d.startLede}
        >
          <GettingStartedSteps locale={locale} />
          <div className="product-start-links">
            <Link href={`/${locale}/docs/guide`} className="section-link">
              {d.startGuideLink}
              <Icon name="arrow-right" className="icon icon-flip" />
            </Link>
            <Link href={`/${locale}/docs/vocabulary`} className="section-link">
              {d.startVocabularyLink}
              <Icon name="arrow-right" className="icon icon-flip" />
            </Link>
          </div>
        </Section>

        {/* WHERE IT RUNS TODAY — each surface with a mark and a word. */}
        <Section
          id="home-availability"
          layout="split"
          className="home-section"
          title={d.availabilityHeading}
          scope={d.availabilityLede}
        >
          <dl className="ruled-list">
            {d.availability.map(([surface, status, detail, href], index) => (
              <div key={surface}>
                <dt>{href ? <a href={href} className="body-link">{surface}</a> : surface}</dt>
                <dd>
                  <Status tone={AVAILABILITY_TONES[index] ?? "idle"}>{status}</Status>
                  {detail}
                </dd>
              </div>
            ))}
          </dl>
          <p className="section-scope mt-4">{d.availabilityNote}</p>
        </Section>

        {showHighlights && (
          <Section
            id="home-release-highlights"
            layout="split"
            className="home-section"
            title={fill(pickText(highlightsPublished ? RELEASE_HIGHLIGHTS.newHeading : RELEASE_HIGHLIGHTS.comingHeading, locale), { version: RELEASE_HIGHLIGHTS.version })}
            scope={highlightsPublished ? undefined : pickText(RELEASE_HIGHLIGHTS.comingLede, locale)}
            link={<Link href={`/${locale}/changelog`} className="section-link">{pickText(RELEASE_HIGHLIGHTS.changelogLink, locale)}</Link>}
          >
            <dl className="ruled-list">
              {RELEASE_HIGHLIGHTS.items.map((item) => (
                <div key={item.title}>
                  <dt>{item.title}</dt>
                  <dd>{pickText(item.body, locale)}</dd>
                </div>
              ))}
            </dl>
          </Section>
        )}


      </div>

      {/* THE SEA AGAIN — the install command where the page ends; the water
          runs on into the footer. */}
      <section className="home-install stage sea-continues" aria-labelledby="home-install">
        <div className="home-install-inner">
          <div className="section-head-text">
            <h2 className="section-title" id="home-install">{d.installBandHeading}</h2>
            <p className="home-install-channels">
              GitHub Releases ({d.binaries}) · npm · Cargo · Docker · Windows · Android / Termux · {d.chinaMirrors}
            </p>
            <Link href={`/${locale}/install`} className="section-link">
              {d.installGuideLink}
              <Icon name="arrow-right" className="icon icon-flip" />
            </Link>
          </div>
          <InstallCodeBlock
            cmd={GETTING_STARTED_STEPS[0].commands[0]}
            copyLabel={d.copy}
            copiedLabel={d.copied}
          />
        </div>
      </section>
    </div>
  );
}
