import type { Metadata } from "next";
import localFont from "next/font/local";
import { notFound } from "next/navigation";
import { Nav } from "@/components/nav";
import { Footer } from "@/components/footer";
import { UsageCounting } from "@/components/usage-counting";
import { BUILD_FACTS } from "@/lib/facts";
import { isValidLocale, localeDirection, locales } from "@/lib/i18n/config";
import { getChrome, getHome } from "@/lib/i18n/dictionaries";
import { serializeJsonLd } from "@/lib/json-ld";
import { buildPageMetadata } from "@/lib/page-meta";
import { buildSiteJsonLd } from "@/lib/site-schema";
import { metadata as notFoundMetadata } from "./not-found";
import "../globals.css";

// Shannon Sans is the one face, as in the GPUI app (`set_theme`). The pinned
// variable font is split by scripts/subset-shannon-sans.py into a Latin face,
// the only font preloaded, and an extended face (latin-ext, Greek, Cyrillic,
// Devanagari) the browser fetches only when a page contains one of its
// glyphs. The unicode ranges must match what that script prints. Code uses
// the system monospace stack (tokens-roles.css), so no mono face loads.
const sansLatin = localFont({
  src: "../../public/brand/fonts/ShannonSans-Variable-latin.woff2",
  weight: "100 900",
  style: "normal",
  variable: "--font-shannon-latin",
  display: "swap",
  // The extended face's metric-matched fallback sits last in the stack; a
  // fallback here would claim every non-Latin glyph before the extended face.
  adjustFontFallback: false,
  declarations: [
    {
      prop: "unicode-range",
      value:
        "U+0000-00FF, U+0131, U+0152-0153, U+02BB-02BC, U+02C6, U+02DA, U+02DC, U+0304, U+0308, U+0329, U+2000-206F, U+20AC, U+2122, U+2190-2199, U+2212-2215, U+FEFF, U+FFFD",
    },
  ],
});

const sansExt = localFont({
  src: "../../public/brand/fonts/ShannonSans-Variable-ext.woff2",
  weight: "100 900",
  style: "normal",
  variable: "--font-shannon-ext",
  display: "swap",
  preload: false,
  declarations: [
    {
      prop: "unicode-range",
      value:
        "U+0100-02BA, U+02BD-02C5, U+02C7-02D9, U+02DB, U+02DD-0303, U+0305-0307, U+0309-0328, U+032A-052F, U+0900-097F, U+10FB, U+1AB0-1ACE, U+1C80-1C88, U+1D00-1FFF, U+2070-209C, U+20A0-20AB, U+20AD-20C0, U+20F0, U+2100-2121, U+2123-218F, U+25CC, U+2C60-2C7F, U+2DE0-2E5D, U+A640-A69F, U+A700-A7FF, U+A8FF, U+A92E, U+AB30-AB6B, U+FB00-FB06, U+FE00-FE2F, U+FFFC, U+10780-107BA, U+1DF00-1DF1E",
    },
  ],
});

export function generateStaticParams() {
  return locales.map((locale) => ({ locale }));
}

export async function generateMetadata({ params }: { params: Promise<{ locale: string }> }): Promise<Metadata> {
  const { locale } = await params;
  // The layout answers not-found for this; without the guard the home page's
  // title, canonical, and hreflang stream into that 404 (`/wp-login.php`).
  // Throwing here instead leaves the page with no title at all.
  if (!isValidLocale(locale)) return notFoundMetadata;
  const home = getHome(locale);
  return buildPageMetadata({
    path: "/",
    locale,
    title: home.metaTitle,
    description: home.metaDescription,
  });
}

export default async function LocaleLayout({
  children,
  params,
}: {
  children: React.ReactNode;
  params: Promise<{ locale: string }>;
}) {
  const { locale } = await params;
  // Dotted paths bypass locale redirection so real files keep resolving.
  // An unknown path such as /foo.txt (or /foo.txt/faq) still binds `[locale]`
  // here. Reject it before reading dictionaries or rendering home chrome,
  // so nonexistent files never become HTTP 200 pages with a fake html lang.
  if (!isValidLocale(locale)) notFound();
  const chrome = getChrome(locale);
  // RTL locales (e.g. ar) set the document direction from the canonical
  // registry so the browser handles bidirectional layout from the root.
  const dir = localeDirection(locale);
  const siteJsonLd = buildSiteJsonLd(locale);

  return (
    <html
      lang={locale}
      dir={dir}
      className={`${sansLatin.variable} ${sansExt.variable}`}
      suppressHydrationWarning
    >
      <body>
        <script
          type="application/ld+json"
          dangerouslySetInnerHTML={{ __html: serializeJsonLd(siteJsonLd) }}
        />
        {/* Site-wide theme, applied before paint so there is no flash. The
            site is light unless a stored "dark" (`cw-theme`, see
            components/theme-toggle.tsx) pins the dark set. */}
        <script
          dangerouslySetInnerHTML={{
            __html:
              "(function(){try{var t=localStorage.getItem('cw-theme');if(t==='light'||t==='dark'){document.documentElement.setAttribute('data-theme',t);}}catch(e){}})();",
          }}
        />
        <a href="#main-content" className="skip-link">
          {chrome.skipToContent}
        </a>
        <Nav locale={locale} />
        <main id="main-content">{children}</main>
        <Footer locale={locale} />
        {/* Aggregate usage counting, on by default — see lib/telemetry. The
            choice lives on the privacy page; every opt-out stays off. */}
        <UsageCounting appVersion={BUILD_FACTS.version ?? "0.0.0"} />
      </body>
    </html>
  );
}
