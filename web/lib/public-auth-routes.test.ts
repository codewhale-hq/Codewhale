import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { APP_LOGIN_URL, APP_URL } from "./i18n/links";
import {
  APP_AUTH_CALLBACK_URL,
  CANONICAL_MARK_SHA256,
  CANONICAL_MARK_SRC,
  canonicalPublicAuthPath,
  publicAuthAppDestination,
  publicAuthCallbackDestination,
  publicAuthKind,
  publicAuthRemainder,
} from "./public-auth-routes";

const GEIST_SANS_SHA256 = "5c5588027e59e74e205b9d6c7fd877b261091cee367f6bb3a8d505da14cae512";
const GEIST_SANS_BYTES = 69576;
const GEIST_MONO_SHA256 = "cf5bf55f69e043b1ecf04b0719119b7298e94add67ebcaa709a925b945b46b25";
const GEIST_MONO_BYTES = 71272;
const GEIST_OFL_SHA256 = "930853ee1daa68554d9e35c8a9175affb74f699fad9a5da6ee5ebe76379d9137";
const GEIST_OFL_BYTES = 4368;

describe("public auth remainders and kinds", () => {
  it("recognizes the observed 404 paths with and without a locale prefix", () => {
    expect(publicAuthRemainder("/signin")).toBe("signin");
    expect(publicAuthRemainder("/en/signin")).toBe("signin");
    expect(publicAuthRemainder("/pt-BR/signup")).toBe("signup");
    expect(publicAuthRemainder("/zh/auth/callback")).toBe("auth/callback");
    expect(publicAuthKind("/signin")).toBe("sign-in");
    expect(publicAuthKind("/en/signin")).toBe("sign-in");
    expect(publicAuthKind("/signup")).toBe("sign-up");
    expect(publicAuthKind("/ja/signup")).toBe("sign-up");
    expect(publicAuthKind("/auth/callback")).toBe("callback");
    expect(publicAuthKind("/en/auth/callback")).toBe("callback");
    expect(publicAuthKind("/install")).toBeNull();
    expect(publicAuthKind("/en/docs")).toBeNull();
  });

  it("folds login/register aliases onto the public sign-in route", () => {
    expect(canonicalPublicAuthPath("/login")).toBe("/signin");
    expect(canonicalPublicAuthPath("/en/login")).toBe("/en/signin");
    expect(canonicalPublicAuthPath("/register")).toBe("/signin");
    expect(canonicalPublicAuthPath("/zh/register")).toBe("/zh/signin");
    expect(canonicalPublicAuthPath("/create-account")).toBe("/signin");
    expect(canonicalPublicAuthPath("/signin")).toBeNull();
    expect(canonicalPublicAuthPath("/en/signup")).toBeNull();
  });
});

describe("CWC destinations", () => {
  it("sends English (and other) public pages to the default app entry", () => {
    expect(publicAuthAppDestination("en")).toBe(APP_LOGIN_URL);
    expect(publicAuthAppDestination("ja")).toBe(APP_LOGIN_URL);
    expect(publicAuthAppDestination("zh")).toBe(`${APP_URL}/zh/login`);
  });

  it("hops auth callbacks to the CWC app and preserves the query string", () => {
    expect(APP_AUTH_CALLBACK_URL).toBe(`${APP_URL}/auth/callback`);
    expect(
      publicAuthCallbackDestination(new URL("https://codewhale.net/auth/callback?code=abc&state=1")),
    ).toBe(`${APP_AUTH_CALLBACK_URL}?code=abc&state=1`);
    expect(
      publicAuthCallbackDestination(new URL("https://codewhale.net/en/auth/callback?code=abc")),
    ).toBe(`${APP_AUTH_CALLBACK_URL}?code=abc`);
    expect(
      publicAuthCallbackDestination(new URL("https://codewhale.net/en/signin")),
    ).toBeNull();
  });
});

describe("canonical mark", () => {
  it("ships the pinned raster generated from the canonical vector", () => {
    const bytes = readFileSync(new URL("../public/brand/codewhale-mark.png", import.meta.url));
    expect(createHash("sha256").update(bytes).digest("hex")).toBe(CANONICAL_MARK_SHA256);
    expect(CANONICAL_MARK_SRC).toBe("/brand/codewhale-mark.png");
  });
});

describe("website font assets", () => {
  it("ships the pinned Geist faces with the complete adjacent OFL notice", () => {
    const read = (name: string) => readFileSync(new URL(`../public/brand/fonts/${name}`, import.meta.url));
    const sha = (data: Buffer) => createHash("sha256").update(data).digest("hex");
    const [sans, mono, license] = [read("Geist-Variable.woff2"), read("GeistMono-Variable.woff2"), read("OFL.txt")];
    expect([sans.length, sha(sans)]).toEqual([GEIST_SANS_BYTES, GEIST_SANS_SHA256]);
    expect([mono.length, sha(mono)]).toEqual([GEIST_MONO_BYTES, GEIST_MONO_SHA256]);
    expect([license.length, sha(license)]).toEqual([GEIST_OFL_BYTES, GEIST_OFL_SHA256]);
  });
});
