import { Icon } from "./icon";
import type { ModelFact } from "@/lib/facts";
import { MODELS_COPY, groupModelsByMaker, formatModelTokens, formatModelDate } from "@/lib/content/models";
import { fill, getChrome, pickText } from "@/lib/i18n/dictionaries";

export function ModelBadge({ locale }: { locale: string }) {
  return <span className="models-reasoning">{pickText(MODELS_COPY.reasoning, locale)}</span>;
}

export function ModelsCatalog({ models, locale }: { models: ModelFact[]; locale: string }) {
  const t = (copy: { en: string; zh: string }) => pickText(copy, locale);
  const dateLocale = getChrome(locale).dateLocale;
  const groups = groupModelsByMaker(models);

  return (
    <div className="group-card">
      {groups.map((group) => {
        const label = group.label || t(MODELS_COPY.otherMakers);
        return (
          <details key={group.key} className="disclosure-row">
            <summary>
              <span>{label}</span>
              <span className="models-count">{fill(t(MODELS_COPY.models), { count: String(group.models.length) })}</span>
              <Icon name="chevron-down" className="disclosure-chevron" />
            </summary>
            <div className="disclosure-body">
              <div className="data-table-wrap">
                <table className="data-table">
                  <thead>
                    <tr>
                      <th scope="col">{t(MODELS_COPY.colModel)}</th>
                      <th scope="col"><span className="sr-only">{t(MODELS_COPY.colKind)}</span></th>
                      <th scope="col">{t(MODELS_COPY.colContext)}</th>
                      <th scope="col">{t(MODELS_COPY.colAdded)}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {group.models.map((model) => (
                      <tr key={model.id}>
                        <th scope="row">
                          <code>{model.id}</code>
                        </th>
                        <td>{model.reasoning ? <ModelBadge locale={locale} /> : null}</td>
                        <td className="tabular">{formatModelTokens(model.contextWindow)}</td>
                        <td className="tabular">{formatModelDate(model.addedAt, dateLocale)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </div>
          </details>
        );
      })}
    </div>
  );
}
