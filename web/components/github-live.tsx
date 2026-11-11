export function LiveStarCount({ initial, numberLocale, asOf }: { initial?: number; numberLocale: string; asOf?: string }) {
  if (initial === undefined) return null;
  const formatted = new Intl.NumberFormat(numberLocale).format(initial);
  return (
    <span
      className="github-star-count tabular"
      title={asOf}
      aria-label={asOf ? `${formatted}, ${asOf}` : undefined}
    >
      {formatted}
    </span>
  );
}

export interface PulseRow { key: "stars" | "contributors" | "pulls" | "issues"; label: string; href: string; value?: string; asOf?: string }

export function LiveStarLink({ initial, label, href, numberLocale, asOf }: { initial?: number; label: string; href: string; numberLocale: string; asOf?: string }) {
  if (initial === undefined) return null;
  const formatted = new Intl.NumberFormat(numberLocale).format(initial);
  return (
    <a href={href} className="section-link home-proof-stars" title={asOf} aria-label={asOf ? `${formatted} ${label}, ${asOf}` : undefined}>
      <span aria-hidden="true">★</span>
      <span className="tabular">{formatted}</span>
      <span>{label}</span>
    </a>
  );
}

export function LivePulseStats({ rows }: { rows: PulseRow[]; numberLocale?: string }) {
  return (
    <dl className="pulse-stats">
      {rows.map((row) => {
        if (row.value === undefined) return null;
        return (
          <div key={row.key}>
            <dt>{row.label}</dt>
            <dd>
              <a
                href={row.href}
                className="tabular"
                title={row.asOf}
                aria-label={row.asOf ? `${row.value} ${row.label}, ${row.asOf}` : undefined}
              >
                {row.value}
              </a>
            </dd>
          </div>
        );
      })}
    </dl>
  );
}
