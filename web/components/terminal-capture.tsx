import type { CSSProperties } from "react";
import {
  TERMINAL_CAPTURE_FRAMES,
  type TerminalCaptureFrame,
  type TerminalCaptureFrameId,
  type TerminalCaptureStyle,
} from "@/lib/terminal-capture.generated";

export type TerminalCaptureData = Pick<TerminalCaptureFrame, "file" | "cols"> & {
  styles: readonly TerminalCaptureStyle[];
  lines: readonly (readonly (readonly [string, number])[])[];
};

const BRAILLE = /[⠀-⣿]/;

function runStyle(style: TerminalCaptureStyle): CSSProperties {
  return {
    color: style.fg,
    backgroundColor: style.bg,
    fontWeight: style.bold ? 700 : undefined,
    fontStyle: style.italic ? "italic" : undefined,
    opacity: style.dim ? 0.6 : undefined,
    textDecoration: style.underline ? "underline" : style.strike ? "line-through" : undefined,
  };
}

/**
 * A real terminal frame as live HTML text: the PTY cells the terminal
 * emulator parsed (lib/terminal-captures, compacted by
 * scripts/render-terminal-capture.mjs), drawn as styled runs in the system
 * monospace font. Nothing is retouched or staged.
 *
 * The frame or a contiguous detail scales with the frame's width and the
 * grid's column count, staying live, selectable text.
 * Assistive technology gets one summary (`label`); the glyph-by-glyph grid
 * is presentation. Braille cells (the whale mark) are pinned to one column
 * each, because system monospace fonts draw them from a fallback font.
 */
export function TerminalCapture({
  frame = "home",
  label,
  regionLabel,
  rows,
  data: given,
}: {
  frame?: TerminalCaptureFrameId;
  data?: TerminalCaptureData;
  label: string;
  regionLabel: string;
  rows?: readonly [number, number];
}) {
  const data: TerminalCaptureData = given ?? TERMINAL_CAPTURE_FRAMES[frame];
  const start = rows ? Math.max(0, Math.min(data.lines.length - 1, rows[0])) : 0;
  const end = rows ? Math.max(start + 1, Math.min(data.lines.length, rows[1])) : data.lines.length;
  const lines = data.lines.slice(start, end);
  const styles = data.styles;
  // The padding around the grid continues the frame's own top and bottom rows.
  const top = styles[lines[0][0][1]].bg;
  const bottom = styles[lines[lines.length - 1][0][1]].bg;
  return (
    <div className="term-capture-scroll" dir="ltr" lang="en" role="region" aria-label={regionLabel} tabIndex={0}>
      <pre
        className="term-capture"
        role="img"
        aria-label={label}
        data-capture-file={data.file}
        data-capture-row-start={start + 1}
        data-capture-row-end={end}
        style={{ background: `linear-gradient(${top}, ${bottom})`, ["--cols" as string]: data.cols } as CSSProperties}
      >
        {lines.map((runs, row) => (
          <span key={row} className="term-capture-row">
            {runs.map(([text, index], run) => (
              <span key={run} style={runStyle(styles[index])}>
                {BRAILLE.test(text)
                  ? [...text].map((ch, i) =>
                      BRAILLE.test(ch) ? (
                        <span key={i} className="term-capture-cell">
                          {ch}
                        </span>
                      ) : (
                        ch
                      ),
                    )
                  : text}
              </span>
            ))}
          </span>
        ))}
      </pre>
    </div>
  );
}
