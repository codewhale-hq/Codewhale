import type { Config } from "tailwindcss";

export default {
  // Dictionaries carry per-locale class names (e.g. the Arabic and Hindi
  // docs guide's `leading-loose`); unscanned, those utilities never exist.
  content: ["./app/**/*.{ts,tsx}", "./components/**/*.{ts,tsx}", "./lib/i18n/dictionaries/**/*.ts"],
  theme: {
    extend: {
      colors: {
        // Every colour resolves through the role tokens
        // (app/styles/tokens-roles.css), so the stage and both appearances
        // re-ink it. Alpha modifiers mix the role toward transparent.
        ...Object.fromEntries(
          Object.entries({
            canvas: "--bg",
            surface: "--surface",
            panel: "--panel",
            fg: "--text",
            muted: "--muted",
            line: "--line",
            accent: "--accent",
            "on-accent": "--on-accent",
            hover: "--hover",
            selected: "--selected",
            attention: "--attention",
            live: "--live",
            success: "--success",
            danger: "--danger",
            // Legacy names still used by the docs pages (post-renovation sweep).
            paper: "--paper",
            "paper-deep": "--paper-deep",
            "paper-edge": "--paper-edge",
            "paper-card": "--paper-card",
            "paper-line": "--paper-line",
            "paper-line-soft": "--paper-line-soft",
            ink: "--ink",
            "ink-soft": "--ink-soft",
            "ink-mute": "--ink-mute",
            indigo: "--indigo",
            "indigo-deep": "--indigo-deep",
            "indigo-pale": "--indigo-pale",
            ochre: "--ochre",
            jade: "--jade",
            cobalt: "--cobalt",
          }).map(([name, role]) => [
            name,
            `color-mix(in srgb, var(${role}) calc(<alpha-value> * 100%), transparent)`,
          ]),
        ),
      },
      fontFamily: {
        // Every family resolves through the role stacks in
        // app/styles/tokens-roles.css (Geist faces loaded in app/[locale]/layout.tsx).
        display: ["var(--font-display)"],
        body: ["var(--font-body)"],
        cjk: ["var(--font-cjk)"],
        mono: ["var(--font-mono)"],
      },
      // `transition-colors` and friends use the same motion tokens as
      // app/styles, so reduced motion stills them too.
      transitionDuration: { DEFAULT: "var(--dur-state)" },
      transitionTimingFunction: { DEFAULT: "var(--ease-spring)" },
    },
    // Replaces Tailwind's scale with the GPUI radius grammar
    // (app/styles/tokens-roles.css): 6px controls, 10px surfaces (cards,
    // code, menus), 14px raised sheets, and pills. Nothing in between.
    borderRadius: {
      none: "0",
      sm: "var(--radius-control)",
      DEFAULT: "var(--radius-control)",
      lg: "var(--radius-surface)",
      xl: "var(--radius-sheet)",
      full: "var(--radius-pill)",
    },
    // Replaces Tailwind's scale so wide tracking cannot be generated: labels
    // are sentence case at normal tracking. `wide` stays for Han body copy.
    letterSpacing: {
      tighter: "-0.05em",
      tight: "-0.025em",
      crisp: "-0.018em",
      normal: "0em",
      wide: "0.025em",
    },
  },
  // No all-caps anywhere, as in the GPUI app: the `uppercase` utility and
  // its siblings are not generated.
  corePlugins: { textTransform: false },
  plugins: [],
} satisfies Config;
