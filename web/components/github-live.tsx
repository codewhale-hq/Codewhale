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
