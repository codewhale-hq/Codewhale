import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { captureCaption } from "./capture-caption";
import { RELEASED_CAPTURE_FRAMES, RELEASED_CAPTURE_META, type ReleasedCaptureFrame } from "./released-capture.generated";
import { TERMINAL_CAPTURE_FRAMES } from "./terminal-capture.generated";

const released = RELEASED_CAPTURE_FRAMES as Record<string, ReleasedCaptureFrame>;
const dir = new URL("./terminal-captures/released/", import.meta.url);
const provenance = JSON.parse(readFileSync(new URL("provenance.json", dir), "utf8")) as {
  binary: { version_line: string };
  frames: { id: string; file: string; columns: number; rows: number }[];
};
const visibleText = (frame: ReleasedCaptureFrame) => frame.lines.map((runs) => runs.map(([text]) => text).join(""));
const stripped = (file: string) =>
  readFileSync(new URL(file, dir), "utf8")
    .replace(/\n$/, "")
    .replace(/\x1b\[[0-9;]*m/g, "")
    .split("\n");

describe("capture caption", () => {
  it("gives development frames the development caption and never the released one", () => {
    const dev = Object.values(TERMINAL_CAPTURE_FRAMES);
    expect(dev.every((frame) => frame.provenance === "development")).toBe(true);
    expect(captureCaption(dev).caption).toBe("captionDevelopment");
  });

  it("gives a released task sequence that shows the test run the released caption", () => {
    const sequence = ["02-task-prompt", "03-task-diff", "04-task-approval", "05-task-result"].map((n) => released[`cols56/${n}`]);
    expect(captureCaption(sequence)).toMatchObject({ caption: "captionReleased", reason: null });
  });

  it("gives no caption under a frame that is not about the task or shows no test run", () => {
    expect(captureCaption([released["cols100/01-custom-form"]])).toMatchObject({ caption: null });
    expect(captureCaption([released["cols100/00-launch"]]).caption).toBeNull();
    expect(captureCaption([released["cols56/05-task-result"], released["cols56/01-custom-form"]]).caption).toBeNull();
    const reason = captureCaption([released["cols100/03-task-diff"], released["cols100/04-task-approval"]]);
    expect(reason.caption).toBeNull();
    expect(reason.reason).toContain("tests ran");
    expect(captureCaption([released["cols100/06-receipt.crop"]]).caption).toBeNull();
  });

  it("gives no caption to frames of both origins together or to none", () => {
    expect(captureCaption([TERMINAL_CAPTURE_FRAMES.home, released["cols56/05-task-result"]]).caption).toBeNull();
    expect(captureCaption([]).caption).toBeNull();
  });

  it("flags frames that lack the model name or the Ask marker", () => {
    const notes = captureCaption([released["cols100/01-custom-form"], released["cols100/05-task-result"], released["cols100/00-launch"]]).frames;
    expect(notes.map((n) => n.needsPostureNote)).toEqual([true, false, false]);
    expect(notes[0]).toMatchObject({ lacksAskPosture: true, lacksModelName: false });
  });
});

describe("released capture", () => {
  it("reads the version line from the provenance file", () => {
    expect(RELEASED_CAPTURE_META.versionLine).toBe(provenance.binary.version_line);
    for (const frame of Object.values(released)) {
      expect(frame.versionLine).toBe(provenance.binary.version_line);
      expect(frame.provenance).toBe("released");
    }
  });

  it("keeps every frame's text identical to its capture with the escape codes removed", () => {
    expect(Object.keys(released)).toEqual(provenance.frames.map((f) => f.id));
    for (const entry of provenance.frames) {
      const frame = released[entry.id];
      const text = visibleText(frame);
      expect(text).toHaveLength(entry.rows);
      text.forEach((row) => expect([...row]).toHaveLength(entry.columns));
      expect(text.map((row) => row.trimEnd()), entry.id).toEqual(stripped(entry.file).map((row) => row.trimEnd()));
    }
  });

  it("records what each frame shows to match its own text", () => {
    for (const frame of Object.values(released)) {
      const text = visibleText(frame).join("\n");
      expect(frame.shows.askPosture, frame.id).toBe(text.includes("● ask"));
      expect(frame.shows.modelName, frame.id).toBe(text.includes("scripted-demo"));
      expect(frame.shows.testRun, frame.id).toBe(/Ran \d+ tests? in [0-9.]+s/.test(text) && /\bOK\b/.test(text));
    }
  });
});
