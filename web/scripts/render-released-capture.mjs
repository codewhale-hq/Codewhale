#!/usr/bin/env node
import { readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { hex, paletteColor } from "./terminal-palette.mjs";

const webRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const inputDir = path.join(webRoot, "lib", "terminal-captures", "released");
const outPath = path.join(webRoot, "lib", "released-capture.generated.ts");

const DEFAULT_FG = paletteColor(252);
const DEFAULT_BG = paletteColor(234);

const ESCAPE_OR_CONTROL = /\x1b\[([0-9;]*)m|[\x00-\x1f\x7f]/gu;

function freshState() {
  return { fg: null, bg: null, bold: false, dim: false, italic: false, underline: false, strike: false, reverse: false };
}

function applySgr(state, params, where) {
  const p = params.length ? params : [0];
  for (let i = 0; i < p.length; i++) {
    const c = p[i];
    if (c === 0) Object.assign(state, freshState());
    else if (c === 1) state.bold = true;
    else if (c === 2) state.dim = true;
    else if (c === 3) state.italic = true;
    else if (c === 4) state.underline = true;
    else if (c === 7) state.reverse = true;
    else if (c === 9) state.strike = true;
    else if (c === 22) state.bold = state.dim = false;
    else if (c === 23) state.italic = false;
    else if (c === 24) state.underline = false;
    else if (c === 27) state.reverse = false;
    else if (c === 29) state.strike = false;
    else if (c >= 30 && c <= 37) state.fg = paletteColor(c - 30);
    else if (c >= 90 && c <= 97) state.fg = paletteColor(c - 90 + 8);
    else if (c >= 40 && c <= 47) state.bg = paletteColor(c - 40);
    else if (c >= 100 && c <= 107) state.bg = paletteColor(c - 100 + 8);
    else if (c === 39) state.fg = null;
    else if (c === 49) state.bg = null;
    else if ((c === 38 || c === 48) && p[i + 1] === 5 && p[i + 2] <= 255) {
      state[c === 38 ? "fg" : "bg"] = paletteColor(p[i + 2]);
      i += 2;
    } else if ((c === 38 || c === 48) && p[i + 1] === 2 && p.slice(i + 2, i + 5).every((v) => v <= 255) && i + 4 < p.length) {
      state[c === 38 ? "fg" : "bg"] = hex(p[i + 2], p[i + 3], p[i + 4]);
      i += 4;
    } else {
      throw new Error(`${where}: unhandled SGR ${params.join(";")}`);
    }
  }
}

function styleOf(state) {
  let fg = state.fg ?? DEFAULT_FG;
  let bg = state.bg ?? DEFAULT_BG;
  if (state.reverse) [fg, bg] = [bg, fg];
  return {
    fg,
    bg,
    ...(state.bold ? { bold: true } : {}),
    ...(state.italic ? { italic: true } : {}),
    ...(state.dim ? { dim: true } : {}),
    ...(state.underline ? { underline: true } : {}),
    ...(state.strike ? { strike: true } : {}),
  };
}

function parse(ansi, columns, rows, where) {
  const state = freshState();
  const input = ansi.endsWith("\n") ? ansi.slice(0, -1) : ansi;
  const cellRows = input.split("\n").map((line, row) => {
    const cells = [];
    let last = 0;
    for (const match of line.matchAll(ESCAPE_OR_CONTROL)) {
      for (const ch of line.slice(last, match.index)) cells.push([ch, styleOf(state)]);
      if (match[1] === undefined) throw new Error(`${where} row ${row + 1}: unhandled control ${JSON.stringify(match[0])}`);
      applySgr(state, match[1] === "" ? [] : match[1].split(";").map(Number), `${where} row ${row + 1}`);
      last = match.index + match[0].length;
    }
    for (const ch of line.slice(last)) cells.push([ch, styleOf(state)]);
    if (cells.length > columns) throw new Error(`${where} row ${row + 1}: ${cells.length} cells, wider than ${columns}`);
    const blank = styleOf(freshState());
    while (cells.length < columns) cells.push([" ", blank]);
    return cells;
  });
  if (cellRows.length !== rows) throw new Error(`${where}: ${cellRows.length} rows, provenance says ${rows}`);
  return cellRows;
}

function compact(cellRows) {
  const styles = [];
  const styleIndex = new Map();
  const indexOf = (style) => {
    const key = JSON.stringify(style);
    if (!styleIndex.has(key)) {
      styleIndex.set(key, styles.length);
      styles.push(style);
    }
    return styleIndex.get(key);
  };
  const lines = cellRows.map((cells) => {
    const runs = [];
    for (const [ch, style] of cells) {
      const last = runs.at(-1);
      if (ch === " ") {
        const visible = (s) => `${s.bg}|${s.underline ? 1 : 0}|${s.strike ? 1 : 0}|${s.dim ? 1 : 0}`;
        if (last && visible(styles[last[1]]) === visible(style)) {
          last[0] += ch;
          continue;
        }
        const plain = { fg: DEFAULT_FG, bg: style.bg, ...(style.dim ? { dim: true } : {}), ...(style.underline ? { underline: true } : {}), ...(style.strike ? { strike: true } : {}) };
        runs.push([ch, indexOf(plain)]);
        continue;
      }
      const index = indexOf(style);
      if (last && last[1] === index) last[0] += ch;
      else runs.push([ch, index]);
    }
    return runs;
  });
  return { styles, lines };
}

function strip(ansi) {
  return ansi.replace(ESCAPE_OR_CONTROL, (m, sgr) => {
    if (sgr === undefined && m !== "\n") throw new Error(`unhandled control ${JSON.stringify(m)}`);
    return sgr === undefined ? m : "";
  });
}

function generate() {
  const provenance = JSON.parse(readFileSync(path.join(inputDir, "provenance.json"), "utf8"));
  const versionLine = provenance.binary.version_line;
  const raw = new Map();
  const frames = {};
  for (const entry of provenance.frames) {
    const where = entry.file;
    const ansi = readFileSync(path.join(inputDir, entry.file), "utf8");
    let cellRows;
    if (entry.crop_of) {
      const parent = raw.get(entry.crop_of);
      if (!parent) throw new Error(`${where}: crop_of ${entry.crop_of} must be listed before its crop`);
      const { first, last } = entry.crop_rows;
      cellRows = parent.cellRows.slice(first - 1, last);
      const own = strip(ansi).replace(/\n$/, "").split("\n");
      const sliced = cellRows.map((cells) => cells.map(([ch]) => ch).join(""));
      const trimmed = (rows) => rows.map((row) => row.replace(/ +$/, ""));
      if (JSON.stringify(trimmed(own)) !== JSON.stringify(trimmed(sliced))) {
        throw new Error(`${where}: is not rows ${first}-${last} of ${entry.crop_of}`);
      }
      if (cellRows.length !== entry.rows) throw new Error(`${where}: ${cellRows.length} rows, provenance says ${entry.rows}`);
    } else {
      cellRows = parse(ansi, entry.columns, entry.rows, where);
      raw.set(entry.id, { cellRows });
    }
    const { styles, lines } = compact(cellRows);
    frames[entry.id] = {
      id: entry.id,
      file: `web/lib/terminal-captures/released/${entry.file}`,
      state: entry.shows,
      provenance: "released",
      versionLine,
      cols: entry.columns,
      rows: entry.rows,
      shows: {
        testRun: entry.shows_test_run,
        askPosture: entry.shows_ask_posture,
        modelName: entry.shows_model_name,
        modelConnected: entry.model_connected,
        taskSession: entry.in_task_session,
      },
      cropOf: entry.crop_of ?? null,
      cropRows: entry.crop_rows ? [entry.crop_rows.first, entry.crop_rows.last] : null,
      styles,
      lines,
    };
  }
  const meta = {
    capture: provenance.capture,
    versionLine,
    releaseTag: provenance.binary.release_tag,
    asset: provenance.binary.asset,
    releaseChecksumLine: provenance.binary.release_checksum_line,
    defaultFg: DEFAULT_FG,
    defaultBg: DEFAULT_BG,
  };
  return `// GENERATED by web/scripts/render-released-capture.mjs — do not edit.
// Source: web/lib/terminal-captures/released/ (${versionLine}, tmux capture-pane -p -e).
import type { TerminalCaptureFrame } from "./terminal-capture.generated";

export type ReleasedCaptureFrame = Omit<TerminalCaptureFrame, "provenance"> & {
  id: string;
  provenance: "released";
  versionLine: string;
  shows: {
    testRun: boolean;
    askPosture: boolean;
    modelName: boolean;
    modelConnected: boolean;
    taskSession: boolean;
  };
  cropOf: string | null;
  cropRows: [number, number] | null;
};

export const RELEASED_CAPTURE_META = ${JSON.stringify(meta, null, 2)} as const;

export const RELEASED_CAPTURE_FRAMES = ${JSON.stringify(frames)} as const satisfies Record<string, ReleasedCaptureFrame>;

export type ReleasedCaptureFrameId = keyof typeof RELEASED_CAPTURE_FRAMES;
`;
}

if (process.argv.includes("--check")) {
  let current = "";
  try {
    current = readFileSync(outPath, "utf8");
  } catch {
  }
  if (current !== generate()) {
    console.error("released-capture.generated.ts is stale: run node scripts/render-released-capture.mjs");
    process.exit(1);
  }
  console.log("released-capture.generated.ts is current");
} else {
  writeFileSync(outPath, generate());
  console.log(`wrote ${path.relative(webRoot, outPath)}`);
}
