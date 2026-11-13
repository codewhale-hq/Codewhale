import Link from "next/link";
import type { ReactNode } from "react";
import { EnglishRegion } from "@/components/fallback-region";
import { HeroInstall } from "@/components/hero-install";
import { Icon } from "@/components/icon";
import { InstallCodeBlock } from "@/components/install-code-block";
import { OnboardingCheck } from "@/components/onboarding-check";
import { WhalePose } from "@/components/whale-pose";
import { claimAvailability, runtimeCapabilityContext } from "@/lib/capabilities";
import { INSTALL_COMMANDS, INSTALL_COPY, INSTALL_PAGE_COMMANDS as C } from "@/lib/content/install";
import { splitCode } from "@/lib/content/models";
import type { LocalizedText } from "@/lib/content/vocabulary";
import { getFacts } from "@/lib/facts";
import { fill, getHome, pickText, pickTextLocale } from "@/lib/i18n/dictionaries";
import { buildPageMetadata } from "@/lib/page-meta";

export const revalidate = 300;

const REPO_BLOB = "https://github.com/codewhale-hq/CodeWhale/blob";

export async function generateMetadata({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  return buildPageMetadata({
    path: "/install",
    locale,
    title: pickText(INSTALL_COPY.metaTitle, locale),
    description: pickText(INSTALL_COPY.metaDescription, locale),
  });
}

function Rich({ text }: { text: string }) {
  return <>{splitCode(text).map((part, i) => (part.code ? <code key={i}>{part.text}</code> : part.text))}</>;
}

function Block({ id, title, children }: { id: string; title: string; children: ReactNode }) {
  return (
    <section id={id} className="install-guide" aria-labelledby={`${id}-title`}>
      <h2 className="section-title" id={`${id}-title`}>{title}</h2>
      {children}
    </section>
  );
}

function Channel({ id, title, children }: { id: string; title: string; children: ReactNode }) {
  return (
    <div id={id} className="install-channel">
      <h3>{title}</h3>
      {children}
    </div>
  );
}

export default async function InstallPage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const home = getHome(locale);
  const copyLocale = pickTextLocale(locale);
  const facts = await getFacts();
  const release = facts.latestPublishedRelease;
  const version = release?.version ?? null;
  const tag = release?.tag ?? null;
  const uninstallReleased = claimAvailability({ feature: "unix-uninstall" }, runtimeCapabilityContext(facts)) === "released";
  const t = (copy: LocalizedText) => fill(pickText(copy, locale), { version: version ?? "" });
  const p = (copy: LocalizedText) => <p><Rich text={t(copy)} /></p>;
  const code = (cmd: string) => (
    <InstallCodeBlock cmd={cmd} copyLabel={home.copy} copiedLabel={home.copied} copyLocale={locale} />
  );
  const guideFile = locale === "zh" ? "docs/zh_hans/INSTALL.md" : "docs/INSTALL.md";
  const guideRef = tag ?? "main";
  const mirrorGuide = `${REPO_BLOB}/${guideRef}/${locale === "zh" ? "docs/zh_hans/CNB_MIRROR.md" : "docs/CNB_MIRROR.md"}`;
  const mirrors = (
    <Block id="mirrors" title={t(INSTALL_COPY.mirrorsTitle)}>
      {p(INSTALL_COPY.mirrorsNpm)}
      {p(INSTALL_COPY.mirrorsCnb)}
      {code(C.cnbVerify)}
      <p><a href={mirrorGuide} className="link">{t(INSTALL_COPY.mirrorsGuide)}</a></p>
    </Block>
  );

  return (
    <>
      <div id="install-overview" className="install-head">
        <div className="install-head-inner">
          <WhalePose pose="run" className="install-head-pose" priority />
          <div className="install-head-text">
            <h1 className="page-title">{home.sectionStartHeading}</h1>
            <p className="page-lede"><Rich text={home.sectionStartBody} /></p>
            <HeroInstall locale={locale} ariaLabel={home.heroInstallAria} copyLabel={home.copy} copiedLabel={home.copied} />
            <p className="install-head-source" lang={copyLocale}>
              <Icon name="check" className="icon" />
              <a href="/install.sh" className="link">{t(INSTALL_COPY.scriptLink)}</a>
            </p>
            {version ? <p className="install-head-note" lang={copyLocale}>{t(INSTALL_COPY.releaseLine)}</p> : null}
          </div>
        </div>
      </div>
      <EnglishRegion when={copyLocale === "en" && locale !== "en"}>
        <div lang={copyLocale}>
          {locale === "zh" ? mirrors : null}

          <Block id="install-new-to-terminal" title={t(INSTALL_COPY.newTitle)}>
            {p(INSTALL_COPY.newLead)}
            <Channel id="install-new-macos" title={t(INSTALL_COPY.newMacTitle)}>
              <ol>
                <li><Rich text={t(INSTALL_COPY.newMac1)} /></li>
                <li><Rich text={t(INSTALL_COPY.newMac2)} />{code(INSTALL_COMMANDS.shell)}</li>
                <li><Rich text={t(INSTALL_COPY.newMac3)} /></li>
              </ol>
            </Channel>
            <Channel id="install-new-windows" title={t(INSTALL_COPY.newWinTitle)}>
              <ol>
                <li><Rich text={t(INSTALL_COPY.newWin1)} /></li>
                <li><Rich text={t(INSTALL_COPY.newWin2)} />{code(INSTALL_COMMANDS.windows)}</li>
                <li><Rich text={t(INSTALL_COPY.newWin3)} /></li>
              </ol>
            </Channel>
            <p>
              <Link href={`/${locale}/docs/first-time`} className="section-link">
                {t(INSTALL_COPY.newNext)}
                <Icon name="arrow-right" className="icon icon-flip" />
              </Link>
            </p>
          </Block>

          <Block id="install-path" title={t(INSTALL_COPY.pathTitle)}>
            {p(INSTALL_COPY.pathPlaces)}
            <ul>
              {[INSTALL_COPY.pathZsh, INSTALL_COPY.pathBashLinux, INSTALL_COPY.pathBashMac, INSTALL_COPY.pathOther, INSTALL_COPY.pathFish].map((row) => (
                <li key={row.en}><Rich text={t(row)} /></li>
              ))}
            </ul>
            {p(INSTALL_COPY.pathBlockIntro)}
            {code(C.pathBlock)}
            {p(INSTALL_COPY.pathBlockAfter)}
            {code(C.pathNow)}
            {p(INSTALL_COPY.pathOptOut)}
            {code(C.noModifyPath)}
            {p(INSTALL_COPY.pathManualIntro)}
            {code(C.pathManual)}
          </Block>

          <Block id="install-verify" title={t(INSTALL_COPY.verifyTitle)}>
            {p(version ? INSTALL_COPY.verifyBody : INSTALL_COPY.verifyBodyUnknown)}
            {code(C.version)}
            <OnboardingCheck kind="install" locale={locale} />
          </Block>

          <Block id="install-first-task" title={t(INSTALL_COPY.firstTitle)}>
            <ol>
              {[INSTALL_COPY.firstStep1, INSTALL_COPY.firstStep2, INSTALL_COPY.firstStep3].map((row) => (
                <li key={row.en}><Rich text={t(row)} /></li>
              ))}
            </ol>
            <p>
              <Link href={`/${locale}/docs/guide`} className="section-link">
                {t(INSTALL_COPY.quickstart)}
                <Icon name="arrow-right" className="icon icon-flip" />
              </Link>
            </p>
          </Block>

          <Block id="install-usage" title={t(INSTALL_COPY.usageTitle)}>
            {p(INSTALL_COPY.usageBody)}
            {code(C.telemetryOff)}
            {p(INSTALL_COPY.usageInstaller)}
            {code(C.installTelemetryOff)}
            <p>
              <Link href={`/${locale}/legal/privacy#usage-counting`} className="section-link">
                {t(INSTALL_COPY.usageLink)}
                <Icon name="arrow-right" className="icon icon-flip" />
              </Link>
            </p>
          </Block>

          <Block id="install-update" title={t(INSTALL_COPY.updateTitle)}>
            {p(INSTALL_COPY.updateBody)}
            {code(C.update)}
          </Block>

          <Block id="install-remove" title={t(INSTALL_COPY.removeTitle)}>
            {uninstallReleased ? (
              <>
                {p(INSTALL_COPY.removeNow)}
                {code(C.uninstall)}
                {p(INSTALL_COPY.removeFilesNow)}
              </>
            ) : (
              p(version ? INSTALL_COPY.removeFilesOnly : INSTALL_COPY.removeFilesOnlyUnknown)
            )}
            {code(C.removeFiles)}
            {p(INSTALL_COPY.removePath)}
            {p(INSTALL_COPY.removeData)}
            {code(C.removeData)}
            {version ? <p><Rich text={t(INSTALL_COPY.removeGuide)} /></p> : null}
          </Block>

          <Block id="install-troubleshooting" title={t(INSTALL_COPY.troubleTitle)}>
            <ul>
              {[INSTALL_COPY.troubleNotFound, INSTALL_COPY.troubleQueued, INSTALL_COPY.troubleLegacy].map((row) => (
                <li key={row.en}><Rich text={t(row)} /></li>
              ))}
            </ul>
            <p>
              <Link href={`/${locale}/docs/troubleshooting`} className="section-link">
                {t(INSTALL_COPY.troubleMore)}
                <Icon name="arrow-right" className="icon icon-flip" />
              </Link>
            </p>
          </Block>

          <Block id="install-channels" title={t(INSTALL_COPY.channelsTitle)}>
            {p(version ? INSTALL_COPY.channelsLead : INSTALL_COPY.channelsLeadUnknown)}
            <Channel id="install-windows" title={t(INSTALL_COPY.windowsTitle)}>
              {code(INSTALL_COMMANDS.windows)}
              {p(INSTALL_COPY.windowsVerify)}
              {p(INSTALL_COPY.windowsPackage)}
              {p(INSTALL_COPY.windowsSandbox)}
              {p(INSTALL_COPY.windowsUpdate)}
            </Channel>
            <Channel id="install-npm" title={t(INSTALL_COPY.npmTitle)}>
              {code(INSTALL_COMMANDS.npm)}
              {p(INSTALL_COPY.npmBody)}
              {p(INSTALL_COPY.npmEacces)}
              {code(C.npmPrefix)}
              {p(INSTALL_COPY.npmUpdate)}
            </Channel>
            <Channel id="install-cargo" title={t(INSTALL_COPY.cargoTitle)}>
              {code(INSTALL_COMMANDS.cargo)}
              {p(INSTALL_COPY.cargoBody)}
              {p(INSTALL_COPY.cargoUpdate)}
              {code(C.cargoUpdate)}
            </Channel>
            <Channel id="install-homebrew" title={t(INSTALL_COPY.brewTitle)}>
              {code(C.brewInstall)}
              {p(INSTALL_COPY.brewBody)}
              {p(INSTALL_COPY.brewUpdate)}
              {code(C.brewRemove)}
            </Channel>
          </Block>

          {locale === "zh" ? null : mirrors}

          <section id="install-full-guide" className="install-guide" aria-label={t(INSTALL_COPY.fullGuide)}>
            <p>
              <a href={`${REPO_BLOB}/${guideRef}/${guideFile}`} className="section-link">
                {t(INSTALL_COPY.fullGuide)}
                <Icon name="external" className="icon" />
              </a>
            </p>
          </section>
        </div>
      </EnglishRegion>
    </>
  );
}
