import { RepositoryReference, referenceMetadata } from "../_components/repository-reference";

export async function generateMetadata({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  return referenceMetadata("plugin-authoring", locale);
}

export default async function ReferencePage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  return <RepositoryReference slug="plugin-authoring" locale={locale} />;
}
