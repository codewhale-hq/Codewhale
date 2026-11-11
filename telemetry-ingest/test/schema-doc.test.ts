/**
 * The doc and the validator are welded.
 *
 * `docs/TELEMETRY.md` is a promise to users, and the Rust suite already welds it
 * to the client's structs. This file welds it to the *server*: the field names
 * and enum spellings are parsed back out of the markdown and compared for set
 * equality against `src/schema.ts`. A field published in the doc that this
 * endpoint would reject fails here; a field this endpoint would accept that the
 * doc never published fails here too. Neither can be fixed by editing only one
 * side.
 *
 * The parsers below mirror `crates/telemetry/src/tests.rs` on purpose — same
 * fenced-block extraction, same "identifier followed by a colon is a key" rule.
 */

import { describe, expect, it } from "vitest";

import {
  ARCHES,
  BATCH_MAX_BYTES,
  BATCH_MAX_EVENTS,
  COLD_START_BUCKETS,
  COUNTER_FIELDS,
  DURATION_BUCKETS,
  ENVELOPE_FIELDS,
  ERROR_FIELDS,
  EVENT_FIELDS,
  EVENT_NAMES,
  EXIT_CLASSES,
  INSTALL_KINDS,
  LIBCS,
  MAX_BODY_BYTES,
  OSES,
  PRODUCT_COUNTER_FIELDS,
  OPERATIONS_FIELDS,
  SCHEMA_VERSION,
  SESSION_SOURCES,
  SURFACES,
  TURN_WALL_FIELDS,
} from "../src/schema";
import { DOC, goldenBatch } from "./support";

/** Every fenced ```jsonc block, in document order. */
function jsoncBlocks(doc: string): string[] {
  const blocks: string[] = [];
  let current: string[] | null = null;
  for (const raw of doc.split("\n")) {
    const line = raw.trimEnd();
    if (current === null) {
      if (line.trim() === "```jsonc") current = [];
      continue;
    }
    if (line.trim() === "```") {
      blocks.push(current.join("\n"));
      current = null;
    } else {
      current.push(line);
    }
  }
  return blocks;
}

/**
 * Every `"name":` key in a block, nested objects included. Values are never
 * matched: a key is an identifier-shaped string followed by a colon, and no
 * value in these blocks has that shape.
 */
function documentedKeys(block: string): Set<string> {
  const keys = new Set<string>();
  const pattern = /"([a-z0-9_]+)"\s*:/g;
  for (const match of block.matchAll(pattern)) {
    keys.add(match[1]);
  }
  return keys;
}

/** The rows of the first markdown table after `anchor`, as `[first, rest]`. */
function tableAfter(doc: string, anchor: string): Array<[string, string]> {
  const lines = doc.split("\n");
  const start = lines.findIndex((line) => line.includes(anchor));
  expect(start, `anchor not found in docs/TELEMETRY.md: ${anchor}`).toBeGreaterThanOrEqual(0);

  const rows: Array<[string, string]> = [];
  let inTable = false;
  for (const line of lines.slice(start + 1)) {
    const trimmed = line.trim();
    if (!trimmed.startsWith("|")) {
      if (inTable) break;
      continue;
    }
    inTable = true;
    // Split on pipes that are not escaped as `\|` — escaped pipes are enum
    // separators inside a cell, not cell boundaries.
    const cells = trimmed
      .replace(/^\|/, "")
      .replace(/\|$/, "")
      .split(/(?<!\\)\|/)
      .map((cell) => cell.trim());
    const first = (cells[0] ?? "").replace(/`/g, "").trim();
    if (first === "" || /^[-:]+$/.test(first)) continue;
    if (first.toLowerCase() === "field" || first.toLowerCase() === "file") continue;
    rows.push([first, cells.slice(1).join(" | ")]);
  }
  return rows;
}

/**
 * The enum list a table row publishes: the first backticked run in the row's
 * remaining cells that contains a `\|` separator.
 */
function enumFromRow(rows: Array<[string, string]>, field: string): string[] {
  const row = rows.find(([name]) => name === field);
  expect(row, `no table row for ${field}`).toBeDefined();
  const rest = (row as [string, string])[1].replace(/\\\|/g, "|");
  for (const match of rest.matchAll(/`([^`]+)`/g)) {
    if (match[1].includes("|")) {
      return match[1].split("|").map((value) => value.trim());
    }
  }
  throw new Error(`no enum list published for ${field}`);
}

/** Every token the doc puts inside backticks, split on `|` and `,`. */
function documentedTokens(doc: string): Set<string> {
  const tokens = new Set<string>();
  const unescaped = doc.replace(/\\\|/g, "|");
  for (const match of unescaped.matchAll(/`([^`\n]+)`/g)) {
    for (const part of match[1].split(/[|,]/)) {
      tokens.add(part.trim());
    }
  }
  return tokens;
}

const BLOCKS = jsoncBlocks(DOC);
const TOKENS = documentedTokens(DOC);

describe("the doc and the validator publish the same fields", () => {
  it("finds one jsonc block for the envelope and one per event", () => {
    expect(BLOCKS).toHaveLength(1 + EVENT_NAMES.filter((name) => name !== "funnel_usage").length);
  });

  it("agrees on the envelope field set", () => {
    expect([...documentedKeys(BLOCKS[0])].sort()).toEqual(
      [...ENVELOPE_FIELDS].sort(),
    );
  });

  it("agrees on the install_or_upgrade field set", () => {
    expect([...documentedKeys(BLOCKS[1])].sort()).toEqual(
      [...EVENT_FIELDS.install_or_upgrade].sort(),
    );
  });

  it("agrees on the session_start field set", () => {
    expect([...documentedKeys(BLOCKS[2])].sort()).toEqual(
      [...EVENT_FIELDS.session_start].sort(),
    );
  });

  it("agrees on the session_end field set, nested objects included", () => {
    const expected = [
      ...EVENT_FIELDS.session_end,
      ...COUNTER_FIELDS,
      ...ERROR_FIELDS,
      ...TURN_WALL_FIELDS,
    ].sort();
    expect([...documentedKeys(BLOCKS[3])].sort()).toEqual(expected);
  });

  it("agrees on the panic field set", () => {
    expect([...documentedKeys(BLOCKS[4])].sort()).toEqual(
      [...EVENT_FIELDS.panic].sort(),
    );
  });

  it("agrees on aggregate product and operations fields", () => {
    expect([...documentedKeys(BLOCKS[5])].sort()).toEqual(
      [...EVENT_FIELDS.product_usage, ...PRODUCT_COUNTER_FIELDS].sort(),
    );
    expect([...documentedKeys(BLOCKS[6])].sort()).toEqual(
      ["event", ...OPERATIONS_FIELDS].sort(),
    );
  });

  it("agrees on the counters table, field for field", () => {
    const rows = tableAfter(DOC, "**`counters`** — closed field set");
    expect(rows.map(([name]) => name)).toEqual([...COUNTER_FIELDS]);
  });

  it("agrees on the errors table, field for field", () => {
    const rows = tableAfter(DOC, "**`errors`** — closed field set");
    expect(rows.map(([name]) => name)).toEqual([...ERROR_FIELDS]);
  });

  it("agrees on the envelope table, field for field", () => {
    const rows = tableAfter(DOC, "### Batch envelope");
    expect(rows.map(([name]) => name)).toEqual([...ENVELOPE_FIELDS]);
  });
});

describe("the doc and the validator publish the same enums", () => {
  const rows = tableAfter(DOC, "### Batch envelope");

  it.each([
    ["surface", SURFACES],
    ["os", OSES],
    ["arch", ARCHES],
    ["libc", LIBCS],
  ] as const)("agrees on the %s whitelist", (field, expected) => {
    expect(enumFromRow(rows, field).sort()).toEqual([...expected].sort());
  });

  it.each([
    ["install kind", INSTALL_KINDS],
    ["session source", SESSION_SOURCES],
    ["duration bucket", DURATION_BUCKETS],
    ["exit class", EXIT_CLASSES],
    ["cold start bucket", COLD_START_BUCKETS],
  ] as const)("publishes every %s value it accepts", (_label, values) => {
    for (const value of values) {
      expect(TOKENS.has(value), `${value} is not in docs/TELEMETRY.md`).toBe(
        true,
      );
    }
  });
});

describe("the doc and the validator publish the same limits", () => {
  it("agrees on SCHEMA_VERSION", () => {
    const match = DOC.match(/`SCHEMA_VERSION\s*=\s*(\d+)`/);
    expect(match).not.toBeNull();
    expect(Number((match as RegExpMatchArray)[1])).toBe(SCHEMA_VERSION);
    expect(goldenBatch().schema_version).toBe(1); // preserved legacy golden
  });

  it("agrees on the per-batch caps", () => {
    const match = DOC.match(/Capped at (\d+) events or (\d+) KiB per batch/);
    expect(match).not.toBeNull();
    const [, events, kib] = match as RegExpMatchArray;
    expect(Number(events)).toBe(BATCH_MAX_EVENTS);
    expect(Number(kib) * 1024).toBe(BATCH_MAX_BYTES);
  });

  it("caps the body above what a conforming client can send", () => {
    // 65536 event bytes + 199 commas + ~375 bytes of envelope keys and values.
    const worstCaseBody = BATCH_MAX_BYTES + (BATCH_MAX_EVENTS - 1) + 375;
    expect(MAX_BODY_BYTES).toBeGreaterThan(worstCaseBody);
    // …and not so far above it that the cap has stopped meaning anything.
    expect(MAX_BODY_BYTES).toBeLessThan(worstCaseBody * 2);
  });

  it("agrees that the disk rings are larger than one batch", () => {
    expect(DOC).toContain("rings capped at 512 records or\n256 KiB");
    expect(MAX_BODY_BYTES).toBeLessThan(256 * 1024);
  });
});

describe("the red line the doc publishes", () => {
  it("still says batches are IP-stripped at ingest", () => {
    // If this line ever leaves the doc, the deploy of this Worker needs a
    // second look before it ships.
    expect(DOC).toContain("Batches are **IP-stripped at ingest**");
    expect(DOC).toContain(
      "No IP is stored, logged, or joined to `install_id`",
    );
  });
});
