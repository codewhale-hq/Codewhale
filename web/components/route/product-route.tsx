import Link from "next/link";
import { useId, type CSSProperties, type ReactNode } from "react";
import type { ProductMapId, ProductMapState } from "@/lib/content/product-map";
import { InlineCode } from "./inline-code";
import { RouteWhale } from "./route-whale";

export interface RoutePart {
  id: ProductMapId;
  state: ProductMapState;
  name: string;
  stateWord: string;
  sentence: string | null;
  href: string | null;
}

type MarkState = ProductMapState | "engine";

interface Geometry {
  stops: { x: number; state: MarkState }[];
  today: number;
  solidEnd: number;
}

const pct = (value: number) => `${(value * 100).toFixed(3)}%`;
const vars = (values: Record<string, string | number>) => values as CSSProperties;

function releasedCount(parts: readonly RoutePart[]): number {
  const first = parts.findIndex((part) => part.state !== "released");
  return first < 0 ? parts.length : first;
}

function geometryFrom(xs: readonly number[], parts: readonly RoutePart[], split: number): Geometry {
  const today = releasedCount(parts);
  const next = today + 1 < xs.length ? xs[today + 1] : 1;
  const solidEnd = xs[today] + (next - xs[today]) * split;
  return {
    stops: xs.map((x, index) => ({ x, state: index === 0 ? "engine" : parts[index - 1].state })),
    today: solidEnd,
    solidEnd: today === parts.length ? 1 : solidEnd,
  };
}

function mapGeometry(parts: readonly RoutePart[]): Geometry {
  const count = parts.length + 1;
  return geometryFrom(Array.from({ length: count }, (_, index) => index / count), parts, 0.5);
}

function StateMark({ state }: { state: MarkState }) {
  return (
    <svg className="road-mark" data-state={state} viewBox="-6 -6 12 12" aria-hidden="true" focusable="false">
      <MarkShape state={state} />
    </svg>
  );
}

function MarkShape({ state }: { state: MarkState }) {
  if (state === "engine" || state === "released") return <circle r="5" className="road-mark-fill" />;
  if (state === "developing") {
    return (
      <>
        <circle r="4.25" className="road-mark-ring" />
        <path d="M0 -4.25a4.25 4.25 0 0 0 0 8.5z" className="road-mark-fill" />
      </>
    );
  }
  if (state === "inviteOnly") {
    return (
      <>
        <circle r="4.25" className="road-mark-ring" />
        <circle r="1.75" className="road-mark-fill" />
      </>
    );
  }
  return <circle r="4.25" className="road-mark-ring road-mark-dashed" />;
}

function Rail({ geometry }: { geometry: Geometry }) {
  const id = useId().replace(/[^a-z0-9]/gi, "");
  const mask = `road-mask-${id}`;
  const fade = `road-fade-${id}`;
  const { stops, solidEnd } = geometry;
  const tail = stops[stops.length - 1];
  const lines: ReactNode[] = [];
  stops.forEach((stop, index) => {
    const end = index + 1 < stops.length ? stops[index + 1].x : 1;
    const into: MarkState = index + 1 < stops.length ? stops[index + 1].state : stop.state;
    const line = (from: number, to: number, state: MarkState, key: string) => (
      <line
        key={key}
        className="road-line"
        data-line={state === "engine" ? "released" : state}
        x1={pct(from)}
        x2={pct(to)}
        y1="50%"
        y2="50%"
        pathLength={state === "released" || state === "engine" ? 1 : undefined}
        style={vars(to === 1 ? { "--i": index, stroke: `url(#${fade})` } : { "--i": index })}
      />
    );
    if (end <= solidEnd) lines.push(line(stop.x, end, "released", `s${index}`));
    else if (stop.x < solidEnd) {
      lines.push(line(stop.x, solidEnd, "released", `s${index}`));
      lines.push(line(solidEnd, end, into, `l${index}`));
    } else lines.push(line(stop.x, end, into, `l${index}`));
  });
  return (
    <div className="road-rail" aria-hidden="true" style={vars({ "--today": pct(geometry.today) })}>
      <svg className="road-drawing" focusable="false">
        <linearGradient id={fade} className="road-fade" data-state={tail.state} gradientUnits="userSpaceOnUse" x1={pct(Math.max(tail.x, 0.92))} x2="100%">
          <stop className="road-fade-near" />
          <stop className="road-fade-far" offset="1" />
        </linearGradient>
        <mask id={mask} maskUnits="userSpaceOnUse" x="0" y="0" width="100%" height="100%">
          <rect width="100%" height="100%" fill="#fff" />
          {stops.map((stop, index) =>
            stop.state === "engine" || stop.state === "released" ? null : <circle key={index} cx={pct(stop.x)} cy="50%" r="7.5" fill="#000" />,
          )}
        </mask>
        <g mask={`url(#${mask})`}>{lines}</g>
        {stops.map((stop, index) => (
          <svg key={index} className="road-station" data-state={stop.state} x={pct(stop.x)} y="50%" width="1" height="1" overflow="visible" style={vars({ "--i": index })}>
            <g transform="scale(1.2)">
              <MarkShape state={stop.state} />
            </g>
          </svg>
        ))}
      </svg>
      <span className="road-today">
        <RouteWhale />
      </span>
    </div>
  );
}

function StateWord({ part }: { part: RoutePart }) {
  return (
    <span className="road-state" data-state={part.state}>
      <StateMark state={part.state} />
      {part.stateWord}
    </span>
  );
}

function PartName({ part, locale }: { part: RoutePart; locale: string }) {
  return part.href ? (
    <Link className="road-name" href={`/${locale}${part.href}`}>
      {part.name}
    </Link>
  ) : (
    <span className="road-name">{part.name}</span>
  );
}

export function RouteMap({ parts, engine, label, locale }: { parts: RoutePart[]; engine: string; label: string; locale: string }) {
  const treeRoot = useId();
  return (
    <>
      <div className="road road-map">
        <Rail geometry={mapGeometry(parts)} />
        <ol className="road-stops" aria-label={label} style={{ gridTemplateColumns: `repeat(${parts.length + 1}, minmax(0, 1fr))` }}>
          <li className="road-stop" data-state="engine">
            <span className="road-name">{engine}</span>
          </li>
          {parts.map((part) => (
            <li key={part.id} className="road-stop" data-state={part.state}>
              <PartName part={part} locale={locale} />{" "}
              <StateWord part={part} />{" "}
              {part.sentence ? (
                <span className="road-sentence">
                  <InlineCode text={part.sentence} />
                </span>
              ) : null}
            </li>
          ))}
        </ol>
      </div>
      <div className="road-tree">
        <p className="road-tree-root" id={treeRoot}>
          {engine}
        </p>
        <ul aria-labelledby={treeRoot}>
          {parts.map((part) => (
            <li key={part.id} data-state={part.state}>
              <span className="road-tree-line">
                <PartName part={part} locale={locale} />
                <span className="road-tree-leader" aria-hidden="true" />
                <StateWord part={part} />
              </span>
              {part.sentence ? (
                <span className="road-sentence">
                  <InlineCode text={part.sentence} />
                </span>
              ) : null}
            </li>
          ))}
        </ul>
      </div>
    </>
  );
}
