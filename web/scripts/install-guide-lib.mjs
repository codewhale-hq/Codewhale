import { createHash } from "node:crypto";
import { Marked, Renderer } from "marked";
import GithubSlugger from "github-slugger";

const DOCS_URL = "https://github.com/codewhale-hq/CodeWhale/blob/main/docs/";

function docLink(href, sourcePath) {
  const documentUrl = new URL(sourcePath, DOCS_URL);
  const url = new URL(href, documentUrl);
  if (!["https:", "http:", "mailto:"].includes(url.protocol)) {
    throw new Error(`Unsupported install-guide link protocol: ${url.protocol}`);
  }
  if (href.startsWith("#")) return href;
  if (url.href.startsWith(`${documentUrl.href}#`)) return url.hash;
  return url.href;
}

/** Render the checked-in guide at build time; fenced commands stay copyable. */
export function buildInstallGuide(source, { sourcePath = "INSTALL.md", tableLabel = "Installation table" } = {}) {
  const slugger = new GithubSlugger();
  const anchors = [];
  const fragments = [];
  let tableNumber = 0;
  const marked = new Marked({
    gfm: true,
    renderer: {
      heading(token) {
        const id = slugger.slug(token.text);
        anchors.push(id);
        return `<h${token.depth} id="${id}">${this.parser.parseInline(token.tokens)}</h${token.depth}>\n`;
      },
      html({ text }) {
        // INSTALL.md uses explicit aliases for old incoming links. No other
        // raw HTML is needed; reject it rather than widening the HTML surface.
        const anchor = text.match(/^<a id="([\p{L}\p{M}\p{N}_-]+)">$/u);
        if (anchor) {
          anchors.push(anchor[1]);
          return text;
        }
        if (text === "</a>") return text;
        throw new Error(`Unsupported raw HTML in install guide: ${text.slice(0, 80)}`);
      },
      table(token) {
        const label = `${tableLabel} ${++tableNumber}: ${token.header.map((cell) => cell.text).join(" / ")}`
          .replaceAll("&", "&amp;").replaceAll('"', "&quot;").replaceAll("<", "&lt;").replaceAll(">", "&gt;");
        return `<div class="install-guide-table" role="region" aria-label="${label}" tabindex="0">${Renderer.prototype.table.call(this, token)}</div>\n`;
      },
    },
    walkTokens(token) {
      if (token.type === "link" || token.type === "image") {
        token.href = docLink(token.href, sourcePath);
        if (token.href.startsWith("#")) fragments.push(decodeURIComponent(token.href.slice(1)));
      }
    },
  });
  const tokens = marked.lexer(source);
  marked.walkTokens(tokens, marked.defaults.walkTokens);
  const searchSlugger = new GithubSlugger();
  const visibleText = (token) => {
    if (token.type === "html" || token.type === "space" || token.type === "hr") return "";
    if (token.type === "table") return [...token.header, ...token.rows.flat()].map(visibleText).join(" ");
    if (token.items) return token.items.map(visibleText).join(" ");
    if (token.tokens) return token.tokens.map(visibleText).join(" ");
    return token.text ?? "";
  };
  const searchSections = [];
  let preamble = "";
  for (const token of tokens) {
    if (token.type === "heading") {
      searchSections.push({ id: searchSlugger.slug(token.text), title: visibleText(token), text: "" });
    } else if (searchSections.length) {
      const section = searchSections[searchSections.length - 1];
      section.text = [section.text, visibleText(token)].filter(Boolean).join(" ");
    } else {
      preamble = [preamble, visibleText(token)].filter(Boolean).join(" ");
    }
  }
  const chunks = [];
  let htmlTokens = [];
  const flush = () => {
    if (htmlTokens.length) {
      chunks.push({ kind: "html", text: marked.parser(htmlTokens) });
      htmlTokens = [];
    }
  };
  for (const token of tokens) {
    if (token.type === "code") {
      flush();
      chunks.push({ kind: "code", text: token.text });
    } else {
      htmlTokens.push(token);
    }
  }
  flush();
  const missing = fragments.filter((id) => !anchors.includes(id));
  if (missing.length) throw new Error(`Missing INSTALL.md anchors: ${missing.join(", ")}`);
  return { sourceHash: createHash("sha256").update(source).digest("hex"), anchors, chunks, searchSections, preamble };
}

export function installAnchorErrors(text, anchors) {
  return [...text.matchAll(/INSTALL\.md#([\w%.-]+)/g)]
    .map((match) => decodeURIComponent(match[1]))
    .filter((id) => !anchors.includes(id));
}

export function renderInstallGuideModule(source, chineseSource) {
  const guide = buildInstallGuide(source);
  const chinese = chineseSource === undefined ? undefined : buildInstallGuide(chineseSource, { sourcePath: "zh_hans/INSTALL.md", tableLabel: "安装表" });
  const chineseGuide = chineseSource === undefined ? "" : `\nexport const CHINESE_INSTALL_GUIDE = ${JSON.stringify(chinese, null, 2)} as const;\n`;
  const localizedGuides = chineseSource === undefined ? "" : `\nexport const INSTALL_GUIDES = {\n  en: { guide: INSTALL_GUIDE, lang: "en", sourcePath: "INSTALL.md" },\n  zh: { guide: CHINESE_INSTALL_GUIDE, lang: "zh-Hans", sourcePath: "zh_hans/INSTALL.md" },\n} as const;\n`;
  return `// Generated from checked-in installation guides by scripts/derive-install.mjs. Do not edit.\n\nexport const INSTALL_GUIDE = ${JSON.stringify(guide, null, 2)} as const;\n${chineseGuide}${localizedGuides}`;
}
