import type { ChromeDict } from "../types";

/**
 * English reference chrome dictionary. Every other locale must match these
 * keys exactly (`npm run check:locales`, `dictionaries.test.ts`).
 */
export const chrome: ChromeDict = {
  navDocs: "Docs",
  navStart: "Start",
  navInstall: "Install",
  navFaq: "FAQ",
  navCommunity: "Community",
  navContribute: "Contribute",

  navFeatures: "Capabilities",
  navProduct: "Product",
  navModels: "Models",
  navPlugins: "Plugins",

  skipToContent: "Skip to main content",

  navPrimaryAria: "Primary",
  navHomeAria: "Codewhale home",

  installCta: "Install →",

  dateLocale: "en-US",

  menuOpen: "Open menu",
  menuClose: "Close menu",

  themeAuto: "auto",
  themeLight: "light",
  themeDark: "dark",
  themeAria: "Theme: {mode} (click to cycle)",
  themeTitle: "Theme · auto / light / dark",

  footerTagline:
    "Build apps. Automate work.",
  footerProduct: "Product",
  footerProject: "Project",
  footerDocs: "Docs",
  footerGuide: "Getting started",
  footerInstall: "Install",
  footerModels: "Models",
  footerRuntime: "Runtime",
  footerFaq: "FAQ",
  footerIssues: "Issues",
  footerContribute: "Contribute",
  footerLicense: "MIT license",
  footerTerms: "Terms",
  footerPrivacy: "Privacy",
  footerChangelog: "Changelog",
  footerCanonicalSource: "Canonical source: ",
  footerReleases: " · Releases: ",
  footerReleasesLink: "GitHub Releases",
  footerSecurity: "Security",

  switcherLabel: "Language",
  switcherSwitchTo: "Switch to {label}",
  partialBadge: "(partial)",
};
