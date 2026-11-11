#!/usr/bin/env node
/**
 * render-terminal-capture.mjs — turn real PTY cell captures into what the
 * site renders.
 *
 * Input: web/lib/terminal-captures/*.json, written by
 * crates/tui/tests/cucumber/launch_card_pty.rs::website_current_terminal_capture
 * (schema in manifest.json: {rows, cols, cells[row][col] -> {x, text, fg, bg,
 * flags}}; fg/bg are null, a 0-255 palette index, or [r,g,b]; flags are
 * rio-vt style bits).
 *
 * Output: web/lib/terminal-capture.generated.ts — every frame compacted into
 * rows of styled runs (adjacent cells with the same colours and attributes
 * merged), plus a style table. components/terminal-capture.tsx renders that
 * as live, selectable HTML text. Nothing is retouched: text, colours and
 * attributes are the cells the terminal emulator parsed.
 *
 *   node scripts/render-terminal-capture.mjs            write the module
 *   node scripts/render-terminal-capture.mjs --check    fail if it is stale
 *   node scripts/render-terminal-capture.mjs --html out.html [--frame home]
 *        write a standalone page of one frame (used to rasterize the README
 *        image from the same capture; see --png)
 *   node scripts/render-terminal-capture.mjs --png out.png [--frame home]
 *        rasterize that page with Playwright (PLAYWRIGHT_MODULE may point at
 *        a local playwright install; launched with the "chrome" channel)
 *   node scripts/render-terminal-capture.mjs --svg out.svg [--frame home]
 *        export the same native cells without a browser or a screenshot tool
 */
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { hex, paletteColor } from "./terminal-palette.mjs";

const webRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const captureDir = path.join(webRoot, "lib", "terminal-captures");
const outPath = path.join(webRoot, "lib", "terminal-capture.generated.ts");

// rio-vt crosswords::style::StyleFlags bits.
const INVERSE = 1 << 0;
const BOLD = 1 << 1;
const ITALIC = 1 << 2;
const DIM = 1 << 3;
const HIDDEN = 1 << 4;
const STRIKEOUT = 1 << 5;
const ANY_UNDERLINE = (1 << 6) | (1 << 7) | (1 << 8) | (1 << 9) | (1 << 10);

// Terminal defaults for cells that carry no colour of their own. The captured
// frames paint every visible glyph explicitly, so these only reach blanks.
const DEFAULT_FG = "#f6f2e8";
const DEFAULT_BG = "#0a1e33";

function color(value, fallback) {
  if (value === null || value === undefined) return fallback;
  if (typeof value === "number") return paletteColor(value);
  return hex(...value);
}

/** The frames the site ships, keyed by a short id. */
function frames() {
  const manifest = JSON.parse(readFileSync(path.join(captureDir, "manifest.json"), "utf8"));
  return manifest.frames.map((frame) => ({
    id: frame.file.replace(/^website-/, "").replace(/-\d+x\d+\.json$/, ""),
    file: frame.file,
    state: frame.state,
    manifest,
  }));
}

function compact(capture) {
  const styles = [];
  const styleIndex = new Map();
  const rows = capture.cells.map((row) => {
    const runs = [];
    for (const cell of row) {
      const flags = cell.flags ?? 0;
      let fg = color(cell.fg, DEFAULT_FG);
      let bg = color(cell.bg, DEFAULT_BG);
      if (flags & INVERSE) [fg, bg] = [bg, fg];
      const style = {
        fg,
        bg,
        ...(flags & BOLD ? { bold: true } : {}),
        ...(flags & ITALIC ? { italic: true } : {}),
        ...(flags & DIM ? { dim: true } : {}),
        ...(flags & ANY_UNDERLINE ? { underline: true } : {}),
        ...(flags & STRIKEOUT ? { strike: true } : {}),
      };
      // A blank cell only shows its background; do not split runs on the
      // attributes of a space.
      const text = flags & HIDDEN || cell.text === "" ? " " : cell.text;
      const key = text === " " ? `${bg}` : JSON.stringify(style);
      const last = runs.at(-1);
      if (last && (last.key === key || (text === " " && last.bg === bg))) {
        last.text += text;
        continue;
      }
      runs.push({ key, bg, style: text === " " ? { fg: DEFAULT_FG, bg } : style, text });
    }
    return runs.map((run) => {
      const id = JSON.stringify(run.style);
      if (!styleIndex.has(id)) {
        styleIndex.set(id, styles.length);
        styles.push(run.style);
      }
      return [run.text, styleIndex.get(id)];
    });
  });
  return { styles, rows };
}

function generate() {
  const all = frames();
  const { manifest } = all[0];
  const out = {};
  for (const frame of all) {
    const capture = JSON.parse(readFileSync(path.join(captureDir, frame.file), "utf8"));
    const { styles, rows } = compact(capture);
    out[frame.id] = {
      file: `web/lib/terminal-captures/${frame.file}`,
      state: frame.state,
      provenance: "development",
      cols: capture.cols,
      rows: capture.rows,
      styles,
      lines: rows,
    };
  }
  const header = `// GENERATED by web/scripts/render-terminal-capture.mjs — do not edit.
// Source: web/lib/terminal-captures/*.json, real PTY cell captures from
// ${manifest.source}
// (binary ${manifest.binaryVersion}, base commit ${manifest.baseCommit}, captured ${manifest.capturedAt}).
`;
  const meta = {
    source: manifest.source,
    binaryVersion: manifest.binaryVersion,
    baseCommit: manifest.baseCommit,
    capturedAt: manifest.capturedAt,
    conditions: manifest.conditions,
  };
  return `${header}
export type TerminalCaptureStyle = {
  fg: string;
  bg: string;
  bold?: boolean;
  italic?: boolean;
  dim?: boolean;
  underline?: boolean;
  strike?: boolean;
};

export type TerminalCaptureFrame = {
  file: string;
  state: string;
  provenance: "development" | "released";
  cols: number;
  rows: number;
  styles: TerminalCaptureStyle[];
  /** Each row is a list of [text, style index] runs. */
  lines: [string, number][][];
};

export const TERMINAL_CAPTURE_META = ${JSON.stringify(meta, null, 2)} as const;

export const TERMINAL_CAPTURE_FRAMES = ${JSON.stringify(out)} as const satisfies Record<string, TerminalCaptureFrame>;

export type TerminalCaptureFrameId = keyof typeof TERMINAL_CAPTURE_FRAMES;
`;
}

function escapeHtml(text) {
  return text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

/** A standalone page of one frame, drawn with the same rules as the component. */
function frameHtml(id) {
  const all = frames();
  const frame = all.find((f) => f.id === id);
  if (!frame) throw new Error(`unknown frame ${id}; known: ${all.map((f) => f.id).join(", ")}`);
  const capture = JSON.parse(readFileSync(path.join(captureDir, frame.file), "utf8"));
  const { styles, rows } = compact(capture);
  const body = rows
    .map(
      (runs) =>
        runs
          .map(([text, index]) => {
            const s = styles[index];
            const css = [
              `color:${s.fg}`,
              `background:${s.bg}`,
              s.bold ? "font-weight:700" : "",
              s.italic ? "font-style:italic" : "",
              s.dim ? "opacity:.6" : "",
              s.underline ? "text-decoration:underline" : "",
            ]
              .filter(Boolean)
              .join(";");
            const inner = [...text]
              .map((ch) => (/[\u2800-\u28ff]/.test(ch) ? `<span class="c">${ch}</span>` : escapeHtml(ch)))
              .join("");
            return `<span style="${css}">${inner}</span>`;
          })
          .join(""),
    )
    .map((row) => `<span class="r">${row}</span>`)
    .join("");
  const top = styles[rows[0][0][1]].bg;
  const bottom = styles[rows.at(-1)[0][1]].bg;
  return `<!doctype html><meta charset="utf-8"><title>${escapeHtml(frame.state)}</title>
<style>
html,body{margin:0;background:${DEFAULT_BG}}
pre{margin:0;padding:14px 16px;display:inline-block;font:15px/1.25 Menlo,"SF Mono",ui-monospace,monospace;white-space:pre;background:linear-gradient(${top},${bottom});font-variant-ligatures:none}
.r{display:block;height:1.25em}.r>span{display:inline-block;height:100%;vertical-align:top}
.c{display:inline-block;width:1ch;overflow:hidden;vertical-align:top}
</style>
<pre>${body}</pre>`;
}

const args = process.argv.slice(2);
const frameArg = args.includes("--frame") ? args[args.indexOf("--frame") + 1] : "home";

/** Native cell geometry, colours and glyphs, suitable for PNG rasterization. */
function frameSvg(id) {
  const frame = frames().find((item) => item.id === id);
  if (!frame) throw new Error(`unknown frame ${id}`);
  const capture = JSON.parse(readFileSync(path.join(captureDir, frame.file), "utf8"));
  const { styles, rows } = compact(capture);
  const shapes = [];
  rows.forEach((runs, row) => {
    let column = 0;
    for (const [text, index] of runs) {
      const style = styles[index];
      const glyphs = [...text];
      shapes.push(`<rect x="${column * 10}" y="${row * 20}" width="${glyphs.length * 10}" height="20" fill="${style.bg}"/>`);
      for (const glyph of glyphs) {
        const codepoint = glyph.codePointAt(0);
        if (codepoint >= 0x2800 && codepoint <= 0x28ff) {
          // Braille is a native eight-dot bitmap. Draw its actual set bits
          // rather than depending on a rasterizer's missing-font fallback.
          const dots = [[2.75, 3], [2.75, 7.5], [2.75, 12], [7.25, 3], [7.25, 7.5], [7.25, 12], [2.75, 16.5], [7.25, 16.5]];
          dots.forEach(([x, y], bit) => {
            if ((codepoint - 0x2800) & (1 << bit)) shapes.push(`<circle cx="${column * 10 + x}" cy="${row * 20 + y}" r="1.4" fill="${style.fg}"${style.dim ? ' opacity="0.6"' : ''}/>`);
          });
        } else if (glyph !== " ") {
          const attributes = [
            style.bold ? 'font-weight="700"' : '',
            style.italic ? 'font-style="italic"' : '',
            style.dim ? 'opacity="0.6"' : '',
            style.underline || style.strike ? `text-decoration="${[style.underline ? 'underline' : '', style.strike ? 'line-through' : ''].filter(Boolean).join(' ')}"` : '',
          ].filter(Boolean).join(' ');
          shapes.push(`<text x="${column * 10 + 5}" y="${row * 20 + 15}" fill="${style.fg}" ${attributes}>${escapeHtml(glyph)}</text>`);
        }
        column += 1;
      }
    }
  });
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${capture.cols * 10}" height="${capture.rows * 20}" viewBox="0 0 ${capture.cols * 10} ${capture.rows * 20}" font-family="Menlo,DejaVu Sans Mono,monospace" font-size="16" text-anchor="middle"><title>${escapeHtml(frame.state)}</title>${shapes.join('')}</svg>\n`;
}

if (args.includes("--svg")) {
  const target = args[args.indexOf("--svg") + 1];
  writeFileSync(target, frameSvg(frameArg));
  console.log(`wrote ${target}`);
} else if (args.includes("--html")) {
  const target = args[args.indexOf("--html") + 1];
  writeFileSync(target, frameHtml(frameArg));
  console.log(`wrote ${target}`);
} else if (args.includes("--png")) {
  const target = path.resolve(args[args.indexOf("--png") + 1]);
  const tmp = mkdtempSync(path.join(os.tmpdir(), "terminal-capture-"));
  const html = path.join(tmp, "frame.html");
  writeFileSync(html, frameHtml(frameArg));
  const spec = process.env.PLAYWRIGHT_MODULE;
  const mod = await import(spec ? pathToFileURL(path.join(spec, "index.mjs")).href : "playwright");
  const chromium = mod.chromium ?? mod.default.chromium;
  const browser = await chromium.launch({ channel: "chrome" });
  const page = await browser.newPage({ deviceScaleFactor: 2 });
  await page.goto(pathToFileURL(html).href);
  await page.locator("pre").screenshot({ path: target });
  await browser.close();
  rmSync(tmp, { recursive: true, force: true });
  console.log(`wrote ${target}`);
} else {
  const next = generate();
  if (args.includes("--check")) {
    let current = "";
    try {
      current = readFileSync(outPath, "utf8");
    } catch {
      // Missing counts as stale.
    }
    if (current !== next) {
      console.error("terminal-capture.generated.ts is stale: run node scripts/render-terminal-capture.mjs");
      process.exit(1);
    }
    console.log("terminal-capture.generated.ts is current");
  } else {
    writeFileSync(outPath, next);
    console.log(`wrote ${path.relative(webRoot, outPath)}`);
  }
}
