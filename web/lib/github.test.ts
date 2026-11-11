import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { fetchRepoStats, lastPageFromLink, relativeAge, relativeTime } from "./github";

// We test the pure helper functions directly.
// The async fetch functions require mocking the global fetch.

describe("production-build fallback", () => {
  it("keeps static generation offline and uses the dated snapshot and never a zero", async () => {
    const previousPhase = process.env.NEXT_PHASE;
    process.env.NEXT_PHASE = "phase-production-build";
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);

    try {
      const stats = await fetchRepoStats();
      expect(stats.source).toBe("snapshot");
      expect(stats.stars).toBeGreaterThan(0);
      expect(stats.contributors).toBeNull();
      expect(fetchMock).not.toHaveBeenCalled();
    } finally {
      if (previousPhase === undefined) delete process.env.NEXT_PHASE;
      else process.env.NEXT_PHASE = previousPhase;
      vi.unstubAllGlobals();
    }
  });
});

// ── relativeTime ──────────────────────────────────────────────────────

describe("relativeTime", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-06-01T12:00:00Z"));
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("returns 'just now' for less than 30 seconds ago", () => {
    expect(relativeTime("2026-06-01T11:59:45Z")).toBe("just now");
  });

  it("returns 'just now' for dates in the future", () => {
    expect(relativeTime("2026-06-02T12:00:00Z")).toBe("just now");
  });

  it("returns minutes for < 1 hour", () => {
    expect(relativeTime("2026-06-01T11:55:00Z")).toBe("5m");
    expect(relativeTime("2026-06-01T11:30:00Z")).toBe("30m");
  });

  it("returns hours for < 1 day", () => {
    expect(relativeTime("2026-06-01T09:00:00Z")).toBe("3h");
    expect(relativeTime("2026-05-31T18:00:00Z")).toBe("18h");
  });

  it("returns days for < 30 days", () => {
    expect(relativeTime("2026-05-25T12:00:00Z")).toBe("7d");
    expect(relativeTime("2026-05-03T12:00:00Z")).toBe("29d");
  });

  it("returns months for < 12 months", () => {
    expect(relativeTime("2026-03-01T12:00:00Z")).toBe("3mo");
    expect(relativeTime("2025-08-01T12:00:00Z")).toBe("10mo");
  });

  it("returns years for >= 12 months", () => {
    expect(relativeTime("2024-06-01T12:00:00Z")).toBe("2y");
    expect(relativeTime("2025-01-01T00:00:00Z")).toBe("1y");
  });
});

// ── relativeAge ───────────────────────────────────────────────────────

describe("relativeAge", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-06-01T12:00:00Z"));
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("reports past ages as the negative counts Intl.RelativeTimeFormat wants", () => {
    expect(relativeAge("2026-06-01T11:55:00Z")).toEqual({ value: -5, unit: "minute" });
    expect(relativeAge("2026-06-01T09:00:00Z")).toEqual({ value: -3, unit: "hour" });
    expect(relativeAge("2026-05-25T12:00:00Z")).toEqual({ value: -7, unit: "day" });
    expect(relativeAge("2026-03-01T12:00:00Z")).toEqual({ value: -3, unit: "month" });
    expect(relativeAge("2024-06-01T12:00:00Z")).toEqual({ value: -2, unit: "year" });
  });

  it("collapses sub-minute, future, and unparseable dates to the locale's 'now'", () => {
    expect(relativeAge("2026-06-01T11:59:45Z")).toEqual({ value: 0, unit: "second" });
    expect(relativeAge("2026-06-02T12:00:00Z")).toEqual({ value: 0, unit: "second" });
    expect(relativeAge("not-a-date")).toEqual({ value: 0, unit: "second" });
  });

  it("agrees with the compact English form it backs", () => {
    for (const iso of [
      "2026-06-01T11:59:45Z",
      "2026-06-01T11:30:00Z",
      "2026-05-31T18:00:00Z",
      "2026-05-03T12:00:00Z",
      "2025-08-01T12:00:00Z",
      "2025-01-01T00:00:00Z",
    ]) {
      const age = relativeAge(iso);
      const compact = relativeTime(iso);
      if (age.unit === "second") expect(compact).toBe("just now");
      else expect(compact.startsWith(String(Math.abs(age.value)))).toBe(true);
    }
  });
});

// ── lastPageFromLink (via re-export test) ──────────────────────────────

describe("lastPageFromLink", () => {
  it("returns undefined for null input", () => {
    expect(lastPageFromLink(null)).toBeUndefined();
  });

  it("returns undefined for empty string", () => {
    expect(lastPageFromLink("")).toBeUndefined();
  });

  it("extracts page from Link header with last rel", () => {
    const link =
      '<https://api.github.com/repos/codewhale-hq/CodeWhale/issues?page=5>; rel="last"';
    expect(lastPageFromLink(link)).toBe(5);
  });

  it("extracts page from multi-part Link header", () => {
    const link = [
      '<https://api.github.com/repos/codewhale-hq/CodeWhale/issues?page=1>; rel="prev"',
      '<https://api.github.com/repos/codewhale-hq/CodeWhale/issues?page=3>; rel="last"',
    ].join(", ");
    expect(lastPageFromLink(link)).toBe(3);
  });

  it("returns undefined when no last rel present", () => {
    const link =
      '<https://api.github.com/repos/codewhale-hq/CodeWhale/issues?page=1>; rel="prev"';
    expect(lastPageFromLink(link)).toBeUndefined();
  });

  it("returns undefined for invalid URL format", () => {
    const link = "not-a-valid-link-header; rel=last";
    expect(lastPageFromLink(link)).toBeUndefined();
  });
});
