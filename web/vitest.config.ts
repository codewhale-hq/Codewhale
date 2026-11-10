import { fileURLToPath } from "node:url";
import { defineConfig } from "vitest/config";

export default defineConfig({
  resolve: {
    alias: {
      "@": fileURLToPath(new URL(".", import.meta.url)),
    },
  },
  test: {
    globalSetup: ["./vitest.global-setup.ts"],
    include: [
      "lib/**/*.test.ts",
      "components/**/*.test.tsx",
      // The embedded runtime dashboard ships as plain ESM inside the Rust
      // crate, but its target-resolution rail decides whether a reply or an
      // approval is sent at all (#4397). Reaching outside `web/` keeps the
      // test next to the module it covers while still running under the one
      // JS test job CI has.
      "../crates/app-server/src/runtime_web/**/*.test.mjs",
    ],
  },
});
