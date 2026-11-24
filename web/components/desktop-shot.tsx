import DESKTOP_MEDIA from "@/data/desktop-media.json";

export type DesktopShotId = (typeof DESKTOP_MEDIA.shots)[number]["id"];

type Output = { width: number; height: number };
const outputs = DESKTOP_MEDIA.outputs as Record<string, Output>;
const base = `/${DESKTOP_MEDIA.outDir.replace(/^public\//, "")}`;
const widths = [...DESKTOP_MEDIA.widths].sort((a, b) => a - b);
const largest = widths[widths.length - 1];

export function DesktopShot({
  id,
  alt,
  sizes = "(max-width: 900px) 100vw, 1100px",
  eager = false,
}: {
  id: DesktopShotId;
  alt: string;
  sizes?: string;
  eager?: boolean;
}) {
  const size = outputs[`${id}-light-${largest}.webp`];
  return (
    <span className="desktop-shot">
      {(["light", "dark"] as const).map((theme) => {
        const set = (format: string) => widths.map((w) => `${base}/${id}-${theme}-${w}.${format} ${outputs[`${id}-${theme}-${w}.${format}`].width}w`).join(", ");
        return (
          <picture key={theme} className={`desktop-shot-${theme}`}>
            <source type="image/avif" srcSet={set("avif")} sizes={sizes} />
            <img
              src={`${base}/${id}-${theme}-${largest}.webp`}
              srcSet={set("webp")}
              sizes={sizes}
              alt={alt}
              width={size.width}
              height={size.height}
              loading={eager && theme === "light" ? "eager" : "lazy"}
              decoding="async"
            />
          </picture>
        );
      })}
    </span>
  );
}
