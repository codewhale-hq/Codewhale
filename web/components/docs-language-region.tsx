"use client";

import type { ReactNode } from "react";
import { usePathname } from "next/navigation";

export function DocsLanguageRegion({
  locale,
  englishTopics,
  children,
}: {
  locale: string;
  englishTopics: string[];
  children: ReactNode;
}) {
  const pathname = usePathname() ?? "";
  const rest = pathname.split("/").filter(Boolean);
  const inDocs = rest[0] === locale && rest[1] === "docs";
  if (!inDocs || !englishTopics.includes(rest[2] ?? "")) return <>{children}</>;
  return (
    <div className="locale-fallback" lang="en" dir="ltr">
      {children}
    </div>
  );
}
