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

export function rankDocSections(sections: readonly DocSearchSection[], query: string, limit = 12): (DocSearchSection & { excerpt: string })[] {
  const terms = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  if (!terms.length) return [];
  return sections.flatMap((section) => {
    const title = section.title.toLowerCase();
    const page = section.pageTitle.toLowerCase();
    const text = section.text.replace(/\s+/g, " ");
    const body = text.toLowerCase();
    let score = 0;
    for (const term of terms) {
      const hit = (title.includes(term) ? 8 : 0) + (page.includes(term) ? 3 : 0) + (body.includes(term) ? 1 : 0);
      if (!hit) return [];
      score += hit;
    }
    const position = Math.max(0, body.indexOf(terms[0]));
    const start = Math.max(0, position - 60);
    const end = Math.min(text.length, start + 180);
    return [{ ...section, score, excerpt: `${start ? "…" : ""}${text.slice(start, end)}${end < text.length ? "…" : ""}` }];
  }).sort((a, b) => b.score - a.score).slice(0, limit);
}
