import { Fragment } from "react";
import Image from "next/image";
import Link from "next/link";
import { HeroInstall } from "@/components/hero-install";
import { Icon } from "@/components/icon";
import { Section } from "@/components/page-header";
import { Strata } from "@/components/strata";
import { NativeTerminalGallery } from "@/components/native-terminal-gallery";
import { LivingHeroWhale } from "@/components/living-hero-whale";
import { releaseView } from "@/lib/capabilities";
import { getFacts } from "@/lib/facts";
import { GitHubStar, FaceWall, MergedList, PulseStats, statsAsOf } from "@/components/project-pulse";
import { LiveStarLink } from "@/components/github-live";
import { loadProjectPulse } from "@/lib/project-pulse";
import { PROJECT_COPY } from "@/lib/content/project";
import { HOME_OVERVIEW } from "@/lib/content/home-overview";
import { PRODUCT_COPY } from "@/lib/content/product";
import { PARTNER_PROMO } from "@/lib/content/partner-promo";
import { daysLeft, isActiveAt } from "@/lib/expiry";
import { fill, getChrome, getComputerUse, getHome, pickText, splitToken } from "@/lib/i18n/dictionaries";
import { REPO_ISSUES_URL, REPO_URL } from "@/lib/i18n/links";
import { serializeJsonLd } from "@/lib/json-ld";
import { TERMINAL_SCREENSHOT } from "@/lib/media-manifest";
import { buildSoftwareApplicationJsonLd } from "@/lib/software-application-schema";
import { getReleaseUpdates } from "@/lib/release-updates";

// Resolve published releases at request time; source fetches cache for five
// minutes. `getFacts()` rejects legacy or older KV snapshots.
export const dynamic = "force-dynamic";

export default async function HomePage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const d = getHome(locale);
  const chrome = getChrome(locale);
  const facts = await getFacts();
  const updates = await getReleaseUpdates(facts.latestPublishedRelease);
  const pulse = await loadProjectPulse(5, 48);
  const released = releaseView(facts);
  const sourceVersion = facts.version ?? "unknown";
  const publishedRelease = updates.terminal.release;
  const sourceIsPublished = publishedRelease?.version === sourceVersion;
  const promoDays = daysLeft(PARTNER_PROMO.endsAt);
  const showPromo = isActiveAt(PARTNER_PROMO.endsAt) && PARTNER_PROMO.locales.includes(locale);
  const jsonLd = buildSoftwareApplicationJsonLd(publishedRelease, locale);
  const ledeParts = splitToken(d.heroIntro, "brand");
  const gateways = new Set(["OpenRouter", "Together", "OpenAI Codex"]);
  const makers = [...new Set(released.models.map((model) => model.provider).filter((name): name is string => !!name && !gateways.has(name)))]
    .sort((a, b) => a.localeCompare(b));
  const firstParty = new Set(makers);
  const newest = released.models
    .filter((model) => model.addedAt && model.provider && firstParty.has(model.provider))
    .sort((a, b) => (b.addedAt ?? "").localeCompare(a.addedAt ?? ""))
    .slice(0, 6);
  const releaseDate = publishedRelease
    ? new Intl.DateTimeFormat(chrome.dateLocale, { month: "short", day: "numeric", year: "numeric" }).format(new Date(publishedRelease.publishedAt))
    : null;

  return (
    <div className="home">
      <script
        type="application/ld+json"
        dangerouslySetInnerHTML={{ __html: serializeJsonLd(jsonLd) }}
      />

      <div className="home-intro">
        <div className="home-water" aria-hidden="true"><Strata /></div>
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
                <GitHubStar stars={pulse.stats?.stars ?? undefined} locale={locale} asOf={statsAsOf(pulse.stats, locale)} />
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
            </figure>
            <div
              className="home-proof"
              data-source-state={sourceIsPublished ? "published release" : "source candidate"}
              data-source-state-label={sourceIsPublished ? d.publishedRelease : d.sourceCandidate}
            >
              {publishedRelease ? (
                <Link href={`/${locale}/changelog`} className="section-link">
                  {fill(pickText(HOME_OVERVIEW.releaseLabel, locale), { version: `v${publishedRelease.version}` })}
                  {releaseDate ? <span className="home-proof-date"> · {releaseDate}</span> : null}
                </Link>
              ) : (
                <Link href={`/${locale}/changelog`} className="section-link">{chrome.footerChangelog}</Link>
              )}
              <LiveStarLink initial={pulse.stats?.stars ?? undefined} label={pickText(PROJECT_COPY.statStars, locale)} href={REPO_URL} numberLocale={chrome.dateLocale} asOf={statsAsOf(pulse.stats, locale)} />
              <Link href={`/${locale}/contribute`} className="section-link">{fill(d.proofContributors, { count: facts.contributorCount })}</Link>
              <span>{facts.license ?? "MIT"}</span>
              <Link href={`/${locale}/roadmap`} className="section-link">{chrome.footerRoadmap}</Link>
            </div>
          </div>
        </section>
      </div>

      <div className="home-body">
        <Section
          id="home-models"
          className="home-section"
          title={d.modelsHeading}
          scope={released.counts && publishedRelease ? fill(pickText(HOME_OVERVIEW.modelsCount, locale), { version: `v${publishedRelease.version}`, routes: released.counts.providerRoutes, models: released.counts.models }) : undefined}
          link={<Link href={`/${locale}/models`} className="section-link">{d.modelsLink}<Icon name="arrow-right" className="icon icon-flip" /></Link>}
        >
          <ul className="home-makers" role="list">
            {makers.map((maker) => <li key={maker} className="pill">{maker}</li>)}
            <li className="pill pill-quiet">{pickText(HOME_OVERVIEW.modelsAny, locale)}</li>
          </ul>
          {newest.length > 0 && (
            <div className="home-newest">
              <h3 className="home-newest-title">{pickText(HOME_OVERVIEW.modelsNewest, locale)}</h3>
              <ol className="home-newest-list" role="list">
                {newest.map((model) => (
                  <li key={model.id}>
                    <code>{model.id}</code>
                    <span>{model.provider}</span>
                  </li>
                ))}
              </ol>
            </div>
          )}
          {showPromo && (
            <aside className="home-promo-bar home-promo-inline" aria-label={PARTNER_PROMO.title}>
              <div className="home-promo-bar-inner">
                <Image src={PARTNER_PROMO.image.src} alt="" width={64} height={36} className="home-promo-bar-art" unoptimized />
                <p>
                  <strong>{PARTNER_PROMO.title}</strong>
                  <span>{pickText(PARTNER_PROMO.bar, locale)}</span>
                  <span className="home-promo-bar-days">{fill(pickText(promoDays === 1 ? PARTNER_PROMO.dayLeft : PARTNER_PROMO.daysLeft, locale), { count: promoDays })}</span>
                </p>
                <span className="home-promo-bar-tag">{pickText(PARTNER_PROMO.referralTag, locale)}</span>
                <p className="home-promo-bar-disclosure">{pickText(PARTNER_PROMO.disclosure, locale)}</p>
                <a href={PARTNER_PROMO.href} rel="sponsored noopener" className="btn btn-primary">
                  {pickText(PARTNER_PROMO.cta, locale)}
                  <Icon name="external" className="icon" />
                </a>
              </div>
            </aside>
          )}
        </Section>

        <Section
          id="home-capabilities"
          className="home-section"
          title={pickText(PRODUCT_COPY.workHeading, locale)}
          link={<Link href={`/${locale}/features`} className="section-link">{chrome.navFeatures}<Icon name="arrow-right" className="icon icon-flip" /></Link>}
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
            <div>
              <dt>{pickText(HOME_OVERVIEW.computerUseTitle, locale)} <span className="pill pill-quiet">{pickText(HOME_OVERVIEW.computerUseTag, locale)}</span></dt>
              <dd>
                <p>{pickText(HOME_OVERVIEW.computerUseBody, locale)}</p>
                <Link className="section-link" href={`/${locale}/computer-use`}>{getComputerUse(locale).setup}<Icon name="arrow-right" className="icon icon-flip" /></Link>
              </dd>
            </div>
          </dl>
        </Section>

        <Section
          id="home-plugins"
          className="home-section"
          title={pickText(HOME_OVERVIEW.pluginsTitle, locale)}
          scope={pickText(HOME_OVERVIEW.pluginsBody, locale)}
          link={<Link href={`/${locale}/plugins`} className="section-link">{pickText(HOME_OVERVIEW.pluginsLink, locale)}<Icon name="arrow-right" className="icon icon-flip" /></Link>}
        />

        <Section
          id="home-community"
          className="home-section home-community"
          label={pickText(PROJECT_COPY.liveLabel, locale)}
          title={pickText(PROJECT_COPY.liveHeading, locale)}
          scope={pickText(PROJECT_COPY.liveScope, locale)}
          link={<Link href={`/${locale}/feed`} className="section-link">{pickText(PROJECT_COPY.liveLink, locale)}<Icon name="arrow-right" className="icon icon-flip" /></Link>}
        >
          <div className="pulse">
            <div className="pulse-side">
              <PulseStats stats={pulse.stats} contributors={facts.contributorCount} locale={locale} />
            </div>
            {pulse.merged.length > 0 ? (
              <div className="pulse-main">
                <h3 className="pulse-heading"><span className="status status-live"><span className="status-dot" aria-hidden="true" />{pickText(PROJECT_COPY.mergedHeading, locale)}</span></h3>
                <MergedList items={pulse.merged} locale={locale} />
              </div>
            ) : (
              <MergedList items={pulse.merged} locale={locale} />
            )}
          </div>
          <FaceWall faces={pulse.faces} total={facts.contributorCount} locale={locale} />
          <div className="actions">
            <a href={`${REPO_ISSUES_URL}/new/choose`} className="btn btn-primary btn-lg">
              {pickText(PROJECT_COPY.loopFeedback, locale)}
              <Icon name="external" className="icon" />
            </a>
            <Link href={`/${locale}/contribute`} className="btn btn-ghost btn-lg">
              {pickText(PROJECT_COPY.loopHelp, locale)}
              <Icon name="arrow-right" className="icon icon-flip" />
            </Link>
          </div>
        </Section>
      </div>

      <section className="home-install stage sea-continues" aria-labelledby="home-install">
        <div className="home-install-inner">
          <h2 className="section-title" id="home-install">{d.startHeading}</h2>
          <div className="actions">
            <Link href={`/${locale}/install`} className="btn btn-primary btn-lg">
              {d.getCodewhale}
              <Icon name="arrow-right" className="icon icon-flip" />
            </Link>
            <Link href={`/${locale}/docs/guide`} className="btn btn-ghost btn-lg">{chrome.footerGuide}</Link>
          </div>
        </div>
      </section>
    </div>
  );
}
