"use client";

/**
 * Usage counting on the website — the recorder owner and the
 * privacy-page control.
 *
 *   <UsageCounting>          Mounted once in the locale layout. Renders
 *                            nothing. Counts `page_view` on every route
 *                            change (`docs_view` too on docs routes) and any
 *                            element carrying `data-usage="<counter>"` when it
 *                            is clicked; flushes once after a pause or on
 *                            pagehide. `recordUsage()` is the import for
 *                            client components (copy buttons, the error
 *                            boundary).
 *   <UsagePreferenceControl> The only place on the site that shows or changes
 *                            the choice. It lives on the privacy page, not in
 *                            a banner: counting is on by default, every
 *                            opt-out stays off, and other tabs follow through
 *                            the `storage` event.
 *
 * Nothing renders on the server, so the page is complete and static without
 * it; reduced motion is respected because there is no motion.
 */

import { usePathname } from "next/navigation";
import { useCallback, useEffect, useState } from "react";
import { USAGE_COUNTING_COPY } from "@/lib/content/usage-counting";
import { pickText } from "@/lib/i18n/dictionaries";
import { funnelExit, funnelPath } from "@/lib/telemetry/funnel";
import {
  PREFERENCE_STORAGE_KEY,
  PRODUCT_COUNTER_FIELDS,
  FUNNEL_COUNTER_FIELDS,
  type FunnelCounter,
  createUsageRecorder,
  privacySignalActive,
  type ProductCounter,
  type UsagePreference,
  type UsageRecorder,
} from "@/lib/telemetry/product-usage";

const ENDPOINT = "/api/product-telemetry";

let recorder: UsageRecorder | null = null;

function getRecorder(appVersion: string): UsageRecorder | null {
  if (typeof window === "undefined") return null;
  if (!recorder) {
    let storage: Storage | null = null;
    try {
      storage = window.localStorage;
    } catch {
      storage = null;
    }
    if (!storage) return null;
    recorder = createUsageRecorder({
      surface: "website",
      appVersion,
      endpoint: ENDPOINT,
      storage,
      blocked: () => privacySignalActive(),
    });
  }
  return recorder;
}

/** Count one interaction. A no-op while counting is turned off. */
export function recordUsage(counter: ProductCounter): void {
  recorder?.record(counter);
}

export function recordFunnel(counter: FunnelCounter): void {
  recorder?.recordFunnel(counter);
}

function isCounter(value: string | undefined): value is ProductCounter {
  return value !== undefined && (PRODUCT_COUNTER_FIELDS as readonly string[]).includes(value);
}

export function UsageCounting({ appVersion }: { appVersion: string }) {
  const pathname = usePathname();

  useEffect(() => {
    const current = getRecorder(appVersion);
    if (!current) return;
    const onStorage = (event: StorageEvent) => {
      if (event.key !== null && event.key !== PREFERENCE_STORAGE_KEY) return;
      current.sync();
    };
    const onClick = (event: MouseEvent) => {
      const target = event.target instanceof Element ? event.target : null;
      const marked = target?.closest<HTMLElement>("[data-usage]");
      const counter = marked?.dataset.usage;
      if (isCounter(counter)) current.record(counter);
      const explicit = target?.closest<HTMLElement>("[data-funnel]")?.dataset.funnel;
      const link = target?.closest<HTMLAnchorElement>("a[href]");
      const funnel = explicit && (FUNNEL_COUNTER_FIELDS as readonly string[]).includes(explicit)
        ? explicit as FunnelCounter : link ? funnelExit(link.href) : null;
      if (funnel) current.recordFunnel(funnel);
    };
    const onPageHide = () => {
      void current.flush();
    };
    window.addEventListener("storage", onStorage);
    document.addEventListener("click", onClick);
    window.addEventListener("pagehide", onPageHide);
    return () => {
      window.removeEventListener("storage", onStorage);
      document.removeEventListener("click", onClick);
      window.removeEventListener("pagehide", onPageHide);
    };
  }, [appVersion]);

  // Every route change is one page view; registered docs destinations are also a docs view.
  useEffect(() => {
    const current = getRecorder(appVersion);
    if (!current || !pathname) return;
    current.record("page_view");
    const route = funnelPath(pathname);
    if (route.documentation) current.record("docs_view");
    if (route.view) current.recordFunnel(route.view);
  }, [appVersion, pathname]);

  return null;
}

/** The privacy page's status line and opt-out control. */
export function UsagePreferenceControl({ locale, appVersion }: { locale: string; appVersion: string }) {
  // `null` until mounted; `"unavailable"` when the browser offers no storage.
  const [preference, setPreference] = useState<UsagePreference | "unavailable" | "unsaved" | "signal" | null>(null);
  const t = (text: { en: string; zh: string }) => pickText(text, locale);

  useEffect(() => {
    if (privacySignalActive()) {
      setPreference("signal");
      return;
    }
    const current = getRecorder(appVersion);
    if (!current) {
      setPreference("unavailable");
      return;
    }
    setPreference(current.preference());
    const onStorage = (event: StorageEvent) => {
      if (event.key !== null && event.key !== PREFERENCE_STORAGE_KEY) return;
      setPreference(current.preference());
    };
    window.addEventListener("storage", onStorage);
    return () => window.removeEventListener("storage", onStorage);
  }, [appVersion]);

  const toggle = useCallback(() => {
    const current = getRecorder(appVersion);
    if (!current) return;
    const saved = current.preference() === "off" ? current.enable() : current.disable();
    setPreference(saved ? current.preference() : "unsaved");
  }, [appVersion]);

  const off = preference === "off" || preference === "unsaved";
  return (
    <div className="usage-counting" data-state={preference ?? "loading"}>
      <p>{t(USAGE_COUNTING_COPY.summary)}</p>
      <p>{t(USAGE_COUNTING_COPY.choice)}</p>
      {preference !== null && (
        <p className="usage-counting-status" role="status">
          {t(USAGE_COUNTING_COPY.status[preference])}
        </p>
      )}
      {preference !== null && preference !== "unavailable" && preference !== "signal" && (
        <button type="button" className="usage-counting-button" onClick={toggle} aria-pressed={!off}>
          {off ? t(USAGE_COUNTING_COPY.turnOn) : t(USAGE_COUNTING_COPY.turnOff)}
        </button>
      )}
      <p className="usage-counting-elsewhere">{t(USAGE_COUNTING_COPY.elsewhere)}</p>
    </div>
  );
}
