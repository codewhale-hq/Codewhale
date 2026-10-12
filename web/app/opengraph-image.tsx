import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { ImageResponse } from "next/og";
import { IDENTITY_PHRASE, OG_ALT } from "@/lib/page-meta";

export const alt = OG_ALT;
export const size = { width: 1200, height: 630 };
export const contentType = "image/png";

// Read the brand SVGs when the image is generated, never at import time. This
// route is prerendered at build on Node, but its module is still evaluated
// inside the Cloudflare Worker whenever a page regenerates, and the Workers
// Node shim throws on fs.readFile. A top-level read here made every ISR
// regeneration on the site fail with a 500 and serve its build snapshot forever.
async function brandSvgs(): Promise<[string, string]> {
  const [mark, wordmark] = await Promise.all([
    readFile(join(process.cwd(), "public/whale/rest.svg")),
    readFile(join(process.cwd(), "public/brand/wordmark-inverted.svg")),
  ]);
  return [mark.toString(), wordmark.toString()];
}

export default async function OpengraphImage() {
  const [markSvg, wordmarkSvg] = await brandSvgs();
  const markDataUrl = `data:image/svg+xml;base64,${Buffer.from(markSvg).toString("base64")}`;
  const wordmarkDataUrl = `data:image/svg+xml;base64,${Buffer.from(wordmarkSvg).toString("base64")}`;

  // The ocean: navy rising into the logo's blue, the whale in the logo
  // gradient, the inverted wordmark, and the identity phrase once — the
  // wordmark is the name, so no second "Codewhale" heading.
  return new ImageResponse(
    (
      <div
        style={{
          width: "100%",
          height: "100%",
          display: "flex",
          flexDirection: "column",
          alignItems: "center",
          justifyContent: "center",
          gap: 34,
          backgroundImage: "linear-gradient(180deg, #142352 0%, #142638 55%, #0b48bb 100%)",
          fontFamily: "sans-serif",
        }}
      >
        <img src={markDataUrl} width={240} height={240} alt="" />
        {/* The family wordmark is 1024x160 (6.4:1). */}
        <img src={wordmarkDataUrl} width={532} height={83} alt="Codewhale" />
        <div style={{ display: "flex", fontSize: 30, color: "#e6eef7", marginTop: 14 }}>
          {IDENTITY_PHRASE}
        </div>
      </div>
    ),
    { ...size },
  );
}
