import { Fragment } from "react";
import Link from "next/link";
import { HeroInstall } from "@/components/hero-install";
import { readAsOf } from "@/components/project-pulse";
import { InlineCode } from "@/components/route/inline-code";
import { HeroRoute, RouteMap, heroHang, type RoutePart } from "@/components/route/product-route";
import { SessionFrames, type SessionFrame } from "@/components/route/session-frames";
import { TerminalCapture } from "@/components/terminal-capture";
import { runtimeCapabilityContext } from "@/lib/capabilities";
import { captureCaption, frameNote } from "@/lib/capture-caption";
import { PROJECT_COPY } from "@/lib/content/project";
import { PRODUCT_MAP_STATE_KEY, PRODUCT_MAP_TEXT_KEYS, productMap, type ProductMapId } from "@/lib/content/product-map";
import { getFacts } from "@/lib/facts";
import { localeDirection } from "@/lib/i18n/config";
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

function leadSentence(text: string): [string, string] {
  const match = text.match(/^(.+?[.!?。！？؟।])(\s+|(?=\S))([\s\S]+)$/u);
  return match ? [match[1], match[3]] : ["", text];
}

export default async function HomePage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const d = getHome(locale);
  const chrome = getChrome(locale);
  const contribute = getContribute(locale);
  const facts = await getFacts();
  const release = facts.latestPublishedRelease;
  const rtl = localeDirection(locale) === "rtl";
  const jsonLd = buildSoftwareApplicationJsonLd(release, locale);

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
  const sessionWide = SESSION_STEPS.map((step) => RELEASED_CAPTURE_FRAMES[`cols56/${step.key}`]);
  const sessionCaption = captureCaption(sessionWide).caption;
  const session: SessionFrame[] = SESSION_STEPS.map((step, index) => ({
    key: step.key,
    name: d[step.name],
    wide: frame(`cols56/${step.key}`, d[step.summary]),
    narrow: frame(`cols48/${step.key}`, d[step.summary]),
    note: frameNote(sessionWide[index]).needsPostureNote,
  }));
  const providerNote = frameNote(RELEASED_CAPTURE_FRAMES["cols56/01-custom-form"]).needsPostureNote;

  const activity = repoActivityView();
  const merged = latestPerAccount(activity?.pulls ?? [], MERGED_ROWS);
  const accountForms = parseExclusions().confirmed ? contribute.mergedPeopleCount : contribute.mergedAccountsCount;
  const accounts = activity?.count != null ? pluralText(accountForms, activity.count, locale, chrome.dateLocale) : null;
  const accountsAsOf = readAsOf(activity?.readAt, locale);
  const contributors = new Intl.NumberFormat(chrome.dateLocale).format(facts.contributorCount);
  const [contributorsBefore, contributorsAfter = ""] = pluralText(d.contributorsCount, facts.contributorCount, locale, chrome.dateLocale).split(contributors);

  const titleAnswer = d.heroTitle.slice(d.heroTitleLead.length).trim();
  const [beliefLead, beliefRest] = leadSentence(d.belief);

  const actions = (
    <div className="home-actions">
      <Link href={`/${locale}/install`} className="btn btn-primary home-btn">
        {d.getCodewhale}
      </Link>
      <a href={REPO_URL} className="btn home-btn home-btn-secondary">
        {d.readTheCode}
      </a>
    </div>
  );
  const install = (
    <div className="home-install">
      <HeroInstall locale={locale} ariaLabel={d.heroInstallAria} copyLabel={d.copy} copiedLabel={d.copied} />
      {release ? <p className="home-version">{fill(d.installNote, { version: release.version })}</p> : null}
    </div>
  );

  return (
    <div className="home">
      <script type="application/ld+json" dangerouslySetInnerHTML={{ __html: serializeJsonLd(jsonLd) }} />

      <section className="home-sky" aria-labelledby="home-title">
        <div className="home-intro">
          <h1 id="home-title" className="home-title">
            <span className="home-title-lead">{d.heroTitleLead}</span> <span className="home-title-answer">{titleAnswer}</span>
          </h1>
          <p className="home-subhead">
            {splitToken(d.heroIntro, "brand").map((part, index) => (
              <Fragment key={index}>
                {index > 0 ? "Codewhale" : null}
                {part}
              </Fragment>
            ))}
          </p>
          {actions}
          {install}
        </div>
        <HeroRoute parts={parts} engine={d.mapHeading} label={d.sectionDirectionHeading} rtl={rtl} />
        <div className="home-hang" style={{ ["--hang" as string]: heroHang(parts, "terminal") ?? 0 }}>
          <SessionFrames frames={session} initial={SESSION_OPENS_ON} label={d.shotPreview} caption={sessionCaption ? d[sessionCaption] : null} note={d.captureFrameNoPosture} />
        </div>
      </section>

      <div className="home-sections">
        <section className="home-section home-split" aria-labelledby="home-model">
          <div className="home-text">
            <h2 id="home-model" className="home-h2 home-station">{d.sectionModelHeading}</h2>
            <p className="home-body"><InlineCode text={d.sectionModelBody} /></p>
          </div>
          <figure className="home-figure">
            <div className="home-frame home-only-wide">{frame("cols56/01-custom-form", d.frameSummaryProvider, PROVIDER_ROWS)}</div>
            <div className="home-frame home-only-narrow">{frame("cols48/01-custom-form", d.frameSummaryProvider, PROVIDER_ROWS)}</div>
            {providerNote ? <figcaption className="home-caption">{d.captureFrameNoPosture}</figcaption> : null}
          </figure>
        </section>

        <section className="home-section home-split home-split-flip" aria-labelledby="home-control">
          <div className="home-text">
            <h2 id="home-control" className="home-h2 home-station">{d.sectionControlHeading}</h2>
            <p className="home-body"><InlineCode text={d.sectionControlBody} /></p>
          </div>
          <figure className="home-figure">
            <div className="home-frame home-only-wide">{frame("cols56/06-receipt", d.frameSummaryReceipt)}</div>
            <div className="home-frame home-only-narrow">{frame("cols48/06b-receipt-tall", d.frameSummaryReceipt, RECEIPT_ROWS_NARROW)}</div>
          </figure>
        </section>

        <section className="home-section home-pair" aria-labelledby="home-agents">
          <h2 id="home-agents" className="home-h2 home-station">{d.sectionAgentsHeading}</h2>
          <p className="home-body"><InlineCode text={d.sectionAgentsBody} /></p>
        </section>
      </div>

      <section className="home-direction" aria-labelledby="home-direction">
        <div className="home-direction-head">
          <h2 id="home-direction" className="home-h2">{d.sectionDirectionHeading}</h2>
          <p className="home-body"><InlineCode text={d.sectionDirectionBody} /></p>
        </div>
        <RouteMap parts={parts} engine={d.mapHeading} label={d.sectionDirectionHeading} locale={locale} />
      </section>

      <section className="home-belief" aria-label={beliefLead || undefined}>
        <p>
          {beliefLead ? <span className="home-belief-lead">{beliefLead} </span> : null}
          {beliefRest}
        </p>
      </section>

      <div className="home-sections">
        <section className="home-section home-split" aria-labelledby="home-open">
          <div className="home-text">
            <h2 id="home-open" className="home-h2 home-station">{d.sectionOpenHeading}</h2>
            <p className="home-body"><InlineCode text={d.sectionOpenBody} /></p>
          </div>
          <div className="home-people">
            <p className="home-count">
              {contributorsBefore ? <span className="home-count-label">{contributorsBefore.trim()}</span> : null}
              <a className="home-count-number tabular" href={`${REPO_URL}/blob/main/docs/CONTRIBUTORS.md`}>{contributors}</a>
              {contributorsAfter ? <span className="home-count-label">{contributorsAfter.trim()}</span> : null}
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
          <h2 id="home-start" className="home-h2 home-station">{d.sectionStartHeading}</h2>
          <p className="home-body"><InlineCode text={d.sectionStartBody} /></p>
          {actions}
          {install}
        </div>
      </section>
    </div>
  );
}
