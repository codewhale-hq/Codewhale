import { ALL_LOCALES } from "./config";

export type ReviewState = "native" | "machine";

const NATIVE_LOCALES: readonly string[] = ["en"];

export function reviewState(locale: string): ReviewState {
  return NATIVE_LOCALES.includes(locale) ? "native" : "machine";
}

export const MACHINE_DRAFT_LOCALES: readonly string[] = ALL_LOCALES.map((l) => l.code).filter((code) => reviewState(code) === "machine");
