"use client";

import { useEffect, useRef, useState } from "react";
import { WhalePose } from "@/components/whale-pose";
import { whaleStateLabels, type WhaleStateKey } from "@/lib/content/whale-states";
import {
  cueWhale,
  loadWhaleReference,
  onWhaleCue,
  WHALE_PERFORMANCE,
  type WhaleCue,
  type WhaleDirector,
  type WhaleReference,
} from "@/lib/whale-motion";

/**
 * The live v2 whale: the desktop app's own Director and rig, drawn on a
 * canvas. Until the reference loads (or if it fails) the static rest pose
 * stands in, so the hero never shows an empty box.
 *
 * Motion budget: 30 fps while visible, nothing while hidden or off screen.
 * Under prefers-reduced-motion the Director paints its poster pose once per
 * change and never runs a clock. The gaze follows the pointer only at rest,
 * and only with motion allowed.
 *
 * With `perform`, it acts out a short session on its own (WHALE_PERFORMANCE)
 * and names each phase in a word beneath it. A reader's choice elsewhere on
 * the page (a terminal view) takes over for a while, then the loop resumes.
 *
 * Known limits: one live whale per page (habitat gaze is module state in the
 * vendored reference), and cues are illustrative — this component observes
 * no real session.
 */
const CHOSEN_HOLD_MS = 15000;
/** Share of the canvas the 124-unit rig box fills; the rest is room for props. */
const INSET = 0.72;

export function WhaleLive({
  locale = "en",
  className = "",
  follow = true,
  perform = false,
  asleep = false,
}: {
  locale?: string;
  className?: string;
  follow?: boolean;
  perform?: boolean;
  asleep?: boolean;
}) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const asleepRef = useRef(asleep);
  const wakeRef = useRef<() => void>(() => {});
  const [ready, setReady] = useState(false);
  const [state, setState] = useState<WhaleStateKey>("rest");
  const labels = whaleStateLabels(locale);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    let cancelled = false;
    let frame = 0;
    let last = 0;
    let visible = true;
    let live: { reference: WhaleReference; director: WhaleDirector } | null = null;
    const motion = window.matchMedia("(prefers-reduced-motion: reduce)");
    const cleanups: (() => void)[] = [];

    const paint = () => {
      if (!live) return;
      const box = canvas.getBoundingClientRect();
      if (!box.width || !box.height) return;
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      const width = Math.round(box.width * dpr), height = Math.round(box.height * dpr);
      if (canvas.width !== width) canvas.width = width;
      if (canvas.height !== height) canvas.height = height;
      const ctx = canvas.getContext("2d");
      if (!ctx) return;
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, box.width, box.height);
      // The rig's 124-unit box holds the body; props (page, pad, wrench,
      // calves) reach past it, so the whale draws at INSET of the canvas.
      const fit = (Math.min(box.width, box.height) / 124) * INSET;
      ctx.translate(box.width / 2, box.height / 2);
      ctx.scale(fit, fit);
      live.reference.acting.render(ctx, live.director, {
        theme: "paper",
        size: Math.min(box.width, box.height),
        dpr,
        lod: 0,
        pose: follow ? live.reference.habitat.pose(live.director) : undefined,
      });
    };

    const tick = (now: number) => {
      frame = 0;
      if (!live || live.director.reduced || !visible || document.hidden || asleepRef.current) return;
      // Never replay time spent hidden: clamp the step like the app does.
      const dt = last ? Math.min((now - last) / 1000, 0.1) : 1 / 30;
      if (now - last >= 1000 / 31) {
        last = now;
        live.director.step(dt);
        paint();
      }
      frame = requestAnimationFrame(tick);
    };
    const schedule = () => {
      if (!frame && live && !live.director.reduced && visible && !document.hidden && !asleepRef.current) {
        last = 0;
        frame = requestAnimationFrame(tick);
      }
    };
    wakeRef.current = schedule;

    loadWhaleReference().then((reference) => {
      if (cancelled) return;
      const director = new reference.acting.Director({ seed: 11, reduced: motion.matches });
      live = { reference, director };
      setReady(true);
      paint();
      schedule();

      let turn = 0;
      let step = 0;
      let timer: ReturnType<typeof setTimeout> | undefined;
      const autoplay = (delay: number) => {
        clearTimeout(timer);
        if (!perform || motion.matches) return;
        timer = setTimeout(() => {
          const { cue, seconds } = WHALE_PERFORMANCE[step % WHALE_PERFORMANCE.length];
          step += 1;
          cueWhale(cue);
          autoplay(seconds * 1000);
        }, delay);
      };
      const apply = (cue: WhaleCue) => {
        if (cue.presence === "Done") turn += 1;
        director.set(
          cue.presence,
          cue.kind ? { kind: cue.kind, observed: true, parallel: cue.parallel } : null,
          cue.presence === "Done"
            ? { freshness: "Live", turnId: `site-${turn}`, status: "completed" }
            : { freshness: "Live" },
        );
        setState(cue.label);
        paint();
        schedule();
        // A reader's choice holds the stage, then the session starts over.
        if (cue.chosen) { step = 0; autoplay(CHOSEN_HOLD_MS); }
      };
      cleanups.push(onWhaleCue(apply));
      autoplay(1200);
      cleanups.push(() => clearTimeout(timer));

      const onMotion = () => { director.setReduced(motion.matches); paint(); schedule(); autoplay(1200); };
      motion.addEventListener("change", onMotion);
      cleanups.push(() => motion.removeEventListener("change", onMotion));
    }).catch(() => { /* The static pose stays in place. */ });

    const onVisibility = () => schedule();
    document.addEventListener("visibilitychange", onVisibility);
    cleanups.push(() => document.removeEventListener("visibilitychange", onVisibility));

    if (typeof IntersectionObserver !== "undefined") {
      const io = new IntersectionObserver((entries) => {
        visible = entries.some((entry) => entry.isIntersecting);
        schedule();
      });
      io.observe(canvas);
      cleanups.push(() => io.disconnect());
    }
    const resize = new ResizeObserver(() => paint());
    resize.observe(canvas);
    cleanups.push(() => resize.disconnect());

    if (follow) {
      // The whale glances toward the pointer anywhere in the hero, not only
      // over its own box. Passive: scrolling and clicks are untouched.
      const move = (event: PointerEvent) => {
        if (!live || motion.matches) return;
        const box = canvas.getBoundingClientRect();
        const fit = (Math.min(box.width, box.height) / 124) * INSET;
        if (!fit) return;
        live.reference.habitat.observe(
          (event.clientX - box.left - box.width / 2) / fit,
          (event.clientY - box.top - box.height / 2) / fit,
        );
      };
      const leave = () => live?.reference.habitat.leave();
      window.addEventListener("pointermove", move, { passive: true });
      document.documentElement.addEventListener("pointerleave", leave, { passive: true });
      cleanups.push(() => {
        window.removeEventListener("pointermove", move);
        document.documentElement.removeEventListener("pointerleave", leave);
      });
    }

    return () => {
      cancelled = true;
      if (frame) cancelAnimationFrame(frame);
      cleanups.forEach((cleanup) => cleanup());
    };
  }, [follow, perform]);

  useEffect(() => {
    asleepRef.current = asleep;
    if (!asleep) wakeRef.current();
  }, [asleep]);

  return (
    <div className={`whale-live ${className}`.trim()} data-ready={ready || undefined}>
      <WhalePose pose="rest" priority className="whale-live-poster" />
      <canvas ref={canvasRef} className="whale-live-canvas" aria-hidden="true" />
      {perform && (
        // Decorative narration: hidden from assistive technology so the loop
        // never announces itself every few seconds.
        <p className="whale-live-state" data-state={state} aria-hidden="true">
          <span className="whale-live-dot" />
          {labels[state]}
        </p>
      )}
    </div>
  );
}
