import { Fragment } from "react";
import Image from "next/image";
import Link from "next/link";
import { HeroInstall } from "@/components/hero-install";
import { Icon } from "@/components/icon";
import { readAsOf } from "@/components/project-pulse";
import { InlineCode } from "@/components/route/inline-code";
import { RouteMap, type RoutePart } from "@/components/route/product-route";
import { SessionFrames, type SessionFrame } from "@/components/route/session-frames";
import { TerminalCapture } from "@/components/terminal-capture";
import { releaseView, runtimeCapabilityContext } from "@/lib/capabilities";
import { captureCaption, frameNote } from "@/lib/capture-caption";
import { PARTNER_PROMO } from "@/lib/content/partner-promo";
import { PROJECT_COPY } from "@/lib/content/project";
import { PRODUCT_MAP_STATE_KEY, PRODUCT_MAP_TEXT_KEYS, productMap, type ProductMapId } from "@/lib/content/product-map";
import { getFacts } from "@/lib/facts";
import { fill, getChrome, getContribute, getHome, pickText, splitToken } from "@/lib/i18n/dictionaries";
import type { HomeDict } from "@/lib/i18n/dictionaries/types";
import { REPO_URL } from "@/lib/i18n/links";
import { pluralText } from "@/lib/i18n/plural";
import { serializeJsonLd } from "@/lib/json-ld";
import { RELEASED_CAPTURE_FRAMES, type ReleasedCaptureFrameId } from "@/lib/released-capture.generated";
import { parseExclusions, repoActivityView, type ActivityPull } from "@/lib/repo-activity";
import { buildSoftwareApplicationJsonLd } from "@/lib/software-application-schema";

export const revalidate = 300;

const PART_DOC: Partial<Record<ProductMapId, string>> = {
  terminal: "/install",
  localBrowser: "/docs/web",
  manyAgents: "/docs/fleet",
};

const SESSION_STEPS = [
  { key: "02-task-prompt", name: "frameLabelPrompt", summary: "frameSummaryPrompt" },
  { key: "03-task-diff", name: "frameLabelEdit", summary: "frameSummaryEdit" },
  { key: "04-task-approval", name: "frameLabelApproval", summary: "frameSummaryApproval" },
  { key: "05-task-result", name: "frameLabelResult", summary: "frameSummaryResult" },
] as const satisfies readonly { key: string; name: keyof HomeDict; summary: keyof HomeDict }[];
const SESSION_OPENS_ON = 2;
const PROVIDER_ROWS = [9, 21] as const;
const RECEIPT_ROWS_NARROW = [18, 60] as const;
const MERGED_ROWS = 8;

const avatar = (url: string, size: number) => `${url}${url.includes("?") ? "&" : "?"}s=${size}`;

function latestPerAccount(pulls: readonly ActivityPull[], limit: number): ActivityPull[] {
  const seen = new Set<string>();
  const rows: ActivityPull[] = [];
  for (const pull of pulls) {
    if (seen.has(pull.login)) continue;
    seen.add(pull.login);
    rows.push(pull);
    if (rows.length === limit) break;
  }
  return rows;
}

function splitCount(text: string, count: string): [string, string] {
  const at = text.indexOf(count);
  return at < 0 ? [text, ""] : [text.slice(0, at), text.slice(at + count.length)];
}

interface Fact {
  key: string;
  before: string;
  number: string | null;
  after: string;
  href: string;
}

export default async function HomePage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const d = getHome(locale);
  const chrome = getChrome(locale);
  const contribute = getContribute(locale);
  const facts = await getFacts();
  const release = facts.latestPublishedRelease;
  const jsonLd = buildSoftwareApplicationJsonLd(release, locale);
  const numbers = new Intl.NumberFormat(chrome.dateLocale);
  const promoLeft = Date.parse(PARTNER_PROMO.endsAt) - Date.now();
  const promoDays = Math.max(1, Math.ceil(promoLeft / 86_400_000));
  const showPromo = promoLeft > 0 && PARTNER_PROMO.locales.includes(locale);

  const parts: RoutePart[] = productMap({ capabilities: runtimeCapabilityContext(facts) }).map((row) => {
    const keys = PRODUCT_MAP_TEXT_KEYS[row.id];
    return {
      id: row.id,
      state: row.state,
      name: d[keys.name],
      stateWord: d[PRODUCT_MAP_STATE_KEY[row.state]],
      sentence: row.sentenceHolds ? d[keys.sentence] : null,
      href: row.state === "released" ? (PART_DOC[row.id] ?? null) : null,
    };
  });

  const frame = (id: ReleasedCaptureFrameId, summary: string, rows?: readonly [number, number]) => (
    <TerminalCapture data={RELEASED_CAPTURE_FRAMES[id]} label={summary} regionLabel={d.shotPreview} rows={rows} />
  );
  const sessionWide = SESSION_STEPS.map((step) => RELEASED_CAPTURE_FRAMES[`cols100/${step.key}`]);
  const sessionCaption = captureCaption(sessionWide).caption;
  const session: SessionFrame[] = SESSION_STEPS.map((step, index) => ({
    key: step.key,
    name: d[step.name],
    wide: frame(`cols100/${step.key}`, d[step.summary]),
    narrow: frame(`cols48/${step.key}`, d[step.summary]),
    note: frameNote(sessionWide[index]).needsPostureNote,
  }));
  const providerNote = frameNote(RELEASED_CAPTURE_FRAMES["cols56/01-custom-form"]).needsPostureNote;

  const released = releaseView(facts);
  const counted = (key: string, forms: HomeDict["factModels"], n: number, href: string): Fact => {
    const number = numbers.format(n);
    const [before, after] = splitCount(pluralText(forms, n, locale, chrome.dateLocale), number);
    return { key, before, number, after, href };
  };
  const plain = (key: string, text: string, href: string): Fact => ({ key, before: text, number: null, after: "", href });
  const manyAgents = parts.find((part) => part.id === "manyAgents");
  const factList: Fact[] = [
    plain("rust", d.factRust, REPO_URL),
    ...(facts.license ? [plain("license", fill(d.factLicense, { license: facts.license }), `${REPO_URL}/blob/main/LICENSE`)] : []),
    ...(released.counts
      ? [
          counted("providers", d.factProviders, released.counts.providerRoutes, `/${locale}/models`),
          counted("models", d.factModels, released.counts.models, `/${locale}/models`),
        ]
      : []),
    ...(manyAgents?.state === "released" ? [plain("agents", manyAgents.name, `/${locale}${PART_DOC.manyAgents}`)] : []),
  ];

  const activity = repoActivityView();
  const merged = latestPerAccount(activity?.pulls ?? [], MERGED_ROWS);
  const accountForms = parseExclusions().confirmed ? contribute.mergedPeopleCount : contribute.mergedAccountsCount;
  const accounts = activity?.count != null ? pluralText(accountForms, activity.count, locale, chrome.dateLocale) : null;
  const accountsAsOf = readAsOf(activity?.readAt, locale);
  const contributors = numbers.format(facts.contributorCount);
  const [contributorsBefore, contributorsAfter] = splitCount(pluralText(d.contributorsCount, facts.contributorCount, locale, chrome.dateLocale), contributors);

  const titleAnswer = d.heroTitle.slice(d.heroTitleLead.length).trim();
  const brand = (text: string) =>
    splitToken(text, "brand").map((part, index) => (
      <Fragment key={index}>
        {index > 0 ? <bdi>Codewhale</bdi> : null}
        {part}
      </Fragment>
    ));

  const install = (
    <div className="home-install">
      <HeroInstall locale={locale} ariaLabel={d.heroInstallAria} copyLabel={d.copy} copiedLabel={d.copied} />
      <p className="home-version">
        {release ? <span>{fill(d.installNote, { version: release.version })}</span> : null}
        <a href={REPO_URL} className="home-version-link">
          {d.readTheCode}
        </a>
      </p>
    </div>
  );
  const factRow =
    factList.length > 0 ? (
      <ul className="home-facts" role="list" aria-label={d.factsLabel}>
        {factList.map((fact) => {
          const content = (
            <>
              {fact.before}
              {fact.number ? <strong className="tabular">{fact.number}</strong> : null}
              {fact.after}
            </>
          );
          return (
            <li key={fact.key}>
              {fact.href.startsWith("/") ? <Link href={fact.href}>{content}</Link> : <a href={fact.href}>{content}</a>}
            </li>
          );
        })}
      </ul>
    ) : null;
  const sessionShot = (
    <SessionFrames frames={session} initial={SESSION_OPENS_ON} label={d.shotPreview} caption={sessionCaption ? d[sessionCaption] : null} note={d.captureFrameNoPosture} />
  );

  return (
    <div className="home">
      <script type="application/ld+json" dangerouslySetInnerHTML={{ __html: serializeJsonLd(jsonLd) }} />

      <section className="home-hero" data-hero="center" aria-labelledby="home-title">
        <div className="home-hero-text">
          <h1 id="home-title" className="home-title">
            <span className="home-title-lead">{d.heroTitleLead}</span> <span className="home-title-answer">{titleAnswer}</span>
          </h1>
          <p className="home-subhead">{brand(d.heroIntro)}</p>
          {install}
        </div>
        <div className="home-hero-shot">{sessionShot}</div>
        {factRow}
      </section>

      <div className="home-sections">
        <section className="home-section home-split" aria-labelledby="home-model">
          <div className="home-text">
            <h2 id="home-model" className="home-h2">{d.sectionModelHeading}</h2>
            <p className="home-body"><InlineCode text={d.sectionModelBody} /></p>
          </div>
          <figure className="home-figure">
            <div className="home-frame home-only-wide">{frame("cols56/01-custom-form", d.frameSummaryProvider, PROVIDER_ROWS)}</div>
            <div className="home-frame home-only-narrow">{frame("cols48/01-custom-form", d.frameSummaryProvider, PROVIDER_ROWS)}</div>
            {providerNote ? <figcaption className="home-caption">{d.captureFrameNoPosture}</figcaption> : null}
          </figure>
        </section>

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

        <section className="home-section home-split home-split-flip" aria-labelledby="home-control">
          <div className="home-text">
            <h2 id="home-control" className="home-h2">{d.sectionControlHeading}</h2>
            <p className="home-body"><InlineCode text={d.sectionControlBody} /></p>
          </div>
          <figure className="home-figure">
            <div className="home-frame home-only-wide">{frame("cols56/06-receipt", d.frameSummaryReceipt)}</div>
            <div className="home-frame home-only-narrow">{frame("cols48/06b-receipt-tall", d.frameSummaryReceipt, RECEIPT_ROWS_NARROW)}</div>
          </figure>
        </section>
      </div>

      <section className="home-direction" aria-labelledby="home-direction">
        <div className="home-direction-head">
          <h2 id="home-direction" className="home-h3">{d.sectionDirectionHeading}</h2>
          <p className="home-body"><InlineCode text={d.sectionDirectionBody} /></p>
        </div>
        <RouteMap parts={parts} engine={d.mapHeading} label={d.sectionDirectionHeading} locale={locale} />
      </section>

      <div className="home-sections">
        <section className="home-section home-split" aria-labelledby="home-open">
          <div className="home-text">
            <h2 id="home-open" className="home-h2">{d.sectionOpenHeading}</h2>
            <p className="home-body"><InlineCode text={d.sectionOpenBody} /></p>
          </div>
          <div className="home-people">
            <p className="home-count">
              {contributorsBefore.trim() ? <span className="home-count-label">{contributorsBefore.trim()}</span> : null}
              <a className="home-count-number tabular" href={`${REPO_URL}/blob/main/docs/CONTRIBUTORS.md`}>{contributors}</a>
              {contributorsAfter.trim() ? <span className="home-count-label">{contributorsAfter.trim()}</span> : null}
            </p>
            {accounts ? (
              <p className="home-accounts tabular" title={accountsAsOf}>
                {accounts}
                {accountsAsOf ? <span className="sr-only"> {accountsAsOf}</span> : null}
              </p>
            ) : null}
            {merged.length > 0 ? (
              <ol className="home-merged" role="list" aria-label={pickText(PROJECT_COPY.mergedHeading, locale)}>
                {merged.map((pull) => (
                  <li key={pull.number}>
                    {/* eslint-disable-next-line @next/next/no-img-element */}
                    <img src={avatar(pull.avatar, 48)} alt="" width={24} height={24} loading="lazy" decoding="async" />
                    <span className="home-merged-login" dir="ltr">{pull.login}</span>
                    <a className="home-merged-title" href={pull.url} dir="ltr" lang="en">{pull.title}</a>
                  </li>
                ))}
              </ol>
            ) : null}
          </div>
        </section>
      </div>

      <section className="home-start" aria-labelledby="home-start">
        <div className="home-start-inner">
          <div className="home-text">
            <h2 id="home-start" className="home-h2">{d.sectionStartHeading}</h2>
            <p className="home-body"><InlineCode text={d.sectionStartBody} /></p>
          </div>
          {install}
        </div>
      </section>
    </div>
  );
}
