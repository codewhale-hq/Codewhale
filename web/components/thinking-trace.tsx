import { getConstitution, pickText } from "@/lib/i18n/dictionaries";
import { CONSTITUTION_SCENES } from "@/lib/content/constitution";
import { Icon } from "./icon";
/**
 * "See how it decides" — explanatory prose that illustrates how the
 * constitution's rank shows up in a model's reasoning, paired with the
 * decision each line of reasoning led to.
 *
 * These are ILLUSTRATIONS, labelled as such on the page: short paraphrases
 * of the kind of reasoning the rank produces, not a transcript of one cited
 * session. If a real, linkable session transcript is ever published, cite it
 * here and change the label; until then, never present these as captured
 * output (see AGENTS.md Article II).
 */

export function ThinkingTrace({ locale = "en" }: { locale?: string }) {
  const t = getConstitution(locale);
  return (
    <div className="grid-3">
      {CONSTITUTION_SCENES.map((s, i) => (
        <figure key={i} className="trace">
          <figcaption className="trace-head">
            <span className="status status-accent">
              <span className="status-dot" aria-hidden="true" />
              {t.illustration}
            </span>
            <span className="trace-context">{pickText(s.context, locale)}</span>
          </figcaption>
          <p className="trace-body">{s.trace}</p>
          <p className="trace-cites">
            {s.cites.map((c) => (
              <span key={c} className="pill">{c}</span>
            ))}
          </p>
          <p className="trace-decision">
            <Icon name="arrow-right" className="icon icon-flip" />
            <span>{pickText(s.decision, locale)}</span>
          </p>
        </figure>
      ))}
    </div>
  );
}
