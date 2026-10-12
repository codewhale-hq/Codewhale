import type { MetadataRoute } from "next";
import { SITE_NAME } from "@/lib/page-meta";

/**
 * Web app manifest. Icons are the brand mark itself — the C-shaped whale in
 * the logo gradient (#1E8FD8 -> #0B48BB) on a transparent ground, the
 * resting pose from public/whale/rest.svg rasterised to the sizes below
 * (app/icon.svg is the same drawing). No tile.
 */
export default function manifest(): MetadataRoute.Manifest {
  return {
    name: SITE_NAME,
    short_name: SITE_NAME,
    icons: [
      {
        src: "/icon-192.png",
        sizes: "192x192",
        type: "image/png",
      },
      {
        src: "/icon-512.png",
        sizes: "512x512",
        type: "image/png",
      },
    ],
    // The ocean floor (tokens-roles.css --ocean-floor): the dark ground the
    // site prefers, so the splash and chrome sit in the same water.
    theme_color: "#142638",
    background_color: "#142638",
    display: "standalone",
  };
}
