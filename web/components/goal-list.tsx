import { Icon } from "./icon";
import { WhalePose, type WhalePoseName } from "./whale-pose";
import { PROJECT_COPY, PROJECT_GOALS } from "@/lib/content/project";
import { pickText } from "@/lib/i18n/dictionaries";

const GOAL_POSES: WhalePoseName[] = ["run", "connect", "needs", "pod", "read", "talk", "think"];

export function GoalList({ locale, detail = false }: { locale: string; detail?: boolean }) {
  return (
    <ol className={detail ? "goal-list goal-list-detail" : "goal-list"} role="list">
      {PROJECT_GOALS.map((goal, index) => (
        <li key={goal.title.en} className="goal">
          <WhalePose pose={GOAL_POSES[index] ?? "rest"} className="goal-pose" />
          <div className="goal-text">
            <h3><span className="goal-number tabular" aria-hidden="true">{String(index + 1).padStart(2, "0")}</span>{pickText(goal.title, locale)}</h3>
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
