import Image from "next/image";
import { DocArticle } from "../_components/doc-article";
import { getDocsPet } from "@/lib/i18n/dictionaries";
import { buildPageMetadata } from "@/lib/page-meta";

export async function generateMetadata({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const t = getDocsPet(locale);
  return buildPageMetadata({
    path: "/docs/pet",
    locale,
    title: t.metaTitle,
    description: t.metaDescription,
  });
}

export default async function PetPage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  return (
    <>
      <Image
        unoptimized
        src="/brand/pixel-whale.webp"
        alt=""
        width={966}
        height={865}
        className="mx-auto mb-6 h-auto w-32 [image-rendering:pixelated]"
      />
      <DocArticle t={getDocsPet(locale)} locale={locale} />
    </>
  );
}
