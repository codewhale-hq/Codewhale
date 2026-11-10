/**
 * <GettingStartedSteps> — renders the shared new-user path from
 * web/lib/content/getting-started.ts: install → provider connection →
 * first task → verify the answer → optional Fleet setup.
 *
 * Used by the homepage and the /docs/guide page so the path reads
 * identically in both places. Server component, SSG-safe.
 */

import Link from "next/link";
import { OnboardingCheck } from "./onboarding-check";
import { GETTING_STARTED_STEPS } from "@/lib/content/getting-started";
import { getHome, pickText } from "@/lib/i18n/dictionaries";
import { InstallCodeBlock } from "./install-code-block";
import { Icon } from "./icon";

export function GettingStartedSteps({
  locale = "en",
  headingLevel = 3,
}: {
  locale?: string;
  /** 2 when the steps sit directly under the page title, 3 under a section. */
  headingLevel?: 2 | 3;
}) {
  const home = getHome(locale);
  const StepHeading = headingLevel === 2 ? "h2" : "h3";
  const renderStep = (step: (typeof GETTING_STARTED_STEPS)[number], index: number) => (
        <li key={step.id} id={step.id} data-step-id={step.id} className="scroll-mt-32">
          {!step.optional && <span className="gs-step-index" aria-hidden="true">{index + 1}</span>}
          <StepHeading className="gs-step-title">{pickText(step.title, locale)}</StepHeading>
          <p>{pickText(step.body, locale)}</p>
          {step.commands.length > 0 && (
            <InstallCodeBlock cmd={step.commands.join("\n")} copyLabel={home.copy} copiedLabel={home.copied} copyLocale={locale} installIntent={step.id === "install"} />
          )}
          {step.prompt && <InstallCodeBlock cmd={pickText(step.prompt, locale)} copyLabel={home.copy} copiedLabel={home.copied} copyLocale={locale} installIntent={false} />}
          {step.id === "verify-answer" && <OnboardingCheck kind="quickstart" locale={locale} />}
          <Link href={`/${locale}${step.link.href}`} className="gs-step-link">
            {pickText(step.link.label, locale)}
            <Icon name="arrow-right" className="icon icon-flip" />
          </Link>
        </li>
      );
  return (
    <div className="space-y-8">
      <ol className="gs-steps">{GETTING_STARTED_STEPS.filter((step) => !step.optional).map(renderStep)}</ol>
      <aside className="hairline-t pt-6">
        <ul className="gs-steps !grid-cols-1">{GETTING_STARTED_STEPS.filter((step) => step.optional).map(renderStep)}</ul>
      </aside>
    </div>
  );
}
