"use client";

import { useEffect, useRef, useState } from "react";
import drones from "@/vendor/whale-lights/whale-drones.json";

type Stage = "mark" | "assembling" | "rest";
const MARK_MS = 650;
const ASSEMBLE_MS = 960;
const REPEAT_MS = 720;
const FRAME_MS = 1000 / 15;
const SEEN = "codewhale.living-mark";
const TAU = 2 * Math.PI;

export function LivingHeroWhale({ locale = "en" }: { locale?: string }) {
  const [stage, setStage] = useState<Stage>("mark");
  const root = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const cue = useRef(-Infinity);

  useEffect(() => {
    const element = root.current;
    const surface = canvas.current;
    const context = surface?.getContext("2d");
    if (!element || !surface || !context) return;
    const motion = window.matchMedia("(prefers-reduced-motion: reduce)");
    let seen = false;
    try { seen = sessionStorage.getItem(SEEN) === "1"; } catch {}
    let current: Stage = motion.matches ? "rest" : seen ? "assembling" : "mark";
    setStage(current);
    let elapsed = motion.matches ? MARK_MS + ASSEMBLE_MS : 0;
    let previous = 0;
    let painted = 0;
    let frame = 0;
    let visible = true;
    let focused = document.hasFocus();
    let blurAt = focused ? Infinity : performance.now();
    let blurTimer: ReturnType<typeof setTimeout> | undefined;
    let density = drones.launch[0];
    let size = 0;
    let body = "";
    let throat = "";
    const points: { x: number; y: number; kind: string; spine: number }[] = [];
    const hold = seen ? 0 : MARK_MS;
    const duration = seen ? REPEAT_MS : ASSEMBLE_MS;
    const updateStage = (next: Stage) => {
      if (next === current) return;
      if (next === "rest") {
        try { sessionStorage.setItem(SEEN, "1"); } catch {}
      }
      current = next;
      setStage(next);
    };
    const paint = () => {
      context.clearRect(0, 0, size, size);
      if (current === "mark") return;
      const still = motion.matches;
      const progress = still ? 1 : Math.min(1, Math.max(0, (elapsed - hold) / duration));
      const time = still ? 0 : elapsed / 1000;
      const glow = still ? 1 : 0.87 + 0.13 * Math.sin(TAU * time / 4.2);
      const hovered = !still && elapsed - cue.current < 300;
      const crest = !still && elapsed - cue.current < 1200 ? (elapsed - cue.current) / 1200 : (time / 6.1) % 1;
      const scale = size / 124;
      const radius = density.pitch * drones.radius * scale;
      for (let index = 0; index < points.length; index += 1) {
        const point = points[index];
        const sweep = seen ? Math.min(1, Math.hypot(point.x, point.y) / 60) : point.spine;
        const lit = Math.max(0, Math.min(1, (progress - sweep * 0.85) / 0.15));
        if (!lit) continue;
        const shine = still ? 0 : Math.max(0, 1 - Math.abs(point.spine - crest) / 0.12);
        const hover = still ? 0 : density.pitch * 0.12;
        const dx = hover * Math.sin(time + index * 1.7);
        const dy = hover * Math.cos(time * 0.8 + index * 2.3);
        context.globalAlpha = lit * Math.min(1, (0.7 + 0.2 * shine + (hovered ? 0.1 : 0)) * glow);
        context.fillStyle = point.kind === "t" || point.kind === "p" ? throat : body;
        context.beginPath();
        context.arc((point.x + dx + 62) * scale, (point.y + dy + 62) * scale, radius, 0, TAU);
        context.fill();
      }
      context.globalAlpha = 1;
    };
    const active = () => visible && !document.hidden && (focused || performance.now() - blurAt < 2500);
    const tick = (now: number) => {
      frame = 0;
      if (!active()) { previous = 0; return; }
      if (previous) elapsed += now - previous;
      previous = now;
      if (motion.matches) {
        updateStage("rest");
        paint();
        previous = 0;
        return;
      }
      updateStage(elapsed < hold ? "mark" : elapsed < hold + duration ? "assembling" : "rest");
      if (current === "assembling" || now - painted >= FRAME_MS) {
        painted = now;
        paint();
      }
      frame = requestAnimationFrame(tick);
    };
    const wake = () => {
      if (!frame && active()) frame = requestAnimationFrame(tick);
    };
    const resize = () => {
      const bounds = surface.getBoundingClientRect();
      const dpr = window.devicePixelRatio || 1;
      size = Math.round(bounds.width * dpr);
      surface.width = size;
      surface.height = size;
      density = drones.launch[0];
      for (const candidate of drones.launch) {
        if (candidate.pitch * size / 124 >= drones.minPitchPx) density = candidate;
      }
      points.length = 0;
      density.rows.forEach((row, y) => [...row].forEach((kind, x) => {
        if (kind === "." || kind === "e") return;
        const px = density.origin[0] + (x + 0.5) * density.pitch;
        const py = density.origin[1] + (y + 0.5) * density.pitch;
        const angle = (Math.atan2(py, px) * 180 / Math.PI + 360) % 360;
        points.push({ x: px, y: py, kind, spine: Math.min(1, ((angle - 340 + 360) % 360) / 335) });
      }));
      const style = getComputedStyle(element);
      body = style.color;
      throat = getComputedStyle(surface).color;
      paint();
      wake();
    };
    const focus = () => { focused = true; clearTimeout(blurTimer); previous = 0; wake(); };
    const blur = () => {
      focused = false;
      blurAt = performance.now();
      blurTimer = setTimeout(() => { if (frame) cancelAnimationFrame(frame); frame = 0; previous = 0; }, 2500);
    };
    const visibility = () => {
      if (document.hidden) { if (frame) cancelAnimationFrame(frame); frame = 0; previous = 0; }
      else wake();
    };
    const preference = () => {
      if (motion.matches) { elapsed = Math.max(elapsed, hold + duration); updateStage("rest"); }
      paint();
      previous = 0;
      wake();
    };
    const intersection = new IntersectionObserver((entries) => {
      visible = entries.some(entry => entry.isIntersecting);
      if (!visible) { if (frame) cancelAnimationFrame(frame); frame = 0; previous = 0; }
      else wake();
    });
    const observer = new ResizeObserver(resize);
    const appearance = new MutationObserver(resize);
    intersection.observe(element);
    observer.observe(surface);
    appearance.observe(document.documentElement, { attributes: true, attributeFilter: ["class", "data-theme", "style"] });
    window.addEventListener("focus", focus);
    window.addEventListener("blur", blur);
    document.addEventListener("visibilitychange", visibility);
    motion.addEventListener("change", preference);
    const interaction = () => { cue.current = elapsed; wake(); };
    element.addEventListener("pointerenter", interaction);
    element.addEventListener("focus", interaction);
    resize();
    return () => {
      if (frame) cancelAnimationFrame(frame);
      clearTimeout(blurTimer);
      observer.disconnect();
      intersection.disconnect();
      appearance.disconnect();
      window.removeEventListener("focus", focus);
      window.removeEventListener("blur", blur);
      document.removeEventListener("visibilitychange", visibility);
      motion.removeEventListener("change", preference);
      element.removeEventListener("pointerenter", interaction);
      element.removeEventListener("focus", interaction);
    };
  }, []);

  return (
    <div ref={root} className="living-whale" data-stage={stage} lang={locale} aria-hidden="true">
      <span className="living-whale-mark" />
      <canvas ref={canvas} className="living-whale-lights" />
    </div>
  );
}
