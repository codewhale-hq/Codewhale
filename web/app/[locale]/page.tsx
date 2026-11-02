import { Fragment } from "react";
import Image from "next/image";
import Link from "next/link";
import { GettingStartedSteps } from "@/components/getting-started-steps";
import { HeroInstall } from "@/components/hero-install";
import { Icon } from "@/components/icon";
import { InstallCodeBlock } from "@/components/install-code-block";
import { Section } from "@/components/page-header";
import { Status, type StatusTone } from "@/components/status-badge";
import { TerminalCapture } from "@/components/terminal-capture";
import { WhaleLive } from "@/components/whale-live";
import { getFacts } from "@/lib/facts";
import { compareVersions } from "@/lib/features";
import { GETTING_STARTED_STEPS } from "@/lib/content/getting-started";
import { PARTNER_PROMO } from "@/lib/content/partner-promo";
import { RELEASE_HIGHLIGHTS } from "@/lib/content/release-highlights";
import { fill, getChrome, getHome, pickText, splitToken } from "@/lib/i18n/dictionaries";
import {
  DISCORD_URL,
  REPO_ISSUES_URL,
  REPO_RELEASES_URL,
  REPO_URL,
} from "@/lib/i18n/links";
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
              <Link href={`/${locale}/product`} className="btn btn-ghost btn-lg">
                {d.exploreProduct}
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
          <TerminalCapture
            frame="home"
            regionLabel={d.shotPreview}
            label={fill(d.screenshotAlt, { version: TERMINAL_SCREENSHOT.version })}
          />
        </div>
        <figcaption className="figure-caption">
          {TERMINAL_SCREENSHOT.version === publishedRelease?.version
            ? `${d.shotPreview} · v${TERMINAL_SCREENSHOT.version}`
            : `${d.shotPreview} · ${d.sourceCandidate} v${TERMINAL_SCREENSHOT.version}`}
        </figcaption>
      </figure>

      <div
        className="home-proof"
        data-source-state={sourceIsPublished ? "published release" : "source candidate"}
        data-source-state-label={sourceIsPublished ? d.publishedRelease : d.sourceCandidate}
      >
        <Status tone={publishedRelease ? "ready" : "idle"}>
          {publishedRelease
            ? fill(d.latestRelease, { tag: publishedRelease.tag })
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
          title={d.gainHeading}
          scope={d.gainLede}
          link={<Link href={`/${locale}/features`} className="section-link">{getChrome(locale).navFeatures}</Link>}
        >
          <dl className="ruled-list">
            {d.gain.map(([name, description], index) => (
              <div key={name}>
                <dt>{name}</dt>
                <dd>{description} <Link className="section-link" href={`/${locale}/docs/${["tools", "fleet", "mcp"][index]}`}>{getChrome(locale).navDocs}</Link></dd>
              </div>
            ))}
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

        <Section
          id="home-surfaces"
          layout="split"
          className="home-section"
          title={d.surfacesHeading}
          link={
            <Link href={`/${locale}/runtime`} className="section-link">
              {d.runtimeLink}
              <Icon name="arrow-right" className="icon icon-flip" />
            </Link>
          }
        >
          <dl className="ruled-list">
            {d.surfaces.map(([name, description]) => (
              <div key={name}>
                <dt>{name}</dt>
                <dd>{description}</dd>
              </div>
            ))}
          </dl>
        </Section>

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

        {/* COMMUNITY */}
        <section className="home-section" aria-labelledby="home-community">
          <div className="section-split">
            <div className="section-head-text">
              <h2 className="section-title" id="home-community">{d.communityHeading}</h2>
              <p className="section-scope">{fill(d.communityBody, { count: facts.contributorCount })}</p>
            </div>
            <nav aria-label={d.communityLinksAria}>
              <ul className="dir-list" role="list">
                <li>
                  <a href={REPO_URL} className="dir-row">
                    <span className="dir-mark" aria-hidden="true"><Icon name="github" /></span>
                    <span className="dir-text"><span className="dir-title">GitHub</span></span>
                    <span className="dir-action" aria-hidden="true"><Icon name="external" /></span>
                  </a>
                </li>
                <li>
                  <a href={REPO_ISSUES_URL} className="dir-row">
                    <span className="dir-mark" aria-hidden="true"><Icon name="alert" /></span>
                    <span className="dir-text"><span className="dir-title">Issues</span></span>
                    <span className="dir-action" aria-hidden="true"><Icon name="external" /></span>
                  </a>
                </li>
                <li>
                  <a href={DISCORD_URL} className="dir-row">
                    <span className="dir-mark" aria-hidden="true"><Icon name="message" /></span>
                    <span className="dir-text"><span className="dir-title">Discord</span></span>
                    <span className="dir-action" aria-hidden="true"><Icon name="external" /></span>
                  </a>
                </li>
                <li>
                  <Link href={`/${locale}/contribute`} className="dir-row">
                    <span className="dir-mark" aria-hidden="true"><Icon name="git-pull-request" /></span>
                    <span className="dir-text"><span className="dir-title">{d.contribute}</span></span>
                    <span className="dir-action" aria-hidden="true"><Icon name="chevron-right" className="icon icon-flip" /></span>
                  </Link>
                </li>
                <li>
                  {publishedRelease ? (
                    <a href={publishedRelease.url} className="dir-row">
                      <span className="dir-mark" aria-hidden="true"><Icon name="package" /></span>
                      <span className="dir-text"><span className="dir-title">{publishedRelease.tag}</span></span>
                      <span className="dir-action" aria-hidden="true"><Icon name="external" /></span>
                    </a>
                  ) : (
                    <a href={REPO_RELEASES_URL} className="dir-row">
                      <span className="dir-mark" aria-hidden="true"><Icon name="package" /></span>
                      <span className="dir-text"><span className="dir-title">Releases</span></span>
                      <span className="dir-action" aria-hidden="true"><Icon name="external" /></span>
                    </a>
                  )}
                </li>
              </ul>
            </nav>
          </div>
        </section>
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
