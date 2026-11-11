import type { ReactNode } from "react";
import { ENGLISH_ATTRS, fallbackAttrs } from "@/lib/i18n/fallback";

export function FallbackRegion({
  path,
  locale,
  children,
}: {
  path: string;
  locale: string;
  children: ReactNode;
}) {
  const attrs = fallbackAttrs(path, locale);
  if (!attrs) return <>{children}</>;
  return (
    <div className="locale-fallback" {...attrs}>
      {children}
    </div>
  );
}

export function EnglishRegion({ when, children }: { when: boolean; children: ReactNode }) {
  if (!when) return <>{children}</>;
  return (
    <div className="locale-fallback" {...ENGLISH_ATTRS}>
      {children}
    </div>
  );
}

export function EnglishText({ children }: { children: ReactNode }) {
  return <bdi {...ENGLISH_ATTRS}>{children}</bdi>;
}
