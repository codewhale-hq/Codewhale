/**
 * Small, dependency-free Markdown renderer for chat transcripts.
 *
 * Scope is deliberately a safe subset — fenced code blocks, headings, inline
 * code, bold, links (http/https only), lists, and horizontal rules. All text
 * is HTML-escaped before any transform runs, so model output can never inject
 * markup. Fenced blocks are also returned raw so the webview can offer
 * Copy/Insert actions without round-tripping through the DOM.
 */

export interface RenderedMarkdown {
  html: string;
  codeBlocks: string[];
}

export function renderMarkdown(source: string): RenderedMarkdown {
  const codeBlocks: string[] = [];
  const lines = source.replace(/\r\n/g, "\n").split("\n");
  const blocks: string[] = [];

  let index = 0;
  while (index < lines.length) {
    const line = lines[index];
    const fence = matchFence(line);
    if (fence !== undefined) {
      const code: string[] = [];
      index += 1;
      while (index < lines.length && matchFence(lines[index]) === undefined) {
        code.push(lines[index]);
        index += 1;
      }
      index += 1; // consume the closing fence (or run off the end)
      const slot = codeBlocks.length;
      codeBlocks.push(code.join("\n"));
      blocks.push(renderCodeBlock(slot, fence, code.join("\n")));
      continue;
    }

    if (line.trim() === "") {
      index += 1;
      continue;
    }

    if (/^\s*(?:-{3,}|\*{3,}|_{3,})\s*$/.test(line)) {
      blocks.push(`<hr>`);
      index += 1;
      continue;
    }

    const heading = line.match(/^(#{1,4})\s+(.*)$/);
    if (heading) {
      const level = String(heading[1].length + 2); // demote: # -> h3 … keeps sidebar scale sane
      blocks.push(`<h${level}>${inline(heading[2])}</h${level}>`);
      index += 1;
      continue;
    }

    const bullet = line.match(/^\s*[-*+]\s+(.*)$/);
    const numbered = line.match(/^\s*\d+[.)]\s+(.*)$/);
    if (bullet || numbered) {
      const ordered = Boolean(numbered);
      const items: string[] = [];
      while (index < lines.length) {
        const itemLine = lines[index].match(ordered ? /^\s*\d+[.)]\s+(.*)$/ : /^\s*[-*+]\s+(.*)$/);
        if (!itemLine) {
          break;
        }
        items.push(`<li>${inline(itemLine[1])}</li>`);
        index += 1;
      }
      blocks.push(ordered ? `<ol>${items.join("")}</ol>` : `<ul>${items.join("")}</ul>`);
      continue;
    }

    const paragraph: string[] = [];
    while (
      index < lines.length &&
      lines[index].trim() !== "" &&
      matchFence(lines[index]) === undefined &&
      !/^#{1,4}\s/.test(lines[index]) &&
      !/^\s*[-*+]\s+/.test(lines[index]) &&
      !/^\s*\d+[.)]\s+/.test(lines[index]) &&
      !/^\s*(?:-{3,}|\*{3,}|_{3,})\s*$/.test(lines[index])
    ) {
      paragraph.push(inline(lines[index]));
      index += 1;
    }
    blocks.push(`<p>${paragraph.join("<br>")}</p>`);
  }

  return { html: blocks.join("\n"), codeBlocks };
}

function matchFence(line: string): string | undefined {
  const match = line.match(/^\s*(```|~~~)\s*([\w+#.-]*)\s*$/);
  return match ? match[2] : undefined;
}

function renderCodeBlock(slot: number, language: string, code: string): string {
  const label = language || "code";
  const firstLine = code.split("\n", 1)[0] ?? "";
  return (
    `<div class="codeblock" data-cb="${slot}">` +
    `<div class="codeblock-bar"><span class="codeblock-lang">${escapeHtml(label)}</span>` +
    `<span class="codeblock-actions">` +
    `<button type="button" class="cb-copy" data-cb="${slot}" title="Copy code">Copy</button>` +
    `<button type="button" class="cb-insert" data-cb="${slot}" title="Insert at cursor">Insert</button>` +
    `</span></div>` +
    `<pre><code title="${escapeHtml(Array.from(firstLine).slice(0, 80).join(""))}">${escapeHtml(code)}</code></pre>` +
    `</div>`
  );
}

function inline(text: string): string {
  let result = escapeHtml(text);
  const spans: string[] = [];
  result = result.replace(/`([^`]+)`/g, (_match, code: string) => {
    spans.push(`<code>${code}</code>`);
    return `\u0000S${spans.length - 1}\u0000`;
  });
  result = result.replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>");
  result = result.replace(/\[([^\]]+)\]\((https?:\/\/[^)\s]+)\)/g, '<a href="$2">$1</a>');
  result = result.replace(/\u0000S(\d+)\u0000/g, (_match, slot: string) => spans[Number(slot)] ?? "");
  return result;
}

export function escapeHtml(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}
