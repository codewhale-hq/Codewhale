import { useId } from "react";

function Filters({ id }: { id: string }) {
  return (
    <defs>
      <filter id={`${id}-wide`} x="-20%" y="-40%" width="140%" height="180%" colorInterpolationFilters="sRGB">
        <feGaussianBlur stdDeviation="34" />
      </filter>
      <filter id={`${id}-mid`} x="-20%" y="-40%" width="140%" height="180%" colorInterpolationFilters="sRGB">
        <feGaussianBlur stdDeviation="20" />
      </filter>
      <filter id={`${id}-tight`} x="-20%" y="-40%" width="140%" height="180%" colorInterpolationFilters="sRGB">
        <feGaussianBlur stdDeviation="11" />
      </filter>
      <filter id={`${id}-edge`} x="-20%" y="-40%" width="140%" height="180%" colorInterpolationFilters="sRGB">
        <feGaussianBlur stdDeviation="5" />
      </filter>
    </defs>
  );
}

export function Strata() {
  const id = `strata-${useId()}`;
  return (
    <svg
      className="strata strata-hero"
      viewBox="0 0 1440 1000"
      preserveAspectRatio="xMidYMax slice"
      aria-hidden="true"
      focusable="false"
    >
      <Filters id={id} />
      <ellipse cx="1180" cy="160" rx="520" ry="200" fill="var(--strata-haze)" filter={`url(#${id}-wide)`} />
      <path
        d="M -240 690 C 160 640, 420 720, 660 570 C 880 430, 1040 280, 1240 210 C 1340 175, 1440 150, 1700 110 L 1700 1120 L -240 1120 Z"
        fill="var(--strata-1)"
        filter={`url(#${id}-wide)`}
      />
      <path
        d="M -240 800 C 140 770, 400 830, 620 700 C 830 575, 1000 440, 1200 370 C 1330 325, 1500 280, 1700 250 L 1700 1120 L -240 1120 Z"
        fill="var(--strata-2)"
        filter={`url(#${id}-mid)`}
      />
      <path
        d="M -240 890 C 200 860, 430 905, 650 810 C 870 715, 1060 580, 1260 520 C 1400 478, 1540 445, 1700 410 L 1700 1120 L -240 1120 Z"
        fill="var(--strata-3)"
        filter={`url(#${id}-tight)`}
      />
      <path
        d="M -240 965 C 220 945, 470 975, 710 910 C 910 855, 1110 740, 1310 680 C 1430 645, 1560 615, 1700 600 L 1700 1120 L -240 1120 Z"
        fill="var(--strata-4)"
        filter={`url(#${id}-edge)`}
      />
      <g fill="none" strokeLinecap="round">
        <path d="M 980 330 C 1100 285, 1200 255, 1340 215 C 1420 192, 1520 170, 1700 140" stroke="var(--strata-line)" strokeOpacity="0.5" strokeWidth="1.1" />
        <path d="M 760 640 C 900 560, 1040 470, 1200 405 C 1330 352, 1480 310, 1700 275" stroke="var(--strata-line)" strokeOpacity="0.36" strokeWidth="1" />
        <path d="M 620 860 C 840 780, 1040 660, 1240 590 C 1400 535, 1560 500, 1700 470" stroke="var(--strata-line)" strokeOpacity="0.3" strokeWidth="1" />
        <path d="M 420 960 C 640 930, 860 860, 1060 780 C 1240 708, 1420 650, 1700 600" stroke="var(--strata-thread)" strokeOpacity="0.45" strokeWidth="0.9" />
      </g>
    </svg>
  );
}
