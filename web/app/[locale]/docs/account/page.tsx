import { DocArticle } from "../_components/doc-article";
import { getDocsAccount } from "@/lib/content/docs-account";
import { buildPageMetadata } from "@/lib/page-meta";

export async function generateMetadata({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const t = getDocsAccount(locale);
  return buildPageMetadata({
    path: "/docs/account",
    locale,
    title: t.metaTitle,
    description: t.metaDescription,
  });
}

export default async function AccountPage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  return <DocArticle t={getDocsAccount(locale)} locale={locale} />;
}
