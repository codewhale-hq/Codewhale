/**
 * Dictionary shapes for the website localization layer (#3091, #4934).
 *
 * `ChromeDict` covers shared chrome: the newspaper masthead, nav, mobile
 * menu, theme toggle, live ticker, footer, and the locale switcher with its
 * visible partial-pack badge. `HomeDict` covers the landing page
 * (`app/[locale]/page.tsx`). Templates use `{name}` tokens interpolated
 * with `fill()` from dictionaries/index.ts — never concatenate translated
 * sentences around variables in JSX.
 *
 * English (`dictionaries/en/`) is the reference shape; every routed locale —
 * including Chinese — must define exactly the same keys. Parity is enforced
 * by `web/scripts/check-locales.mjs` and `web/lib/i18n/dictionaries.test.ts`.
 * A locale without a dictionary falls back to the English one at lookup
 * time, so an untranslated string renders English copy — never a key.
 *
 * Code-owned strings stay out of these dictionaries per docs/VOICE.md:
 * "Plan · Work · Operate", "Ask · Auto-Review · Full Access",
 * "TUI · exec · web · API", "Codewhale", "GitHub", "Issues",
 * `npm install -g codewhale`, `cargo test --locked`, `codewhale exec`,
 * package-manager proper nouns, mirror names, and captured media paths.
 */

export interface ChromeDict {
  // --- primary nav labels (components/nav.tsx via lib/i18n/links.ts) ---
  navDocs: string;
  navStart: string;
  navInstall: string;
  navFaq: string;
  navCommunity: string;
  navContribute: string;

  /**
   * The primary strip: Product / Models / Plugins / Docs. The
   * older six (Start, Install, FAQ, Community, Contribute) stay in the
   * dictionary for the compact sheet's second group and the footer.
   */
  navProduct: string;
  navFeatures: string;
  navModels: string;
  navPlugins: string;

  /**
   * Skip-to-content link rendered before the nav in app/[locale]/layout.tsx.
   * It sits on EVERY page of EVERY locale, so it belongs to shared chrome —
   * leaving it hardcoded is what kept an EN/ZH branch alive in the layout.
   */
  skipToContent: string;

  /** aria-label for the primary <nav> landmark (components/nav-links.tsx). */
  navPrimaryAria: string;
  /** aria-label for the wordmark link back to the locale home. */
  navHomeAria: string;

  /** Mobile-menu and masthead call to action, e.g. "Install →". */
  installCta: string;

  /**
   * BCP 47 tag used for the masthead weekday via `toLocaleDateString` — not
   * rendered copy, but per-locale, so it belongs beside it. Without this the
   * masthead date renders in English for every non-Chinese locale.
   */
  dateLocale: string;

  /** Mobile-menu toggle labels. */
  menuOpen: string;
  menuClose: string;

  /** Docs theme toggle: the three cycle states. */
  themeAuto: string;
  themeLight: string;
  themeDark: string;
  /** Theme toggle aria-label, e.g. "Docs theme: {mode} (click to cycle)". */
  themeAria: string;
  /** Theme toggle title attribute. */
  themeTitle: string;

  // --- footer ---
  footerTagline: string;
  footerProduct: string;
  footerProject: string;
  footerDocs: string;
  footerGuide: string;
  footerInstall: string;
  footerModels: string;
  footerRuntime: string;
  footerFaq: string;
  footerIssues: string;
  footerContribute: string;
  footerLicense: string;
  /** Footer link to the terms route, e.g. "Terms". */
  footerTerms: string;
  /** Footer link to the privacy route, e.g. "Privacy". */
  footerPrivacy: string;
  /** Footer Product-column link to the release record, e.g. "Changelog". */
  footerChangelog: string;
  /** Prefix before the canonical-source link, e.g. "Canonical source: ". */
  footerCanonicalSource: string;
  /** Separator + label before the releases link, e.g. " · Releases: ". */
  footerReleases: string;
  /** Link text for the GitHub releases page. */
  footerReleasesLink: string;
  /** Link text for the security-contact mailto. */
  footerSecurity: string;

  /** aria-label for the locale switcher control. */
  switcherLabel: string;
  /** Two-locale toggle aria-label, e.g. "Switch to {label}". */
  switcherSwitchTo: string;
  /**
   * Visible badge marking a partial locale pack in the switcher, e.g.
   * "(partial)" — honest scope signal, per the localization quality
   * contract. Keep it short. The switcher shows it, brackets stripped, as
   * the heading of the group that lists partial packs.
   */
  partialBadge: string;
}

export interface FeaturesDict {
  title: string;
  description: string;
  notice: string;
  stable: string;
  flagged: string;
  experimental: string;
  partial: string;
  preview: string;
  none: string;
  docs: string;
  source: string;
  exportJson: string;
}

export interface HomeDict {
  /**
   * `<title>` and meta description for the locale home route, consumed by
   * `generateMetadata` in app/[locale]/layout.tsx.
   */
  metaTitle: string;
  metaDescription: string;

  /** A complete headline that wraps naturally in each locale. */
  heroTitle: string;
  /**
   * Hero lede. Carries a `{brand}` token so the brand can be typeset in its
   * own span wherever the sentence needs it — the page splits on the token
   * instead of concatenating fragments around it.
   */
  heroIntro: string;
  /** Primary action → /install, e.g. "Get Codewhale". */
  getCodewhale: string;
  /** Accessible name for the hero install command and its platform choice. */
  heroInstallAria: string;
  /** Secondary action → /product, e.g. "Explore the product". */
  exploreProduct: string;

  /** Screenshot caption, first item of the dot chain, e.g. "Terminal preview". */
  shotPreview: string;
  /** Screenshot alt text for the current media manifest capture. */
  screenshotAlt: string;

  /** "Latest release {tag}" */
  latestRelease: string;
  releaseUnavailable: string;
  proofContributors: string;
  /** "Source" / "Unreleased" — prepended to `v{version}`. */
  currentSource: string;
  sourceCandidate: string;
  /** "released" / "unreleased" — the machine-readable source-state label. */
  publishedRelease: string;

  /** What a person gains: heading, lede, and three [title, body] rows. */
  gainHeading: string;
  gainLede: string;
  gain: [string, string][];

  /** Running head of the models chapter ("02 / Your models"). */
  chapterModels: string;
  modelsHeading: string;
  modelsBody: string;
  /** Three [route kind, description] rows. */
  modelsFacts: [string, string][];
  modelsLink: string;

  startHeading: string;
  startLede: string;
  startGuideLink: string;
  startVocabularyLink: string;

  /** Running head of the availability chapter ("04 / Where it runs"). */
  chapterAvailability: string;
  availabilityHeading: string;
  availabilityLede: string;
  availability: [string, string, string, string?][];
  availabilityNote: string;

  surfacesHeading: string;
  /** Five [name, description] surfaces. */
  surfaces: [string, string][];
  runtimeLink: string;

  installBandHeading: string;
  copy: string;
  copied: string;
  binaries: string;
  chinaMirrors: string;
  installGuideLink: string;

  communityHeading: string;
  communityBody: string;
  communityLinksAria: string;
  contribute: string;
}

/**
 * Docs "Getting started" page (`app/[locale]/docs/guide/page.tsx`).
 *
 * First of the per-page dictionaries that retire the page-body `isZh`
 * branches left after #4934. Page dictionaries are optional per locale:
 * English is the required reference, any other locale that ships the file
 * is held to exact key parity (`check-locales.mjs` OPTIONAL_FILES), and a
 * locale without it falls back to English at lookup time — matching how
 * page bodies already behave for partial locales.
 */
export interface DocsGuideDict {
  metaTitle: string;
  metaDescription: string;
  /** Body-copy typography for this locale (CJK needs looser leading). */
  bodyClassName: string;
  overviewTitle: string;
  overviewLead: string;
  sessionTitle: string;
  sessionLead: string;
  nextTitle: string;
  sourceNote: string;
}

/**
 * The docs shell: the portal hero in `app/[locale]/docs/layout.tsx` that
 * wraps every docs page, plus the metadata of the hub page it frames
 * (`app/[locale]/docs/page.tsx`, whose body is the `DocsSearch` component).
 *
 * No `bodyClassName` here: the hero typesets through `portal-*` classes,
 * which never varied by locale.
 */
export interface DocsShellDict {
  metaTitle: string;
  metaDescription: string;
  portalMark: string;
  heroTitle: string;
  heroLead: string;
  installCta: string;

  // --- release truth band (docs layout; facts + CHANGELOG.md) ---
  /** Eyebrow over the band, e.g. "Release". */
  releaseLabel: string;
  /** "Latest release {tag} · {date}" — date already formatted per locale. */
  releasePublished: string;
  /** Shown while the source candidate is ahead of the published release. */
  releaseCandidate: string;
  /** Shown when the pages describe exactly the published release. */
  releaseMatches: string;
  /** Link to the /changelog route. */
  releaseChangelog: string;

  // --- task/topic search (components/docs-search.tsx) ---
  searchLabel: string;
  searchPlaceholder: string;
  searchClear: string;
  /** "{matched} of {total} entries match “{query}”". */
  searchMatches: string;
  /** "Nothing matches “{query}”". */
  searchNoMatches: string;
  tasksHeading: string;
  tasksLead: string;
  topicsHeading: string;
  /** Row tag for a first-party page. */
  webGuideTag: string;
  /** Row tag for a GitHub source document. */
  sourceDocTag: string;
  /** Per-row disclosure that reveals the repository files a topic comes from. */
  sourceDetails: string;
  emptyTitle: string;
  emptyBody: string;
  emptyCta: string;
  indexNote: string;

  // --- sidebar and breadcrumb (components/docs-sidebar.tsx, docs-breadcrumb.tsx) ---
  sidebarHeading: string;
  sidebarAria: string;
  breadcrumbAria: string;
  breadcrumbHome: string;
  breadcrumbDocs: string;

  // --- contextual help band under every docs page ---
  helpTitle: string;
  helpLead: string;
  /** "Source: {name}" — the topic's repository document(s). */
  helpSource: string;
  helpTroubleshooting: string;
  helpFaq: string;
  helpDiscord: string;
  helpIssue: string;

  // --- docs page bodies (app/[locale]/docs/_components/doc-article.tsx) ---
  /** Heading over each page's closing "where to go next" links. */
  nextHeading: string;
  /** Word (with its punctuation) that marks an aside, so it reads without color. */
  noteLabel: string;
  /** Accessible name of a page's table of contents. */
  onThisPage: string;

  // --- session recording panel (components/session-media.tsx) ---
  /** Shown in place of a recording that has not been made yet. */
  mediaPendingNote: string;
  mediaPlanLink: string;
  mediaGifFallback: string;
  mediaTranscript: string;
}

/**
 * Shared surface states (`components/surface-state.tsx`,
 * `components/connection-banner.tsx`, the `loading.tsx` / `error.tsx` /
 * `not-found.tsx` boundaries). One dictionary for every empty, loading,
 * error, retry, recovery, and connection state so no page invents its own.
 */
export interface StatesDict {
  loadingLabel: string;
  emptyTitle: string;
  emptyBody: string;
  errorTitle: string;
  errorBody: string;
  retry: string;
  reload: string;
  homeLink: string;
  /** Documentation recovery link on the 404 page. */
  docsIndexLink: string;
  notFoundTitle: string;
  notFoundBody: string;
  notFoundHomeLink: string;
  /** Alt text for the 404 poster (public/codwhale-404.webp). */
  notFoundPosterAlt: string;
  /**
   * A data-bearing page whose source was not asked (build-time prerender)
   * or refused (rate limit, outage). Distinct from `empty`, which asserts
   * that nothing exists, and from `error`, which means the render threw.
   */
  unavailableTitle: string;
  unavailableBody: string;
  /** Connection banner (typed state in lib/connection-state.ts). */
  offlineTitle: string;
  offlineBody: string;
  reconnectingTitle: string;
  /** "Checking the connection (attempt {attempt})." */
  reconnectingBody: string;
  degradedTitle: string;
  degradedBody: string;
  onlineTitle: string;
  onlineBody: string;
  retryNow: string;
  dismiss: string;
  /** "Last checked {time}". */
  lastChecked: string;
}

/* ------------------------------------------------------------------ */
/*  Docs page bodies                                                   */
/* ------------------------------------------------------------------ */

/**
 * One content block on a docs page. Strings are prose: `code` in backticks
 * is typeset as inline code and `[label](/docs/x)` becomes a locale-aware
 * link (see `app/[locale]/docs/_components/doc-article.tsx`). Commands in
 * `code` blocks are shown verbatim and must match the engine source.
 */
export type DocsBlock =
  | { p: string }
  /** A copyable command or config block; `lang` labels it (e.g. "Terminal"). */
  | { code: string; lang?: string }
  /** A `[term, detail]` table; `codeTerms` sets the term column as code. */
  | { rows: readonly (readonly [string, string])[]; codeTerms?: boolean }
  /** Numbered steps, in order. */
  | { steps: readonly string[] }
  /** An unordered list. */
  | { list: readonly string[] }
  /** A short aside: a limit, a caution, or something not built yet. */
  | { note: string };

export interface DocsSection {
  /** Stable anchor id. */
  id: string;
  title: string;
  blocks: readonly DocsBlock[];
}

/**
 * The one shape every task page in `/docs` uses: what this page helps you
 * do (title + lede), how (sections), and where to go next.
 */
export interface DocsPageDict {
  metaTitle: string;
  metaDescription: string;
  /** Body-copy typography for this locale (CJK needs looser leading). */
  bodyClassName: string;
  title: string;
  /** What the reader can do here and why — two sentences at most. */
  lede: string;
  sections: readonly DocsSection[];
  /** Where to go next; `href` is locale-relative. */
  next: readonly { href: string; label: string; note: string }[];
  /** Maintainer pointer, kept out of the rendered copy. */
  sourceNote: string;
}

export type DocsReviewDict = DocsPageDict;

export type DocsAuthDict = DocsPageDict;

export type DocsTrustDict = DocsPageDict;

/** `app/[locale]/changelog/page.tsx` — version-aware release record. */
export interface ChangelogDict {
  metaTitle: string;
  metaDescription: string;
  kicker: string;
  title: string;
  lead: string;
  publishedLabel: string;
  /** "{tag} · published {date}" */
  publishedValue: string;
  candidateLabel: string;
  /** "{version} · unreleased" */
  candidateValue: string;
  /** Shown when the candidate equals the published release. */
  candidateMatches: string;
  releasesLink: string;
  unreleasedHeading: string;
  unreleasedNote: string;
  compareLink: string;
  releasePageLink: string;
  /** "{shown} of {total} entries" */
  moreEntries: string;
  fullNotes: string;
  /** "Full notes for {version}" — links /changelog/<version>, or CHANGELOG.md for Unreleased. */
  releaseNotesLink: string;
  emptyTitle: string;
  emptyBody: string;
  /** `/changelog/[version]` — every entry for one release. */
  versionMetaTitle: string;
  versionMetaDescription: string;
  versionKicker: string;
  /** "Released {date}. …" */
  versionLead: string;
  versionLeadUndated: string;
  allReleasesLink: string;
}

export interface LegalTermsDict {
  metaTitle: string;
  metaDescription: string;
  kicker: string;
  title: string;
  /** "Effective and last updated {date}." — zh also says the English text binds. */
  updated: string;
  privacyLink: string;
  homeLink: string;
}

export interface LegalPrivacyDict {
  metaTitle: string;
  metaDescription: string;
  kicker: string;
  title: string;
  /** Same template as `LegalTermsDict.updated`. */
  updated: string;
  termsLink: string;
  homeLink: string;
}

export interface DigestDict {
  metaTitle: string;
  metaDescription: string;
  /** Heading shown with the empty state. */
  emptyTitle: string;
  emptyBody: string;
  title: string;
  lead: string;
}

/** `app/[locale]/feed/page.tsx`. */
export interface FeedDict {
  metaTitle: string;
  metaDescription: string;
  /** Page H1. */
  title: string;
  /** The H1 again in the other script, set beside it (the bilingual Han title). */
  titleAside: string;
  /** BCP 47 language of `titleAside`. */
  titleAsideLang: string;
  /** `{repo}` is where the page typesets the repository link. */
  lede: string;
  pulls: string;
  issues: string;
  /** Items in a column; `{count}` is filled at render time. */
  shownCount: string;
  openIssue: string;
  openPull: string;
  startDiscussion: string;
}

/** `app/[locale]/community/page.tsx`. */
export interface CommunityDict {
  metaTitle: string;
  metaDescription: string;
  kicker: string;
  /** Page H1. */
  title: string;
  lede: string;
  fileIssue: string;
  browsePulls: string;
  readGuide: string;
  pathsTitle: string;
  pathsScope: string;
  recordTitle: string;
  recordScope: string;
  creditTitle: string;
  /** Credit scope once the source version is the published release. */
  creditScope: string;
  /** Credit scope while the source version is ahead of the published release. */
  creditScopeUnreleased: string;
  /** Credit heading for the published release; `{version}` is filled at render time. */
  creditLabel: string;
  /** Credit heading while the source version is unreleased; `{version}` is filled at render time. */
  creditLabelUnreleased: string;
  mergedTitle: string;
  helpersTitle: string;
  fullRecord: string;
}

/** `app/[locale]/faq/page.tsx` and its `components/faq-search.tsx`. */
export interface FaqDict {
  metaTitle: string;
  metaDescription: string;
  eyebrow: string;
  /** Page H1. */
  title: string;
  /** The H1 again in the other script, set beside it (the bilingual Han title). */
  titleAside: string;
  /** BCP 47 language of `titleAside`. */
  titleAsideLang: string;
  lead: string;
  notCovered: string;
  openIssue: string;
  searchPlaceholder: string;
  searchLabel: string;
  searchClear: string;
  /** `{matched}`, `{total}` and `{query}` are filled at render time. */
  searchMatches: string;
  /** `{query}` is filled at render time. */
  searchNoMatches: string;
  /** Extra classes on each answer. Empty in both locales today: CJK leading lives in `.prose` (primitives.css). */
  answerClassName: string;
  sourcesLabel: string;
  noResultsTitle: string;
  noResultsBody: string;
}

/** `app/[locale]/roadmap/page.tsx`. */
export interface RoadmapDict {
  metaTitle: string;
  metaDescription: string;
  eyebrow: string;
  /** Page H1. */
  title: string;
  introduction: string;
  sectionTitle: string;
  browseIssues: string;
  /** Track size; `{count}` is filled at render time. */
  trackCount: string;
  /** Track size when it is exactly one. */
  trackCountOne: string;
  contributeTitle: string;
  contributeBody: string;
  issuesDetail: string;
  discussionsDetail: string;
  pullsDetail: string;
}

/** `app/[locale]/docs/vocabulary/page.tsx`. */
export interface DocsVocabularyDict {
  metaTitle: string;
  metaDescription: string;
  /** Classes on every body paragraph; zh loosens leading and tracking. */
  bodyClassName: string;
  /** Page H1. */
  title: string;
  lead: string;
  executionHeading: string;
  controlHeading: string;
  controlLead: string;
  routeHeading: string;
  advisoryHeading: string;
  measurementHeading: string;
  leaderboardNote: string;
  /** Maintainer pointer, carried in a hidden `data-source-note`. */
  sourceNote: string;
}

/** `app/[locale]/contribute/page.tsx`. */
export interface ContributeDict {
  metaTitle: string;
  metaDescription: string;
  kicker: string;
  /** Page H1. */
  title: string;
  lede: string;
  fileIssue: string;
  browsePulls: string;
  fullGuide: string;
  pathsTitle: string;
  workflowTitle: string;
  reviewTitle: string;
  reviewScope: string;
  devTitle: string;
  devScope: string;
}

/** `app/[locale]/constitution/page.tsx` and its `components/thinking-trace.tsx`. */
export interface ConstitutionDict {
  metaTitle: string;
  metaDescription: string;
  kicker: string;
  /** Page H1. */
  title: string;
  /** The H1 again in the other script, set beside it (the bilingual Han title). */
  titleAside: string;
  /** BCP 47 language of `titleAside`. */
  titleAsideLang: string;
  lede: string;
  /** Status label on the setup callout. */
  since: string;
  sinceBody: string;
  rankTitle: string;
  rankScope: string;
  boundaryTitle: string;
  boundaryBody: string;
  traceTitle: string;
  traceScope: string;
  /** The label every traced scene carries: these are illustrations, not transcripts. */
  illustration: string;
  install: string;
  configuration: string;
}

/** `app/[locale]/runtime/page.tsx`. */
export interface RuntimeDict {
  metaTitle: string;
  metaDescription: string;
  kicker: string;
  /** Page H1. */
  title: string;
  /** The H1 again in the other script, set beside it (the bilingual Han title). */
  titleAside: string;
  /** BCP 47 language of `titleAside`. */
  titleAsideLang: string;
  lede: string;
  integrationsTitle: string;
  /** Status label on an experimental integration. */
  experimental: string;
  trustTitle: string;
  factsTitle: string;
  version: string;
  toolCount: string;
  sandboxBackends: string;
  /** Summary of the maintainer-facing disclosure (crates, source revision). */
  details: string;
  sourceRevision: string;
  docsLead: string;
  runtimeApiDoc: string;
  mcpDoc: string;
}

export type DocsHooksDict = DocsPageDict;

export type DocsTroubleshootingDict = DocsPageDict;

export type DocsConfigurationDict = DocsPageDict;

export type DocsFleetDict = DocsPageDict;

export type DocsMcpDict = DocsPageDict;

export type DocsModesDict = DocsPageDict;

export type DocsRuntimeApiDict = DocsPageDict;

export type DocsSandboxDict = DocsPageDict;

export type DocsSubagentsDict = DocsPageDict;

export type DocsWebDict = DocsPageDict;

export type DocsWorkDict = DocsPageDict;

export type DocsPetDict = DocsPageDict;

/** Copy for `app/[locale]/computer-use/page.tsx` and the install page's Computer Use section. */
export interface ComputerUseDict {
  metaTitle: string;
  metaDescription: string;
  title: string;
  lead: string;
  publisher: string;
  /** Primary button: the notarized disk image when the release carries one, else the archive. */
  download: string;
  /** Secondary link to the archive the in-app updater consumes. */
  downloadZip: string;
  requirements: string;
  included: string;
  pendingTitle: string;
  pendingBody: string;
  unavailableTitle: string;
  unavailableBody: string;
  releases: string;
  receipt: string;
  setup: string;
  /** Four numbered setup steps, rendered in order. */
  steps: { title: string; body: string }[];
  controlsTitle: string;
  controlsBody: string;
  updateTitle: string;
  updateBody: string;
  help: string;
  notes: string;
  demo: string;
  source: string;
  platforms: string;
}
