import type { ComputerUseDict } from "../types";

/**
 * English reference dictionary for `app/[locale]/computer-use/page.tsx` and
 * the Computer Use section of the install page. Product names (Codewhale,
 * Computer Use), the menu items Pause, Stop and Check for updates, and the
 * macOS setting names stay as the app shows them.
 */
export const computerUse: ComputerUseDict = {
  metaTitle: "Computer Use · Mac beta · Codewhale",
  metaDescription: "Let an agent work in Mac apps while you keep using your computer. Download the notarized Computer Use helper and connect a compatible MCP host.",
  title: "Computer Use",
  lead: "Let an agent work in Mac apps while you keep using your computer. Connect Computer Use to Codewhale or another compatible MCP host, and pause or stop it whenever you need.",
  publisher: "By Codewhale",
  download: "Download for Mac",
  downloadZip: "ZIP archive (used by the in-app updater)",
  requirements: "macOS 13.5 or later · Apple silicon and Intel",
  included: "The Mac helper includes its runtime. Connect it to Computer Use in your MCP host; host setup is separate.",
  pendingTitle: "Mac download in preparation",
  pendingBody: "The public installer will appear here after Apple notarization and release checks are complete.",
  unavailableTitle: "Download availability could not be checked",
  unavailableBody: "Refresh this page to try again, or check the published releases below.",
  releases: "Published releases",
  receipt: "Download verification details",
  setup: "Set up your Mac",
  steps: [
    { title: "Install the app", body: "Open the disk image and drag Codewhale Computer Use into Applications. Open it from Applications, then choose Computer Use from the whale icon in your menu bar." },
    { title: "Review permissions", body: "Use the setup buttons to open Accessibility and Screen Recording in System Settings. You choose which permissions to grant." },
    { title: "Connect your agent", body: "In Codewhale, install Computer Use with /plugin marketplace install codewhale computer-use, then review and enable the plugin. Other compatible MCP hosts connect to the helper using the setup guide. The helper and host plugin are separate pieces." },
  ],
  controlsTitle: "Stay in control",
  controlsBody: "After you grant the macOS permissions, standard mode works without per-app prompts. Apps you deny stay off limits; Pause suspends computer use and Stop computer use ends current sessions. Strict approval is optional. Request a live preview when you want to watch.",
  updateTitle: "Updates when you choose",
  updateBody: "Choose Check for updates from the app. Before installing an update, it checks the download, Codewhale signature, and Apple notarization, and keeps the previous app for recovery.",
  help: "Setup and troubleshooting",
  notes: "Release notes",
  demo: "See the background check",
  source: "Other hosts and platforms",
  platforms: "The Mac helper is a beta, and the marketplace plugin supports Mac. Published releases also include an unsigned experimental Windows x64 preview; Linux requires experimental source or Docker setup. Windows and Linux input uses the shared desktop.",
};
