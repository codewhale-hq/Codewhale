import { desktopDownload, type DesktopReleaseFact, DESKTOP_RELEASE } from "./desktop-release";

export interface RouteRedirect {
  to: string;
  status: 307 | 308;
}

export const PERMANENT_ROUTE_REDIRECTS: Readonly<Record<string, string>> = {
  community: "/contribute",
  feed: "/contribute",
  roadmap: "/contribute#milestones",
  digest: "/changelog",
  product: "",
  runtime: "/docs/runtime-api",
  constitution: "/docs/constitution",
  enterprise: "/faq#teams",
};

export const INSTALL_OR_APP_ROUTES: readonly string[] = ["download", "desktop"];

export const INSTALL_ALIAS_ROUTES: readonly string[] = ["pricing"];

export const REDIRECTED_ROUTES: readonly string[] = [
  ...Object.keys(PERMANENT_ROUTE_REDIRECTS),
  ...INSTALL_OR_APP_ROUTES,
  ...INSTALL_ALIAS_ROUTES,
];

export function routeRedirect(
  segment: string,
  release: DesktopReleaseFact | null = DESKTOP_RELEASE,
): RouteRedirect | null {
  const key = segment.toLowerCase();
  const permanent = PERMANENT_ROUTE_REDIRECTS[key];
  if (permanent !== undefined) return { to: permanent, status: 308 };
  if (INSTALL_OR_APP_ROUTES.includes(key)) {
    return { to: desktopDownload(release) ? "/app" : "/install", status: 307 };
  }
  if (INSTALL_ALIAS_ROUTES.includes(key)) return { to: "/install", status: 307 };
  return null;
}
