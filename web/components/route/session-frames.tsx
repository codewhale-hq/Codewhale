"use client";

import { useId, useRef, useState, type KeyboardEvent, type ReactNode } from "react";

export interface SessionFrame {
  key: string;
  name: string;
  wide: ReactNode;
  narrow: ReactNode;
  note: boolean;
}

export function SessionFrames({
  frames,
  initial,
  label,
  caption,
  note,
}: {
  frames: SessionFrame[];
  initial: number;
  label: string;
  caption: string | null;
  note: string;
}) {
  const [selected, setSelected] = useState(initial);
  const tabs = useRef<(HTMLButtonElement | null)[]>([]);
  const id = useId();

  const move = (event: KeyboardEvent<HTMLDivElement>) => {
    const rtl = getComputedStyle(event.currentTarget).direction === "rtl";
    const last = frames.length - 1;
    const step: Record<string, number> = {
      ArrowRight: rtl ? -1 : 1,
      ArrowLeft: rtl ? 1 : -1,
      ArrowDown: 1,
      ArrowUp: -1,
    };
    let next: number;
    if (event.key === "Home") next = 0;
    else if (event.key === "End") next = last;
    else if (event.key in step) next = (selected + step[event.key] + frames.length) % frames.length;
    else return;
    event.preventDefault();
    setSelected(next);
    tabs.current[next]?.focus();
  };

  return (
    <figure className="home-session">
      <div className="home-session-tabs" role="tablist" aria-label={label} onKeyDown={move}>
        {frames.map((frame, index) => (
          <button
            key={frame.key}
            ref={(node) => {
              tabs.current[index] = node;
            }}
            type="button"
            role="tab"
            id={`${id}-tab-${index}`}
            aria-selected={index === selected}
            aria-controls={`${id}-panel-${index}`}
            tabIndex={index === selected ? 0 : -1}
            onClick={() => setSelected(index)}
          >
            {frame.name}
          </button>
        ))}
      </div>
      {frames.map((frame, index) => (
        <div
          key={frame.key}
          className="home-frame home-session-panel"
          role="tabpanel"
          id={`${id}-panel-${index}`}
          aria-labelledby={`${id}-tab-${index}`}
          hidden={index !== selected}
        >
          <div className="home-only-wide">{frame.wide}</div>
          <div className="home-only-narrow">{frame.narrow}</div>
        </div>
      ))}
      <figcaption className="home-session-caption">
        {caption ? <p className="home-caption">{caption}</p> : null}
        <p className="home-caption" hidden={!frames[selected].note}>
          {note}
        </p>
      </figcaption>
    </figure>
  );
}
