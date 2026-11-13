import nextVitals from "eslint-config-next/core-web-vitals";
import nextTs from "eslint-config-next/typescript";

const eslintConfig = [
  {
    ignores: [
      "node_modules/**",
      ".next/**",
      ".open-next/**",
      ".wrangler/**",
      ".cloudflare/**",
      "out/**",
      "build/**",
      "dist/**",
      "coverage/**",
      "next-env.d.ts",
      // Vendored whale-character-v2 reference (executable oracle; not edited here).
      "vendor/whale-character-v2/**",
    ],
  },
  ...nextVitals,
  ...nextTs,
  {
    rules: {
      // Bilingual CJK content uses curly quotes intentionally
      "react/no-unescaped-entities": "off",
    },
  },
  {
    files: [
      "components/connection-banner.tsx",
      "components/docs-search.tsx",
      "components/mobile-menu.tsx",
      "components/ratatui/explorer.tsx",
      "components/theme-toggle.tsx",
      "components/usage-counting.tsx",
    ],
    rules: {
      "react-hooks/set-state-in-effect": "warn",
    },
  },
  {
    files: ["app/\\[locale\\]/page.tsx"],
    rules: {
      "react-hooks/purity": "off",
    },
  },
];

export default eslintConfig;
