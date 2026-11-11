"use client";

import { useSyncExternalStore } from "react";
import { countryLabel } from "@/lib/content/merch-storefront";

const subscribe = () => () => {};

export function useCountryLabel(locale: string): (code: string, fallback: string) => string {
  const mounted = useSyncExternalStore(subscribe, () => true, () => false);
  return (code, fallback) => (mounted && !locale.startsWith("en") ? countryLabel(code, fallback, locale) : fallback);
}
