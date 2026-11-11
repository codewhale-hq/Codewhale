import { PageHeader } from "@/components/page-header";
import { Status, type StatusTone } from "@/components/status-badge";
import { BUILD_FACTS } from "@/lib/facts";
import { capabilityViews, runtimeCapabilityContext } from "@/lib/capabilities";
import { featureDocsHref, type FeatureStatus, type SurfaceStatus } from "@/lib/features";
import { fill, getFeatures } from "@/lib/i18n/dictionaries";
import { stateLabel } from "@/lib/surface-states";
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
  const views = capabilityViews(runtimeCapabilityContext(BUILD_FACTS));
  return <>
    <PageHeader title={t.title} lede={t.description} actions={<a href="/features.json" className="btn btn-secondary">{t.exportJson}</a>} />
    <div className="page-body">
      <p className="muted">{fill(t.notice, { count: views.length, version: BUILD_FACTS.version ?? "?" })}</p>
      <div className="feature-catalogue">
        {views.map(({ feature, availability, name, summary }) => <article key={feature.id} id={feature.id} className="feature-entry">
          <div lang="en" dir="ltr">
            <h2><a href={`#${feature.id}`}>{name}</a></h2>
            <p>{summary}</p>
          </div>
          <div className="feature-states">
            <Status tone={TONES[feature.status]}>{t[feature.status]}</Status>
            {Object.entries(feature.surfaces).map(([surface, state]) => <Status key={surface} tone={TONES[state]}><bdi>{surface.toUpperCase()}</bdi>: {t[state]}</Status>)}
            {feature.flag && <code dir="ltr">{feature.flag}</code>}
            {availability === "in_source"
              ? <Status tone="idle">{stateLabel("in_source", locale)}</Status>
              : feature.since && <code dir="ltr">v{feature.since}</code>}
          </div>
          <a className="section-link" href={featureDocsHref(feature)}>{feature.docs ? t.docs : t.source}</a>
        </article>)}
      </div>
    </div>
  </>;
}
