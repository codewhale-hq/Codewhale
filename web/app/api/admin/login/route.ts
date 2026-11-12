import { NextResponse } from "next/server";
import { getAgentEnv, safeEqual, createSession } from "@/lib/community-agent";
import { MAINTAINER_TOKEN_UNAVAILABLE, MAX_SECRET_CHARS, maintainerTokenUsable } from "@/lib/maintainer-token";
import { FormBodyError, readBoundedUrlEncodedForm } from "@/lib/bounded-form";

export const dynamic = "force-dynamic";

const ALLOWED_LOCALES = new Set(["en", "zh"]);
const MAX_LOGIN_BODY_BYTES = 4_096;

function pickLocale(value: string | null | undefined): string {
  if (!value) return "en";
  return ALLOWED_LOCALES.has(value) ? value : "en";
}

export async function POST(req: Request) {
  const env = await getAgentEnv();
  const url = new URL(req.url);
  const localeFromQuery = pickLocale(url.searchParams.get("locale"));

  if (!maintainerTokenUsable(env.MAINTAINER_TOKEN)) {
    return new NextResponse(MAINTAINER_TOKEN_UNAVAILABLE, {
      status: 503,
      headers: { "Cache-Control": "no-store" },
    });
  }

  // One maintainer principal: key by that account, never by attacker-controlled
  // headers or submitted tokens. The binding shares counters across isolates
  // in a Cloudflare location. An unavailable limiter must not disable the gate.
  try {
    if (!env.ADMIN_LOGIN_LIMITER) throw new Error("Missing login limiter");
    const { success } = await env.ADMIN_LOGIN_LIMITER.limit({ key: "codewhale-web:admin-login" });
    if (!success) {
      return new NextResponse("Too many login attempts", {
        status: 429,
        headers: { "Cache-Control": "no-store", "Retry-After": "60" },
      });
    }
  } catch {
    return new NextResponse("Login temporarily unavailable", {
      status: 503,
      headers: { "Cache-Control": "no-store", "Retry-After": "60" },
    });
  }

  let form: URLSearchParams;
  try {
    form = await readBoundedUrlEncodedForm(req, MAX_LOGIN_BODY_BYTES);
  } catch (error) {
    if (error instanceof FormBodyError) {
      return new NextResponse(error.message, {
        status: error.status,
        headers: { "Cache-Control": "no-store" },
      });
    }
    throw error;
  }
  const submitted = form.get("token") ?? "";
  const locale = pickLocale(form.get("locale") ?? localeFromQuery);
  if (submitted.length > MAX_SECRET_CHARS) {
    return new NextResponse("Token too long", {
      status: 413,
      headers: { "Cache-Control": "no-store" },
    });
  }

  const valid = await safeEqual(submitted, env.MAINTAINER_TOKEN);
  if (!valid) {
    return NextResponse.redirect(new URL(`/${locale}/admin?err=1`, req.url), {
      status: 303,
      headers: { "Cache-Control": "no-store" },
    });
  }

  const sid = await createSession(env.CURATED_KV, env.MAINTAINER_TOKEN);
  if (!sid) {
    return new NextResponse("Session storage unavailable", {
      status: 503,
      headers: { "Cache-Control": "no-store" },
    });
  }

  const res = NextResponse.redirect(new URL(`/${locale}/admin`, req.url), {
    status: 303,
    headers: { "Cache-Control": "no-store" },
  });
  res.cookies.set("mt_sid", sid, {
    path: "/",
    httpOnly: true,
    secure: true,
    sameSite: "strict",
    maxAge: 60 * 60 * 24,
  });
  return res;
}
