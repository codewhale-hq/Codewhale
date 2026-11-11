import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { USAGE_COUNTING_COPY } from "./content/usage-counting";
import { WEBSITE_USAGE_DISCLOSURE } from "./legal-copy";
import { PRODUCT_COUNTER_FIELDS, type ProductCounter } from "./telemetry/product-usage";

// How the public copy names each counter the website can send.
const WEBSITE_COUNTER_WORDING: Partial<Record<ProductCounter, { en: string; zh: string }>> = {
  page_view: { en: "page views", zh: "页面浏览" },
  docs_view: { en: "documentation views", zh: "文档浏览" },
  install_copy: { en: "install-command copies", zh: "安装命令复制" },
  download: { en: "downloads", zh: "下载" },
  login: { en: "sign-in", zh: "登录" },
  signup: { en: "sign-up", zh: "注册" },
  error_shown: { en: "error pages shown", zh: "错误页面" },
};

function sources(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return sources(path);
    return /\.tsx?$/.test(entry.name) && !/\.test\.tsx?$/.test(entry.name) ? [path] : [];
  });
}

/** Counters the website records: route views plus every call site and marker. */
function recordedCounters(): Set<string> {
  const recorded = new Set<string>(["page_view", "docs_view"]);
  for (const dir of ["../app", "../components"]) {
    for (const file of sources(fileURLToPath(new URL(dir, import.meta.url)))) {
      const text = readFileSync(file, "utf8");
      for (const m of text.matchAll(/recordUsage\("(\w+)"\)|data-usage="(\w+)"/g)) {
        recorded.add(m[1] ?? m[2]);
      }
      for (const m of text.matchAll(/data-usage=\{[^}]*\}/g)) {
        for (const q of m[0].matchAll(/"(\w+)"/g)) recorded.add(q[1]);
      }
    }
  }
  return recorded;
}

describe("usage-counting copy", () => {
  const recorded = recordedCounters();
  const privacy = WEBSITE_USAGE_DISCLOSURE.body;

  it("only names website counters the copy knows how to describe", () => {
    for (const counter of recorded) {
      expect(PRODUCT_COUNTER_FIELDS as readonly string[]).toContain(counter);
      expect(WEBSITE_COUNTER_WORDING).toHaveProperty(counter);
    }
  });

  it.each(Object.entries(WEBSITE_COUNTER_WORDING))(
    "names %s exactly when the site records it",
    (counter, wording) => {
      const sent = recorded.has(counter);
      expect(USAGE_COUNTING_COPY.summary.en.includes(wording.en)).toBe(sent);
      expect(USAGE_COUNTING_COPY.summary.zh.includes(wording.zh)).toBe(sent);
      expect(privacy.includes(wording.en)).toBe(sent);
    },
  );

  it("states how counts are keyed and which signals are honoured, and never calls them anonymous", () => {
    const english = [privacy, USAGE_COUNTING_COPY.summary.en, USAGE_COUNTING_COPY.choice.en].join(" ");
    const chinese = [USAGE_COUNTING_COPY.summary.zh, USAGE_COUNTING_COPY.choice.zh].join(" ");
    for (const phrase of ["random install id", "90 days", "Global Privacy Control", "Do Not Track", "referrer", "not anonymous"]) {
      expect(english, phrase).toContain(phrase);
    }
    for (const phrase of ["随机安装 id", "90 天", "Global Privacy Control", "Do Not Track", "来源", "匿名"]) {
      expect(chinese, phrase).toContain(phrase);
    }
    for (const text of [english, chinese, JSON.stringify(USAGE_COUNTING_COPY.status)]) {
      expect(text).not.toMatch(/plain totals|纯总数|Anonymous usage|匿名使用统计/);
    }
    expect(WEBSITE_USAGE_DISCLOSURE.title).not.toMatch(/anonymous/i);
  });

  it("names the program's off switches exactly", () => {
    for (const text of [privacy, USAGE_COUNTING_COPY.elsewhere.en, USAGE_COUNTING_COPY.elsewhere.zh]) {
      expect(text).toContain("codewhale config set telemetry false");
      expect(text).toContain("CODEWHALE_TELEMETRY=0");
    }
  });
});
