import Link from "next/link";
import { Fragment, type ReactNode } from "react";
import { InstallCodeBlock } from "@/components/install-code-block";
import { docsCapabilityContext, resolveDocsPage } from "@/lib/docs-release";
import { getChrome, getDocsShell, getHome } from "@/lib/i18n/dictionaries";
import type { DocsBlock, DocsPageDict } from "@/lib/i18n/dictionaries/types";

/**
 * <DocArticle> — the one renderer for every task page under /docs.
 *
 * A page answers three questions in order: what you can do here and why
 * (title + lede), how (sections of prose, commands, tables and steps), and
 * what to do next (closing links). All copy comes from a `DocsPageDict`; the
 * page file only picks the dictionary and supplies metadata.
 *
 * Inline syntax inside prose strings is deliberately tiny:
 *   `code`          → <code class="inline">
 *   [label](/path)  → a link; a leading "/" is made locale-relative
 * Nothing else is interpreted, so dictionary text cannot inject markup.
 */

/** Links are underlined so they never rely on color alone (WCAG 1.4.1). */
const LINK = "docs-inline-link underline underline-offset-2 hover:text-indigo";

const INLINE = /(`[^`]+`|\[[^\]]+\]\([^)\s]+\))/g;

export function renderInline(text: string, locale: string): ReactNode {
  return text.split(INLINE).map((part, i) => {
    if (part.startsWith("`") && part.endsWith("`") && part.length > 1) {
      return (
        <code key={i} className="inline">
          {part.slice(1, -1)}
        </code>
      );
    }
    const link = part.match(/^\[([^\]]+)\]\(([^)\s]+)\)$/);
    if (link) {
      const [, label, href] = link;
      if (href.startsWith("/")) {
        return (
          <Link key={i} href={`/${locale}${href}`} className={LINK}>
            {label}
          </Link>
        );
      }
      return (
        <a key={i} href={href} className={LINK} target="_blank" rel="noreferrer">
          {label}
        </a>
      );
    }
    return <Fragment key={i}>{part}</Fragment>;
  });
}

function Block({
  block,
  locale,
  body,
  noteLabel,
}: {
  block: DocsBlock;
  locale: string;
  body: string;
  noteLabel: string;
}) {
  if ("p" in block) {
    return <p className={`${body} mt-3`}>{renderInline(block.p, locale)}</p>;
  }
  if ("code" in block) {
    return (
      <div className="mt-4 space-y-2">
        <p className="text-xs font-mono text-ink-soft">{block.lang}</p>
        <InstallCodeBlock cmd={block.code} copyLabel={getHome(locale).copy} copiedLabel={getHome(locale).copied} copyLocale={locale} installIntent={false} />
      </div>
    );
  }
  if ("rows" in block) {
    return (
      <dl className="docs-ref-rows mt-4">
        {block.rows.map(([term, detail]) => (
          <div key={term}>
            <dt>{block.codeTerms ? <code className="inline">{term}</code> : renderInline(term, locale)}</dt>
            <dd>{renderInline(detail, locale)}</dd>
          </div>
        ))}
      </dl>
    );
  }
  if ("steps" in block) {
    return (
      <ol className={`${body} docs-steps mt-3 list-decimal space-y-2 pl-6`}>
        {block.steps.map((step) => (
          <li key={step}>{renderInline(step, locale)}</li>
        ))}
      </ol>
    );
  }
  if ("list" in block) {
    return (
      <ul className={`${body} docs-list mt-3 list-disc space-y-2 pl-6`}>
        {block.list.map((item) => (
          <li key={item}>{renderInline(item, locale)}</li>
        ))}
      </ul>
    );
  }
  return (
    <p className={`${body} docs-note hairline-l mt-4 pl-4`}>
      <strong>{noteLabel}</strong>{" "}
      {renderInline(block.note, locale)}
    </p>
  );
}

export async function DocArticle({
  t,
  locale,
  extra,
}: {
  t: DocsPageDict;
  locale: string;
  /** Page-owned content rendered at the end of the section with this id. */
  extra?: Record<string, ReactNode>;
}) {
  const shell = getDocsShell(locale);
  const { sections, sourceSections } = resolveDocsPage(t, await docsCapabilityContext());
  const inSource = getChrome(locale).stateInSource;
  return (
    <div className="docs-article space-y-10">
      <header id="overview" className="scroll-mt-32">
        <h1 className="font-display text-3xl mb-1">{t.title}</h1>
        <p className={`${t.bodyClassName} docs-lede mt-3`}>{renderInline(t.lede, locale)}</p>
        {sections.length > 2 && (
          <nav className="docs-toc mt-5" aria-label={shell.onThisPage}>
            <ol className="flex flex-wrap gap-x-5 gap-y-1 text-sm">
              {sections.map((section) => (
                <li key={section.id}>
                  <a href={`#${section.id}`} className={LINK}>
                    {section.title}
                  </a>
                </li>
              ))}
              {sourceSections.length > 0 && (
                <li>
                  <a href="#in-source" className={LINK}>
                    {inSource}
                  </a>
                </li>
              )}
            </ol>
          </nav>
        )}
      </header>

      {sections.map((section) => (
        <section key={section.id} id={section.id} className="scroll-mt-32">
          <h2 className="font-display text-2xl mb-1">{section.title}</h2>
          {section.blocks.map((block, i) => (
            <Block
              key={i}
              block={block}
              locale={locale}
              body={t.bodyClassName}
              noteLabel={shell.noteLabel}
            />
          ))}
          {extra?.[section.id]}
        </section>
      ))}

      {sourceSections.length > 0 && (
        <section id="in-source" className="scroll-mt-32" data-release-scope="source">
          <h2 className="font-display text-2xl mb-1">{inSource}</h2>
          <p className={`${t.bodyClassName} mt-3`}>{renderInline(shell.sourceLead, locale)}</p>
          {sourceSections.map((section) => (
            <div key={section.id} id={section.id} className="scroll-mt-32 mt-6">
              <h3 className="font-display text-xl mb-1">{section.title}</h3>
              {section.blocks.map((block, i) => (
                <Block
                  key={i}
                  block={block}
                  locale={locale}
                  body={t.bodyClassName}
                  noteLabel={shell.noteLabel}
                />
              ))}
            </div>
          ))}
        </section>
      )}

      {t.next.length > 0 && (
        <section id="next" className="scroll-mt-32">
          <h2 className="font-display text-2xl mb-1">{shell.nextHeading}</h2>
          <div className="hairline-t mt-4">
            {t.next.map((item) => (
              <div key={item.href} className="py-4 hairline-b">
                <h3 className="font-display text-xl">
                  <Link href={`/${locale}${item.href}`} className={LINK}>
                    {item.label}
                  </Link>
                </h3>
                <p className={`${t.bodyClassName} mt-1 text-sm`}>{renderInline(item.note, locale)}</p>
              </div>
            ))}
          </div>
        </section>
      )}

      {/* Maintainer pointer: kept out of the rendered copy. */}
      <div hidden data-source-note={t.sourceNote} />
    </div>
  );
}
