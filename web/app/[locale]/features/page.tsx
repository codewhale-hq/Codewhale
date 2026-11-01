import { PageHeader } from "@/components/page-header";
import { Status, type StatusTone } from "@/components/status-badge";
import { BUILD_FACTS } from "@/lib/facts";
import { FEATURE_CATALOGUE, compareVersions, featureDocsHref, type FeatureStatus, type SurfaceStatus } from "@/lib/features";
import { fill, getFeatures, getHome } from "@/lib/i18n/dictionaries";
import { buildPageMetadata } from "@/lib/page-meta";

const TONES: Record<FeatureStatus | SurfaceStatus, StatusTone> = {
  stable: "ready", flagged: "attention", experimental: "idle", partial: "attention", preview: "attention", none: "idle",
};

export async function generateMetadata({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const t = getFeatures(locale);
  return buildPageMetadata({ path: "/features", locale, title: `${t.title} · Codewhale`, description: t.description });
}

export default async function FeaturesPage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const t = getFeatures(locale);
  return <>
    <PageHeader title={t.title} lede={t.description} actions={<a href="/features.json" className="btn btn-secondary">{t.exportJson}</a>} />
    <div className="page-body">
      <p className="muted">{fill(t.notice, { count: FEATURE_CATALOGUE.features.length, version: BUILD_FACTS.version ?? "?" })}</p>
      <div className="feature-catalogue">
        {FEATURE_CATALOGUE.features.map((feature) => <article key={feature.id} id={feature.id} className="feature-entry">
          <div lang="en" dir="ltr">
            <h2><a href={`#${feature.id}`}>{feature.name}</a></h2>
            <p>{feature.summary}</p>
          </div>
          <div className="feature-states">
            <Status tone={TONES[feature.status]}>{t[feature.status]}</Status>
            {Object.entries(feature.surfaces).map(([surface, state]) => <Status key={surface} tone={TONES[state]}><bdi>{surface.toUpperCase()}</bdi>: {t[state]}</Status>)}
            {feature.flag && <code dir="ltr">{feature.flag}</code>}
            {feature.since === "unreleased" && <Status tone="idle">{getHome(locale).sourceCandidate}</Status>}
            {feature.since && feature.since !== "unreleased" && (BUILD_FACTS.latestPublishedRelease && (compareVersions(feature.since, BUILD_FACTS.latestPublishedRelease.version) ?? 0) > 0
              ? <Status tone="idle">{getHome(locale).sourceCandidate} <bdi>v{feature.since}</bdi></Status>
              : <code dir="ltr">v{feature.since}</code>)}
          </div>
          <a className="section-link" href={featureDocsHref(feature)}>{feature.docs ? t.docs : t.source}</a>
        </article>)}
      </div>
    </div>
  </>;
}
