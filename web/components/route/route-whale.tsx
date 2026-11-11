import { useId } from "react";
import whale from "@/vendor/whale-b/whale-b.json";

const GRADIENT_PARTS = new Set(["flukes", "dorsal", "body", "flipper"]);

export function RouteWhale() {
  const gradient = `road-whale-${useId().replace(/[^a-z0-9]/gi, "")}`;
  const { x1, y1, x2, y2 } = whale.gradient;
  return (
    <svg className="road-whale" viewBox={whale.viewBox} aria-hidden="true" focusable="false">
      <defs>
        <linearGradient id={gradient} gradientUnits="userSpaceOnUse" x1={x1} y1={y1} x2={x2} y2={y2}>
          <stop className="road-whale-rise" />
          <stop className="road-whale-deep" offset="1" />
        </linearGradient>
      </defs>
      <g className="road-whale-swim">
        {whale.paths.map((path) => (
          <path
            key={path.id}
            d={path.d}
            className={`road-whale-${path.id}`}
            fill={GRADIENT_PARTS.has(path.id) ? `url(#${gradient})` : undefined}
          />
        ))}
      </g>
    </svg>
  );
}
