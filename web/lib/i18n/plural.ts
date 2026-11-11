export type PluralCategory = "zero" | "one" | "two" | "few" | "many" | "other";

export type PluralForms = Partial<Record<PluralCategory, string>> & { other: string };

export const PLURAL_CATEGORIES: readonly PluralCategory[] = ["zero", "one", "two", "few", "many", "other"];

export function pluralCategory(locale: string, n: number): PluralCategory {
  try {
    return new Intl.PluralRules(locale).select(n) as PluralCategory;
  } catch {
    return "other";
  }
}

export function pluralText(forms: PluralForms, n: number, locale: string, numberLocale: string = locale): string {
  const template = forms[pluralCategory(locale, n)] ?? forms.other;
  let formatted: string;
  try {
    formatted = new Intl.NumberFormat(numberLocale).format(n);
  } catch {
    formatted = String(n);
  }
  return template.replace(/\{n\}/g, formatted);
}

export function pluralCategoriesFor(locale: string): PluralCategory[] {
  const rules = new Intl.PluralRules(locale);
  const reached = new Set<PluralCategory>(["other"]);
  for (let n = 0; n <= 1000; n += 1) reached.add(rules.select(n) as PluralCategory);
  return PLURAL_CATEGORIES.filter((category) => reached.has(category));
}
