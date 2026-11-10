export interface DocSearchSection {
  href: string;
  pageTitle: string;
  title: string;
  text: string;
  lang?: string;
}

export function searchDocSections(sections: readonly DocSearchSection[], query: string): (DocSearchSection & { excerpt: string })[] {
  const normalized = query.trim().toLowerCase();
  if (!normalized) return [];
  return sections.filter((section) => `${section.pageTitle} ${section.title} ${section.text}`.toLowerCase().includes(normalized)).map((section) => {
    const text = section.text.replace(/\[([^\]]+)\]\([^)]+\)/g, "$1").replace(/`/g, "").replace(/\s+/g, " ");
    const position = text.toLowerCase().indexOf(normalized);
    const start = Math.max(0, position - 70);
    const end = Math.min(text.length, start + 240);
    return { ...section, excerpt: `${start ? "…" : ""}${text.slice(start, end)}${end < text.length ? "…" : ""}` };
  });
}
