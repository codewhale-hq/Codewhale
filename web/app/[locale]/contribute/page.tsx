import Link from "next/link";
import { Icon } from "@/components/icon";
import { PageHeader, Section } from "@/components/page-header";
import { getContribute, pickText } from "@/lib/i18n/dictionaries";
import { buildPageMetadata } from "@/lib/page-meta";
import { getMerchCopy } from "@/lib/content/contributor-merch";
import { CONTRIBUTION_PATHS, CONTRIBUTION_STEPS, CONTRIBUTION_REVIEW_NOTES } from "@/lib/content/contributing";

export async function generateMetadata({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const t = getContribute(locale);
  return buildPageMetadata({
    path: "/contribute",
    locale,
    title: t.metaTitle,
    description: t.metaDescription,
  });
}

export default async function ContributePage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const t = getContribute(locale);
  const merch = getMerchCopy(locale);

  return (
    <>
      <PageHeader
        kicker={t.kicker}
        title={t.title}
        lede={t.lede}
        pose="write"
        actions={
          <>
            <Link href="https://github.com/codewhale-hq/CodeWhale/issues/new/choose" className="btn btn-primary btn-lg">
              {t.fileIssue}
            </Link>
            <Link href="https://github.com/codewhale-hq/CodeWhale/pulls" className="btn btn-secondary btn-lg">
              {t.browsePulls}
            </Link>
            <Link href="https://github.com/codewhale-hq/CodeWhale/blob/main/CONTRIBUTING.md" className="btn btn-ghost btn-lg">
              {t.fullGuide}
              <Icon name="external" className="icon" />
            </Link>
          </>
        }
      />

      <div className="page-body">
        <Section id="contribute-paths" title={t.pathsTitle}>
          <div className="grid-2">
            {CONTRIBUTION_PATHS.map((path) => (
              <Link key={path.href} href={path.href} className="tile">
                <h3>{pickText(path.title, locale)}</h3>
                <p>{pickText(path.body, locale)}</p>
                <span className="section-link">
                  {pickText(path.label, locale)}
                  <Icon name="arrow-right" className="icon icon-flip" />
                </span>
              </Link>
            ))}
          </div>
        </Section>

        <Section id="contribute-workflow" title={t.workflowTitle}>
          <ol className="steps">
            {CONTRIBUTION_STEPS.map((step, index) => (
              <li key={step.id}>
                <span className="gs-step-index" aria-hidden="true">{index + 1}</span>
                <h3>{pickText(step.title, locale)}</h3>
                <p>{pickText(step.body, locale)}</p>
                <Link href={step.cta.href} className="section-link">
                  {pickText(step.cta.label, locale)}
                  <Icon name="arrow-right" className="icon icon-flip" />
                </Link>
              </li>
            ))}
          </ol>
        </Section>

        <Section
          id="contribute-review"
          title={t.reviewTitle}
          scope={t.reviewScope}
        >
          <ul className="group-card" role="list">
            {CONTRIBUTION_REVIEW_NOTES.map((note) => (
              <li key={note.en} className="group-row check-row">
                <Icon name="check" className="icon" />
                <span>{pickText(note, locale)}</span>
              </li>
            ))}
          </ul>
        </Section>

        <Section
          id="contribute-dev"
          title={t.devTitle}
          scope={t.devScope}
        >
          <pre tabIndex={0} className="code-block">
{`git clone https://github.com/YOUR_USERNAME/CodeWhale.git
cd CodeWhale
git checkout -b fix/your-change

cargo build --locked -p codewhale-cli
cargo test -p <owning-crate> --locked
cargo fmt --all -- --check`}
          </pre>
        </Section>
        <Section id="contributor-merch" title={merch.title} scope={merch.lede}>
          <Link href={`/${locale}/merch`} className="btn btn-secondary">{merch.cta}</Link>
        </Section>
      </div>
    </>
  );
}
