export interface HtmlTextOptions {
  stripScripts?: boolean;
  stripCapturePre?: boolean;
  tagReplacement?: string;
  decodeEntities?: boolean;
}

const ENTITY_MAP: Record<string, string> = {
  "&lt;": "<",
  "&gt;": ">",
  "&quot;": '"',
  "&#x27;": "'",
  "&amp;": "&",
};

function decodeBasicEntities(text: string): string {
  let out = "";
  let i = 0;
  while (i < text.length) {
    if (text[i] === "&") {
      const semi = text.indexOf(";", i + 1);
      if (semi > i && semi - i <= 8) {
        const entity = text.slice(i, semi + 1);
        const mapped = ENTITY_MAP[entity];
        if (mapped !== undefined) {
          out += mapped;
          i = semi + 1;
          continue;
        }
      }
    }
    out += text[i];
    i += 1;
  }
  return out;
}

function tagNameAt(html: string, start: number): { closing: boolean; name: string; end: number; raw: string } | null {
  const close = html.indexOf(">", start + 1);
  if (close === -1) return null;
  const raw = html.slice(start + 1, close);
  const trimmed = raw.trimStart();
  const closing = trimmed.startsWith("/");
  const body = closing ? trimmed.slice(1).trimStart() : trimmed;
  let nameEnd = 0;
  while (nameEnd < body.length && /[A-Za-z0-9-]/.test(body[nameEnd])) {
    nameEnd += 1;
  }
  return { closing, name: body.slice(0, nameEnd).toLowerCase(), end: close, raw };
}

export function extractHtmlText(html: string, options: HtmlTextOptions = {}): string {
  const {
    stripScripts = false,
    stripCapturePre = false,
    tagReplacement = " ",
    decodeEntities = false,
  } = options;
  let out = "";
  let i = 0;
  let skipUntilTag: string | null = null;

  while (i < html.length) {
    const ch = html[i];
    if (ch === "<") {
      const tag = tagNameAt(html, i);
      if (tag) {
        if (skipUntilTag !== null) {
          if (tag.closing && tag.name === skipUntilTag) {
            skipUntilTag = null;
            out += tagReplacement;
          }
          i = tag.end + 1;
          continue;
        }
        if (!tag.closing && stripScripts && tag.name === "script") {
          skipUntilTag = "script";
          i = tag.end + 1;
          continue;
        }
        if (
          !tag.closing &&
          stripCapturePre &&
          tag.name === "pre" &&
          tag.raw.toLowerCase().includes("data-capture-file")
        ) {
          skipUntilTag = "pre";
          i = tag.end + 1;
          continue;
        }
        out += tagReplacement;
        i = tag.end + 1;
        continue;
      }
    }
    if (skipUntilTag === null) {
      out += ch;
    }
    i += 1;
  }

  return decodeEntities ? decodeBasicEntities(out) : out;
}

export function extractJsonLdScripts(html: string): string[] {
  const blocks: string[] = [];
  let i = 0;
  while (i < html.length) {
    const open = html.indexOf("<", i);
    if (open === -1) break;
    const tag = tagNameAt(html, open);
    if (!tag) break;
    if (!tag.closing && tag.name === "script" && tag.raw.toLowerCase().includes('type="application/ld+json"')) {
      let cursor = tag.end + 1;
      let endIdx = -1;
      while (cursor < html.length) {
        const nextOpen = html.indexOf("<", cursor);
        if (nextOpen === -1) break;
        const endTag = tagNameAt(html, nextOpen);
        if (endTag && endTag.closing && endTag.name === "script") {
          endIdx = nextOpen;
          i = endTag.end + 1;
          break;
        }
        cursor = nextOpen + 1;
      }
      if (endIdx !== -1) {
        blocks.push(html.slice(tag.end + 1, endIdx));
        continue;
      }
    }
    i = tag.end + 1;
  }
  return blocks;
}
