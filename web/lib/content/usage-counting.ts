/**
 * usage-counting.ts — the website's own explanation of anonymous usage
 * counting, shown on the privacy page beside the opt-out control.
 *
 * Counting is on by default. The copy names exactly what is counted, where
 * it goes (Codewhale's own endpoint, which keeps current website batches first-party), what never enters the batch, and how to turn it
 * off. It never claims that the person agreed to anything.
 */

import type { LocalizedText } from "./vocabulary";

export const USAGE_COUNTING_COPY = {
  footerLink: { en: "Usage data", zh: "使用数据" },
  heading: { en: "Usage counting on this site", zh: "本站的使用统计" },
  summary: {
    en: "This site counts page views, documentation views, install-command copies, sign-in link clicks, and error pages shown as plain totals, along with visits to the home, install, quickstart and contribution pages, contribution-link clicks, settled documentation searches, empty searches, selected results and your install or answer-check confirmations. Counting is on by default; no page addresses, search queries, referrers, account or content enter the batch, and its random browser identifier rotates every 90 days. Current website batches stay with Codewhale’s first-party service and are not forwarded to PostHog; older supported batches may use that processor. Your confirmations are self-reports, not automatic proof that installation or a task succeeded, and this browser is not linked to your native installation.",
    zh: "本站会把页面浏览、文档浏览、安装命令复制、登录链接点击及错误页面显示次数作为纯总数统计，同时统计首页、安装、快速入门和贡献页面的浏览、贡献链接点击、输入停顿后的文档搜索、无结果搜索、结果选择，以及你对安装或回答检查的确认。统计默认开启；批次不包含页面地址、搜索词、来源、账户或内容，随机浏览器标识每 90 天轮换一次。当前网站批次只发送给 Codewhale 的第一方服务，不会转发给 PostHog；旧版受支持批次可能使用该处理方。你的确认属于自行报告，不是安装或任务成功的自动证明，也不会把此浏览器与本机安装关联。",
  },
  choice: {
    en: "Turning it off is kept in this browser and clears any queued counts and the install id. Nothing here records that you accepted anything.",
    zh: "关闭后的选择保存在此浏览器中，并会清除已排队的计数和安装 id。这里不会记录你同意了任何内容。",
  },
  status: {
    unsaved: { en: "Your preference could not be saved. Counting is off for this page; reloading may restore the previous preference.", zh: "未能保存你的选择。此页面已关闭统计；重新加载后可能恢复之前的设置。" },
    default: { en: "Anonymous usage counting is on (default).", zh: "匿名使用统计已开启（默认）。" },
    on: { en: "Anonymous usage counting is on.", zh: "匿名使用统计已开启。" },
    off: { en: "Anonymous usage counting is off for this browser.", zh: "此浏览器已关闭匿名使用统计。" },
    unavailable: { en: "This browser has no site storage available, so no counts are kept and there is nothing to turn off.", zh: "此浏览器没有可用的站点存储，因此不会保留任何计数，也没有可关闭的项。" },
  },
  turnOff: { en: "Turn off", zh: "关闭" },
  turnOn: { en: "Turn on", zh: "开启" },
  elsewhere: {
    en: "In the Codewhale app, use Settings → Usage data. In the terminal runtime, run codewhale config set telemetry false or set CODEWHALE_TELEMETRY=0.",
    zh: "在 Codewhale 应用中，使用设置 → 使用数据。在终端运行时中，执行 codewhale config set telemetry false 或设置 CODEWHALE_TELEMETRY=0。",
  },
} as const satisfies Record<string, LocalizedText | Record<string, LocalizedText>>;
