"use client";

import { useId, useState } from "react";
import { ONBOARDING_CHECK_COPY } from "@/lib/content/onboarding-check";
import { pickText, pickTextLocale } from "@/lib/i18n/dictionaries";
import { recordFunnel } from "./usage-counting";

export function OnboardingCheck({ kind, locale }: { kind: "install" | "quickstart"; locale: string }) {
  const [checked, setChecked] = useState(false);
  const description = useId();
  const copy = ONBOARDING_CHECK_COPY[kind];
  return (
    <div className="onboarding-check" lang={pickTextLocale(locale)}>
      <button type="button" className="onboarding-check-btn" disabled={checked} aria-describedby={description} onClick={() => {
        if (checked) return;
        recordFunnel(kind === "install" ? "install_confirmed" : "quickstart_confirmed");
        setChecked(true);
      }}>{pickText(copy.label, locale)}</button>
      <p id={description} className="text-sm" role="status">{pickText(checked ? ONBOARDING_CHECK_COPY.checked : copy.detail, locale)}</p>
    </div>
  );
}
