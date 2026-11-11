/**
 * account-entry.ts — copy for the public sign-in page.
 *
 * codewhale.net is not the signed-in app; this page sends people to
 * app.codewhale.net, says that cloud computers are invite-only and that the
 * terminal needs no account (CURRENT_DECISIONS §132).
 */

import type { LocalizedText } from "./vocabulary";

export const ACCOUNT_ENTRY_COPY = {
  signIn: {
    metaTitle: { en: "Sign in · Codewhale", zh: "登录 · Codewhale" },
    kicker: { en: "Sign in", zh: "登录" },
    title: { en: "Sign in to Codewhale.", zh: "登录 Codewhale 账户。" },
    action: { en: "Sign in", zh: "登录" },
  },
  lede: {
    en: "Sign in with GitHub, Google or an email code. Cloud computers are invite-only. The terminal and codewhale web need no account.",
    zh: "使用 GitHub、Google 或邮箱验证码登录。云端计算机目前仅限受邀使用。终端和 codewhale web 无需账户。",
  },
  metaDescription: {
    en: "Sign in to Codewhale with GitHub, Google or an email code. Cloud computers are invite-only; the terminal needs no account.",
    zh: "使用 GitHub、Google 或邮箱验证码登录 Codewhale。云端计算机仅限受邀使用；使用终端无需账户。",
  },
  installLocally: { en: "Install locally", zh: "本机安装" },
} as const satisfies Record<string, unknown> & { lede: LocalizedText; installLocally: LocalizedText };
