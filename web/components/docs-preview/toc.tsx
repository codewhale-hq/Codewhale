"use client";

import { useEffect, useRef, useState } from "react";

export function DocsPreviewToc({ label, headings }: { label: string; headings: readonly { id: string; depth: number; title: string }[] }) {
  const [current, setCurrent] = useState(headings[0]?.id ?? "");
  const rail = useRef<HTMLElement>(null);

  useEffect(() => {
    const targets = headings.map((heading) => document.getElementById(heading.id)).filter((element): element is HTMLElement => element !== null);
    if (!targets.length) return;
    const observer = new IntersectionObserver(
      (entries) => {
        const visible = entries.filter((entry) => entry.isIntersecting).sort((a, b) => a.boundingClientRect.top - b.boundingClientRect.top)[0];
        if (visible) setCurrent(visible.target.id);
      },
      { rootMargin: "-88px 0px -70% 0px" },
    );
    for (const target of targets) observer.observe(target);
    return () => observer.disconnect();
  }, [headings]);

  useEffect(() => {
    rail.current?.querySelector("[aria-current]")?.scrollIntoView({ block: "nearest" });
  }, [current]);

  if (headings.length < 2) return null;
  return (
    <nav ref={rail} className="dp-toc" aria-label={label}>
      <p className="dp-toc-label">{label}</p>
      <ol>
        {headings.map((heading) => (
          <li key={heading.id} data-depth={heading.depth}>
            <a href={`#${heading.id}`} aria-current={heading.id === current ? "location" : undefined}>{heading.title}</a>
          </li>
        ))}
      </ol>
    </nav>
  );
}
