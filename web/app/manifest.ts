import type { MetadataRoute } from "next";
import { SITE_NAME } from "@/lib/page-meta";

/**
 * Web app manifest. The PWA icons are the desktop app icon tile (cream mark on
 * the near-black tile) rasterised from codewhale-design/whale/svg/icon-light.svg.
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
