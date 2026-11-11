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

const sans = localFont({
  src: "../../public/brand/fonts/Geist-Variable.woff2",
  weight: "100 900",
  style: "normal",
  variable: "--font-geist-sans",
  display: "swap",
});

const mono = localFont({
  src: "../../public/brand/fonts/GeistMono-Variable.woff2",
  weight: "100 900",
  style: "normal",
  variable: "--font-geist-mono",
  display: "swap",
  preload: false,
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
      className={`${sans.variable} ${mono.variable}`}
      suppressHydrationWarning
    >
      <body>
        <script
          type="application/ld+json"
          dangerouslySetInnerHTML={{ __html: serializeJsonLd(siteJsonLd) }}
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
