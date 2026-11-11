import type { RuntimeDict } from "../types";

/**
 * English reference dictionary for `app/[locale]/runtime/page.tsx`. Copy
 * moved verbatim from its `isZh` ternaries. The integration descriptions and
 * the trust-boundary items are content and stay in the page.
 */
export const runtime: RuntimeDict = {
  metaTitle: "Runtime & Integrations · Codewhale",
  metaDescription:
    "Connect editors, scripts, and chat apps to Codewhale through the local Runtime API over HTTP/SSE, an ACP stdio transport, MCP servers, an early VS Code companion, and messaging bridges.",
  kicker: "Runtime & Integrations",
  title: "Drive Codewhale from your own tools",
  titleAside: "运行时与集成",
  titleAsideLang: "zh",
  lede: "Codewhale runs a local control plane alongside the terminal. Editors, scripts, and chat apps use it to read threads, stream events, and answer approvals.",
  integrationsTitle: "Integration surfaces",
  experimental: "Experimental",
  trustTitle: "Trust boundary",
  factsTitle: "Runtime facts",
  version: "Source version",
  toolCount: "Tool implementations in the published release",
  sandboxBackends: "Sandbox backends",
  details: "Details",
  sourceRevision: "Source revision",
  docsLead: "Detailed implementation docs:",
  runtimeApiDoc: "Runtime API and ACP stdio adapter",
  mcpDoc: "MCP integration",
};
