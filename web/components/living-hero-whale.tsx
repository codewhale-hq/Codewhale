"use client";

import { useEffect, useRef, useState } from "react";
import { WhaleLive } from "@/components/whale-live";
import { BEAD_RADIUS, BEADS, IGNITE, beadOffset, beadShine, isStar, lightsAt } from "@/lib/whale-lights";
import { loadWhaleReference, onWhaleCue } from "@/lib/whale-motion";

type Stage = "pending" | "mark" | "whale" | "lights";

const MARK_MS = 700;
const WHALE_MS = 2400;
const ASSEMBLE_MS = 900;
const LIGHTS_FPS = 12;
const LEAVE_MS = 600;
const CHOSEN_HOLD_MS = 15000;
const SEEN = "codewhale.living-mark";

export function LivingHeroWhale({ locale = "en" }: { locale?: string }) {
  const [stage, setStage] = useState<Stage>("pending");
  const [mark, setMark] = useState<string[]>([]);
  const [near, setNear] = useState(false);
  const [held, setHeld] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const beads = useRef<SVGCircleElement[]>([]);
  const leave = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const shown: Stage = stage === "lights" && (near || held) ? "whale" : stage;

  useEffect(() => {
    const motion = window.matchMedia("(prefers-reduced-motion: reduce)");
    let cancelled = false;
    const timers: ReturnType<typeof setTimeout>[] = [];
    const start = () => {
      timers.forEach(clearTimeout);
      timers.length = 0;
      if (motion.matches) {
        setStage("whale");
        return;
      }
      loadWhaleReference().then(({ rig }) => {
        if (cancelled || motion.matches) return;
        const parts = rig.build("mark", { curl: 0, fin: 0, yaw: 0 });
        setMark(parts.shapes
          .filter((shape) => shape.opacity > 0 && shape.role !== "hole" && shape.role !== "cutout")
          .map((shape) => [rig.pathString(shape.path), ...rig.holesFor(shape.id, parts.shapes, true).map(rig.pathString)].join(" ")));
        let seen = false;
        try {
          seen = window.sessionStorage.getItem(SEEN) === "1";
          window.sessionStorage.setItem(SEEN, "1");
        } catch {
          seen = false;
        }
        if (seen) {
          setStage("lights");
          return;
        }
        setStage("mark");
        timers.push(setTimeout(() => setStage("whale"), MARK_MS));
        timers.push(setTimeout(() => setStage("lights"), MARK_MS + WHALE_MS));
      }).catch(() => setStage("whale"));
    };
    start();
    motion.addEventListener("change", start);
    return () => {
      cancelled = true;
      timers.forEach(clearTimeout);
      motion.removeEventListener("change", start);
    };
  }, []);

  useEffect(() => {
    let hold: ReturnType<typeof setTimeout> | undefined;
    const off = onWhaleCue((cue) => {
      if (!cue.chosen) return;
      clearTimeout(hold);
      setHeld(true);
      hold = setTimeout(() => setHeld(false), CHOSEN_HOLD_MS);
    });
    return () => {
      off();
      clearTimeout(hold);
      clearTimeout(leave.current);
    };
  }, []);

  useEffect(() => {
    if (shown !== "lights") return;
    let frame = 0;
    let last = 0;
    let visible = true;
    const entered = performance.now();
    const paint = (now: number) => {
      const time = now / 1000;
      const front = Math.min(1, (now - entered) / ASSEMBLE_MS) * (1 + IGNITE);
      const { glow, crest } = lightsAt(time);
      beads.current.forEach((dot, index) => {
        const bead = BEADS[index];
        if (!bead) return;
        const lit = Math.max(0, Math.min(1, (front - bead.spine) / IGNITE));
        const shine = beadShine(index, crest);
        const [dx, dy] = beadOffset(index, time, shine);
        dot.setAttribute("cx", (bead.home[0] + dx).toFixed(2));
        dot.setAttribute("cy", (bead.home[1] + dy).toFixed(2));
        dot.style.opacity = (lit * (isStar(index) ? 0.95 : 0.5 + 0.5 * shine) * glow).toFixed(3);
      });
    };
    const tick = (now: number) => {
      frame = 0;
      if (!visible || document.hidden) return;
      if (now - last >= 1000 / LIGHTS_FPS) {
        last = now;
        paint(now);
      }
      frame = requestAnimationFrame(tick);
    };
    const wake = () => {
      if (!frame && visible && !document.hidden) frame = requestAnimationFrame(tick);
    };
    const observer = new IntersectionObserver((entries) => {
      visible = entries.some((entry) => entry.isIntersecting);
      wake();
    });
    if (root.current) observer.observe(root.current);
    document.addEventListener("visibilitychange", wake);
    wake();
    return () => {
      if (frame) cancelAnimationFrame(frame);
      observer.disconnect();
      document.removeEventListener("visibilitychange", wake);
    };
  }, [shown]);

  return (
    <div
      ref={root}
      className="living-whale"
      data-stage={shown}
      onPointerEnter={() => {
        clearTimeout(leave.current);
        setNear(true);
      }}
      onPointerLeave={() => {
        clearTimeout(leave.current);
        leave.current = setTimeout(() => setNear(false), LEAVE_MS);
      }}
    >
      <WhaleLive locale={locale} asleep={shown !== "whale"} />
      <svg className="living-whale-lights" viewBox="-62 -62 124 124" aria-hidden="true" focusable="false">
        <g className="living-whale-mark">
          {mark.map((d, index) => <path key={index} d={d} fillRule="evenodd" />)}
        </g>
        <g className="living-whale-beads">
          {BEADS.map((bead, index) => (
            <circle
              key={index}
              ref={(node) => { if (node) beads.current[index] = node; }}
              cx={bead.home[0]}
              cy={bead.home[1]}
              r={BEAD_RADIUS * (isStar(index) ? 1.25 : 1)}
              data-star={isStar(index) || undefined}
            />
          ))}
        </g>
      </svg>
    </div>
  );
}
