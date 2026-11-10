"use client";

import { useId, useState } from "react";
import { TerminalCapture } from "@/components/terminal-capture";
import { getNativeTerminalCopy, NATIVE_TERMINAL_VIEWS } from "@/lib/content/native-terminal";
import { TERMINAL_CAPTURE_FRAMES, type TerminalCaptureFrameId } from "@/lib/terminal-capture.generated";
import { cueWhale, type WhaleCue } from "@/lib/whale-motion";
import "./native-terminal-gallery.css";

// What the homepage whale acts out beside each captured view. Illustration
// only: it mirrors the screen the reader picked, not a running session.
const WHALE_CUES: Record<string, WhaleCue> = {
  home: { presence: "Idle", label: "rest", chosen: true },
  composer: { presence: "Listening", label: "listen", chosen: true },
  workbar: { presence: "Working", kind: "editing", label: "write", chosen: true },
  "workbar-fleet": { presence: "Working", kind: "delegating", parallel: 3, label: "pod", chosen: true },
  "provider-picker": { presence: "Working", kind: "network", label: "connect", chosen: true },
  help: { presence: "Thinking", label: "think", chosen: true },
};

function hasCapture(id: string): id is TerminalCaptureFrameId {
  return Object.prototype.hasOwnProperty.call(TERMINAL_CAPTURE_FRAMES, id);
}

// New captured views appear when their real frame is added to the generated
// module. A named view without a capture never becomes a website button.
const views = Object.keys(NATIVE_TERMINAL_VIEWS).flatMap((id) =>
  hasCapture(id) ? [{ id }] : [],
);

const PANEL_ROWS: Partial<Record<TerminalCaptureFrameId, readonly [number, number]>> = {
  "workbar-fleet": [27, 32],
  "provider-picker": [1, 13],
};

/** Website controls select real PTY captures; the terminal itself is unchanged. */
export function NativeTerminalGallery({
  locale,
  defaultFrame = "composer",
  label,
  regionLabel,
}: {
  locale: string;
  defaultFrame?: TerminalCaptureFrameId;
  label: string;
  regionLabel: string;
}) {
  const [frame, setFrame] = useState<TerminalCaptureFrameId>(defaultFrame);
  const [showFull, setShowFull] = useState(false);
  const captureId = useId();
  const selected = views.find((view) => view.id === frame) ?? views[0];
  if (!selected) return null;
  const copy = getNativeTerminalCopy(locale);
  const { label: viewLabel, description } = copy.views[selected.id];
  const panelRows = PANEL_ROWS[selected.id];
  const detail = Boolean(panelRows && !showFull);

  return (
    <div className="native-terminal-gallery">
      <div className="native-terminal-view" id={captureId} dir="ltr">
        <TerminalCapture
          frame={selected.id}
          label={selected.id === "home" ? label : `${viewLabel}. ${description}${detail ? ` ${copy.detailLabel}.` : ""}`}
          regionLabel={`${regionLabel} · ${viewLabel}`}
          rows={detail ? panelRows : undefined}
        />
      </div>
      <div className="native-terminal-controls" role="group" aria-label={copy.viewsLabel}>
        {views.map((view) => (
          <button
            key={view.id}
            type="button"
            className="native-terminal-button"
            aria-pressed={selected.id === view.id}
            aria-controls={captureId}
            onClick={() => {
              setFrame(view.id);
              setShowFull(false);
              // Only a reader's choice moves the whale; the page loads at rest.
              const cue = WHALE_CUES[view.id];
              if (cue) cueWhale(cue);
            }}
          >
            {copy.views[view.id].label}
          </button>
        ))}
      </div>
      <p className="native-terminal-description" aria-live="polite" aria-atomic="true">{description}</p>
      <div className="native-terminal-footer">
        {panelRows ? (
          <button type="button" className="btn btn-secondary btn-sm" aria-expanded={showFull} aria-controls={captureId} onClick={() => setShowFull(!showFull)}>
            {showFull ? copy.showDetail : copy.showFull}
          </button>
        ) : null}
        <span className="native-terminal-scroll-hint">{copy.scrollHint}</span>
      </div>
    </div>
  );
}
