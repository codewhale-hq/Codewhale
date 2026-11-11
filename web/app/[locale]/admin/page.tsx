import type { Metadata } from "next";
import { cookies } from "next/headers";
import { ConnectionBanner } from "@/components/connection-banner";
import { EmptyState, ErrorState } from "@/components/surface-state";
import { WhalePose } from "@/components/whale-pose";
import { getAgentEnv, listDrafts, validateSession, type AgentDraft } from "@/lib/community-agent";
import { AdminClient } from "./admin-client";

export const dynamic = "force-dynamic";

// Maintainer-only surface: keep it out of search indexes (robots.ts also
// disallows /*/admin).
export const metadata: Metadata = {
  title: "Maintainer panel · Codewhale",
  robots: { index: false, follow: false },
  alternates: {},
  openGraph: { title: "Maintainer panel · Codewhale" },
};

const TYPE_LABELS: Record<string, { en: string; zh: string }> = {
  digest: { en: "Weekly Digest", zh: "每周摘要" },
  linkcheck: { en: "Broken Link", zh: "失效链接" },
  "semantic-drift": { en: "Content Drift", zh: "内容漂移" },
};

function LoginForm({ locale, error }: { locale: string; error: boolean }) {
  const isZh = locale === "zh";
  return (
    <div className="account-entry">
      <ConnectionBanner locale={locale} />
      <WhalePose pose="listen" className="account-entry-pose" />
      <h1 className="page-title">
        {isZh ? "维护者登录" : "Maintainer login"}
      </h1>
      <form method="POST" action={`/api/admin/login?locale=${locale}`} autoComplete="off" className="account-form">
        <input type="hidden" name="locale" value={locale} />
        <label className="field">
          <span className="field-label">{isZh ? "令牌" : "Token"}</span>
          <input
            type="password"
            name="token"
            required
            autoFocus
            autoComplete="off"
            spellCheck={false}
            aria-invalid={error || undefined}
            aria-describedby={error ? "admin-token-error" : undefined}
            className="field-input font-mono"
          />
        </label>
        <button
          type="submit"
          className="btn btn-primary btn-lg"
        >
          {isZh ? "登录" : "Sign in"}
        </button>
        {error && (
          <p id="admin-token-error" className="field-hint field-hint-error" role="alert">
            {isZh ? "令牌错误。" : "Invalid token."}
          </p>
        )}
      </form>
    </div>
  );
}

export default async function AdminPage({
  params,
  searchParams,
}: {
  params: Promise<{ locale: string }>;
  searchParams: Promise<{ err?: string }>;
}) {
  const { locale } = await params;
  const { err } = await searchParams;
  const isZh = locale === "zh";

  const env = await getAgentEnv();

  if (!env.MAINTAINER_TOKEN) {
    return (
      <div className="route-state">
        <ConnectionBanner locale={locale} />
        <div className="mt-6" />
        <ErrorState
          locale={locale}
          title={isZh ? "未配置" : "Not configured"}
          titleAs="h1"
          body={
            isZh
              ? "此页面当前不可用。"
              : "This page is not available right now."
          }
        />
      </div>
    );
  }

  const cookieStore = await cookies();
  const sid = cookieStore.get("mt_sid")?.value;
  const authed = await validateSession(env.CURATED_KV, sid);

  if (!authed) {
    return <LoginForm locale={locale} error={err === "1"} />;
  }

  let drafts: AgentDraft[] = [];
  try {
    drafts = await listDrafts(env.CURATED_KV);
  } catch (e) {
    console.error("failed to list drafts", e);
  }

  const pending = drafts.filter((d) => !d.posted);
  const posted = drafts.filter((d) => d.posted);

  return (
    <section className="page-body admin-page">
      {/* The signed-in shell: typed offline / reconnect state with a real
          server probe, so a paused action is never mistaken for a posted one. */}
      <ConnectionBanner locale={locale} />
      <div className="admin-head">
        <div>
          <h1 className="page-title">
            {isZh ? "社区助理草稿" : "Community Assistant Drafts"}
          </h1>
          <p className="page-meta tabular">
            {pending.length} pending · {posted.length} posted
          </p>
        </div>
        <form method="POST" action={`/api/admin/logout?locale=${locale}`}>
          <button
            type="submit"
            className="btn btn-ghost btn-sm"
          >
            {isZh ? "退出" : "Sign out"}
          </button>
        </form>
      </div>

      {pending.length === 0 && posted.length === 0 && (
        <EmptyState
          locale={locale}
          title={isZh ? "暂无草稿" : "No drafts yet"}
          body={
            isZh
              ? "有新草稿时会显示在这里。"
              : "New drafts appear here when there are any."
          }
        />
      )}

      <AdminClient
        drafts={pending}
        posted={posted}
        isZh={isZh}
        typeLabels={TYPE_LABELS}
      />
    </section>
  );
}
