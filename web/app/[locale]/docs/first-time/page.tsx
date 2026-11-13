import { DocArticle } from "../_components/doc-article";
import { getDocsFirstTime } from "@/lib/i18n/dictionaries";
import { buildPageMetadata } from "@/lib/page-meta";

export async function generateMetadata({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const t = getDocsFirstTime(locale);
  return buildPageMetadata({
    path: "/docs/first-time",
    locale,
    title: t.metaTitle,
    description: t.metaDescription,
  });
}

export default async function FirstTimePage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  return <DocArticle t={getDocsFirstTime(locale)} locale={locale} />;
}
