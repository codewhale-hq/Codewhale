import { PageHeader } from "@/components/page-header";
import { EmptyState } from "@/components/surface-state";
import {
  DIGEST_RECORD_PREFIX,
  isPublishedDigest,
  type WeeklyDigestRecord as WeeklyDigest,
} from "@/lib/community-agent";
import { getDigest } from "@/lib/i18n/dictionaries";
import { getEnv, readBoundedKvJson } from "@/lib/kv";
import { buildPageMetadata } from "@/lib/page-meta";

type DigestSection = WeeklyDigest["sections"][number];

const MAX_LISTED_KEYS = 1000;
const MAX_DIGEST_READS = 32;
const READ_BATCH = 4;
const MAX_DIGEST_BYTES = 128 * 1024;

export const revalidate = 3600; // Cache page updates hourly

export async function generateMetadata({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const t = getDigest(locale);
  return buildPageMetadata({
    path: "/digest",
    locale,
    title: t.metaTitle,
    description: t.metaDescription,
  });
}

export default async function DigestArchivePage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const t = getDigest(locale);

  const { CURATED_KV: kv } = await getEnv();
  const digests: WeeklyDigest[] = [];
  if (kv) {
    try {
      const { keys } = await kv.list({ prefix: DIGEST_RECORD_PREFIX, limit: MAX_LISTED_KEYS });
      const newest = keys.slice(0, MAX_LISTED_KEYS).map(({ name }) => name)
        .filter((name) => name.startsWith(DIGEST_RECORD_PREFIX)).sort().reverse().slice(0, MAX_DIGEST_READS);
      for (let offset = 0; offset < newest.length; offset += READ_BATCH) {
        const names = newest.slice(offset, offset + READ_BATCH);
        const records = await Promise.all(names.map((name) => readBoundedKvJson(kv, name, MAX_DIGEST_BYTES)));
        for (const [index, record] of records.entries()) {
          if (isPublishedDigest(record) && DIGEST_RECORD_PREFIX + record.weekId === names[index]) digests.push(record);
        }
      }
      digests.sort((a, b) => Date.parse(b.generatedAt) - Date.parse(a.generatedAt));
    } catch (e) {
      console.error("Error listing weekly digests", e);
    }
  }

  // A maintainer reviews a digest in one language (the admin locale), and
  // only that language is published, so each digest renders in the language
  // it was reviewed in whatever the reader's locale.

  if (digests.length === 0) {
    return (
      <>
        <PageHeader title={t.title} lede={t.lead} pose="read" />
        <div className="page-body">
          <EmptyState locale={locale} title={t.emptyTitle} body={t.emptyBody} pose="sleep" />
        </div>
      </>
    );
  }

  return (
    <>
      <PageHeader title={t.title} lede={t.lead} pose="read" />
      <div className="page-body">
        <div className="page-body-narrow digest-list">
          {digests.map((digest: WeeklyDigest) => {
            const zh = digest.approvedLang === "zh";
            return (
            <article key={digest.weekId} className="changelog-release" lang={zh ? "zh" : "en"}>
              <header className="digest-head">
                <span className="pill tabular">{digest.weekId}</span>
                <h2 className="section-title">{zh ? digest.titleZh : digest.titleEn}</h2>
                <p className="section-scope">{zh ? digest.summaryZh : digest.summaryEn}</p>
              </header>
              <div className="changelog-sections">
                {digest.sections.map((section: DigestSection, idx: number) => (
                  <section key={idx}>
                    <h3>{section.heading}</h3>
                    <ul>
                      {section.items.map((item: string, itemIdx: number) => (
                        <li key={itemIdx}>{item}</li>
                      ))}
                    </ul>
                  </section>
                ))}
              </div>
            </article>
            );
          })}
        </div>
      </div>
    </>
  );
}
