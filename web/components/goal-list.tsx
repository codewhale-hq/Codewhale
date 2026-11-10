import { Icon } from "./icon";
import { PROJECT_COPY, PROJECT_GOALS } from "@/lib/content/project";
import { pickText } from "@/lib/i18n/dictionaries";

export function GoalList({ locale, detail = false }: { locale: string; detail?: boolean }) {
  return (
    <ol className={detail ? "goal-list goal-list-detail" : "goal-list"} role="list">
      {PROJECT_GOALS.map((goal, index) => (
        <li key={goal.title.en} className="goal">
          <span className="goal-number tabular" aria-hidden="true">{String(index + 1).padStart(2, "0")}</span>
          <div className="goal-text">
            <h3>{pickText(goal.title, locale)}</h3>
            <p>{pickText(goal.body, locale)}</p>
            {detail ? (
              <>
                <p className="goal-open">
                  <span className="goal-open-label">{pickText(PROJECT_COPY.goalOpen, locale)}</span>
                  {pickText(goal.open, locale)}
                </p>
                <p className="goal-links">
                  {goal.links.map((link) => (
                    <a key={link.href} href={link.href} className="section-link">
                      {pickText(link.label, locale)}
                      <Icon name="external" className="icon" />
                    </a>
                  ))}
                </p>
              </>
            ) : null}
          </div>
        </li>
      ))}
    </ol>
  );
}
