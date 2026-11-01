import { AUTO_ROUTING_COPY } from "./content/faq-routing";
import { createElement } from "react";
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { buildFaqPageJsonLd } from "./faq-schema";
import { serializeJsonLd } from "./json-ld";
import { extractText, flattenExtractedText } from "./react-text";
import { SITE_URL } from "./page-meta";

const faqContent = readFileSync(new URL("./content/faq.tsx", import.meta.url), "utf8");
const faqPage = readFileSync(new URL("../app/[locale]/faq/page.tsx", import.meta.url), "utf8");

describe("FAQPage structured data", () => {
  it("maps every question to an acceptedAnswer with flattened text", () => {
    const items = [
      {
        q: "What is Codewhale?",
        a: createElement(
          "p",
          null,
          "A terminal-native agent. Run ",
          createElement("code", null, "codewhale"),
          ".",
        ),
      },
      {
        q: "How do I install?",
        a: ["npm install -g codewhale", createElement("span", null, " or Cargo.")],
      },
    ];
    const schema = buildFaqPageJsonLd({
      items,
      url: `${SITE_URL}/en/faq`,
      inLanguage: "en",
    });

    expect(schema).toEqual({
      "@context": "https://schema.org",
      "@type": "FAQPage",
      url: "https://codewhale.net/en/faq",
      inLanguage: "en",
      mainEntity: [
        {
          "@type": "Question",
          name: "What is Codewhale?",
          acceptedAnswer: {
            "@type": "Answer",
            text: "A terminal-native agent. Run codewhale .",
          },
        },
        {
          "@type": "Question",
          name: "How do I install?",
          acceptedAnswer: {
            "@type": "Answer",
            text: "npm install -g codewhale or Cargo.",
          },
        },
      ],
    });
  });

  it("covers all 48 curated pairs from the shared FAQ content", () => {
    const questions = [...faqContent.matchAll(/^\s+q: "(.+)"/gm)].map((match) => match[1]);
    questions.push(AUTO_ROUTING_COPY.question.en, AUTO_ROUTING_COPY.question.zh);
    expect(questions).toHaveLength(48);
    expect(new Set(questions).size).toBe(48);
    expect(faqContent).toContain("const faqEn");
    expect(faqContent).toContain("const faqZh");
    expect(faqPage).toContain("buildFaqPageJsonLd({");
    expect(faqPage).toContain("items,");
    expect(faqPage).toContain('canonicalLocaleForPath("/faq", locale)');
    expect(faqPage).toContain('type="application/ld+json"');
    expect(faqPage).toContain("serializeJsonLd(jsonLd)");
  });

  it("keeps every internal answer link on the reader's locale", () => {
    // Every locale without its own FAQ reads the English answers, so a
    // literal `/en/...` link sent a /ja reader to the English site (W01-02).
    expect(faqContent).not.toMatch(/href=\{?["'`]\/[a-z]{2}(?:-[A-Za-z]+)?\//);
  });
});

describe("extractText", () => {
  it("flattens nested elements and ignores booleans", () => {
    const node = createElement(
      "div",
      null,
      "Hello ",
      createElement("code", null, "codewhale"),
      false,
      createElement("span", null, [" ", 2]),
    );

    expect(extractText(node)).toContain("codewhale");
    expect(flattenExtractedText(node)).toBe("Hello codewhale 2");
  });

  it("escapes angle brackets so JSON-LD cannot close the script tag", () => {
    expect(serializeJsonLd({ text: "</script>" })).toBe('{"text":"\\u003c/script>"}');
  });
});
