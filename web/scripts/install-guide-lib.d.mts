export type InstallGuide = {
  preamble: string;
  sourceHash: string;
  anchors: string[];
  searchSections: { id: string; title: string; text: string }[];
  chunks: { kind: "html" | "code"; text: string }[];
};
export function buildInstallGuide(source: string, options?: { sourcePath?: string; tableLabel?: string }): InstallGuide;
export function installAnchorErrors(text: string, anchors: readonly string[]): string[];
export function renderInstallGuideModule(source: string, chineseSource?: string): string;
