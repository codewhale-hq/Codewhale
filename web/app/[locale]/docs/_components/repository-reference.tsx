import Link from "next/link";
import { InstallCodeBlock } from "@/components/install-code-block";
import { REFERENCE_GUIDE_COPY } from "@/lib/content/reference-guides";
import { getFacts } from "@/lib/facts";
import { fill, getHome, pickText } from "@/lib/i18n/dictionaries";
import { buildPageMetadata } from "@/lib/page-meta";
import { getReferenceGuide } from "@/lib/reference-guides";

export function referenceMetadata(slug: string, locale: string) {
  const { spec } = getReferenceGuide(slug);
  return buildPageMetadata({ path: `/docs/${slug}`, locale, title: `${pickText(spec.title, locale)} · Codewhale Docs`, description: pickText(spec.lead, locale) });
}

export async function RepositoryReference({ slug, locale }: { slug: string; locale: string }) {
  const { spec, source, sources, guide } = getReferenceGuide(slug);
  const home = getHome(locale);
  const tag = (await getFacts()).latestPublishedRelease?.tag ?? null;
  return (
    <div className="space-y-8">
      <header id="overview">
        <h1>{pickText(spec.title, locale)}</h1>
        <p className="text-ink-soft mt-3 leading-relaxed">{pickText(spec.lead, locale)}</p>
        {tag && <p className="docs-note mt-4">{fill(pickText(REFERENCE_GUIDE_COPY.scope, locale), { tag })}</p>}
        <p className="docs-note mt-3">{pickText(REFERENCE_GUIDE_COPY.language, locale)}</p>
        <Link href={`/${locale}${spec.tutorial}`} className="docs-inline-link underline inline-block mt-4">{pickText(spec.tutorialLabel, locale)}</Link>
      </header>
      <div className="install-guide" lang="en">
        {guide.chunks.map((chunk, index) => chunk.kind === "code" ? (
          <div key={index} className="my-4">
            <p className="text-xs font-mono" lang={locale}>{pickText(REFERENCE_GUIDE_COPY.code, locale)}</p>
            <InstallCodeBlock cmd={chunk.text} copyLabel={home.copy} copiedLabel={home.copied} copyLocale={locale} installIntent={false} />
          </div>
        ) : <div key={index} dangerouslySetInnerHTML={{ __html: chunk.text }} />)}
      </div>
      {tag && (
        <p>
          <a href={`https://github.com/codewhale-hq/CodeWhale/blob/${tag}/${source.path}`} className="docs-inline-link underline">{fill(pickText(REFERENCE_GUIDE_COPY.published, locale), { tag })}</a>
        </p>
      )}
      <a href={`https://github.com/codewhale-hq/CodeWhale/blob/main/${source.path}`} className="docs-inline-link underline">{pickText(REFERENCE_GUIDE_COPY.source, locale)}</a>
      <details>
        <summary>{pickText(REFERENCE_GUIDE_COPY.revision, locale)}</summary>
        {sources.map((entry) => <p key={entry.path} className="text-xs font-mono break-all mt-2">{entry.path} · SHA-256: {entry.sha256}</p>)}
      </details>
    </div>
  );
}
