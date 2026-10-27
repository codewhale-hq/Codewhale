import type { ComputerUseDict } from "../types";

/**
 * English reference dictionary for `app/[locale]/computer-use/page.tsx` and
 * the Computer Use section of the install page. Product names (Codewhale,
 * Computer Use), the menu items Pause, Stop and Check for updates, and the
 * macOS setting names stay as the app shows them.
 */
export const computerUse: ComputerUseDict = {
  metaTitle: "Computer Use · Preview · Codewhale",
  metaDescription: "Preview: download and set up Codewhale Computer Use for Mac. Background app control, permission setup, and human Pause and Stop controls.",
  title: "Computer Use",
  lead: "Let Codewhale work across your apps while you keep working. Trust it once on this Mac and stop computer use whenever you want.",
  publisher: "By Codewhale",
  download: "Download for Mac",
  downloadZip: "ZIP archive (used by the in-app updater)",
  requirements: "macOS 13.5 or later · Apple silicon and Intel",
  included: "One app download. No separate Node installation or compiler required.",
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
    { title: "Enable Computer Use", body: "In Codewhale’s Connectors settings, install or update Computer Use, then review and enable it. Ask it to work in an app. On first use, choose Trust until revoked in the native helper; that trust carries across apps, models, and tasks." },
  ],
  controlsTitle: "Stay in control",
  controlsBody: "The compact Codewhale control names the app in use. Pause suspends computer use; Stop computer use ends all current computer sessions. In Settings, choose Turn off under Remembered access to remove the connection’s saved access. Ask for a live preview when you want to watch.",
  updateTitle: "Updates when you choose",
  updateBody: "Choose Check for updates from the app. Before installing an update, it checks the download, Codewhale signature, and Apple notarization, and keeps the previous app for recovery.",
  help: "Setup and troubleshooting",
  notes: "Release notes",
  demo: "See the background check",
  source: "Source and other platforms",
  platforms: "This download is for Mac. Windows and Linux currently use the source plugin and host-side setup.",
};
