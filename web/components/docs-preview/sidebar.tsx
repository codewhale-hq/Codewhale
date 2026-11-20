"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import { DOCS_PREVIEW_NAV } from "@/lib/content/docs-preview";
import { pickText } from "@/lib/i18n/dictionaries";

export function DocsPreviewSidebar({ locale, label }: { locale: string; label: string }) {
  const pathname = usePathname();
  return (
    <nav className="dp-nav" aria-label={label}>
      {DOCS_PREVIEW_NAV.map((group) => (
        <div key={group.group.en} className="dp-nav-group">
          <p className="dp-nav-label">{pickText(group.group, locale)}</p>
          <ul>
            {group.pages.map((page) => {
              const href = `/${locale}/docs-preview/${page.slug}`;
              return (
                <li key={page.slug}>
                  <Link href={href} aria-current={pathname === href ? "page" : undefined}>{pickText(page.label, locale)}</Link>
                </li>
              );
            })}
          </ul>
        </div>
      ))}
    </nav>
  );
}
