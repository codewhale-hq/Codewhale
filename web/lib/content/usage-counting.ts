/**
 * usage-counting.ts — the website's own explanation of usage
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
    en: "This site counts page views, documentation views, install-command copies, sign-in link clicks and error pages shown, along with visits to the home, install, quickstart and contribution pages, clicks on links to Codewhale’s GitHub stars, issues, help-wanted work and contributing guide, documentation searches, searches with no result, selected search results, and your own confirmations that you installed Codewhale or checked a quickstart answer. Each batch of counts is keyed on a random install id that this site stores in this browser and replaces every 90 days. The id is not derived from you or your device, but it does tie together the counts sent from this browser while it lasts, so these are not anonymous totals. A batch also carries the site’s version number and the time it was sent. No page address, search text, referrer, account or content is stored, and the batch goes only to Codewhale’s own counting service. Counts are not unique visitors, and your confirmations are self-reports, not proof that an installation or a task succeeded.",
    zh: "本站会统计页面浏览、文档浏览、安装命令复制、登录链接点击及错误页面显示次数，同时统计首页、安装、快速入门和贡献页面的浏览，指向 Codewhale 的 GitHub 星标、议题、求助事项和贡献指南的链接点击，文档搜索、无结果搜索、搜索结果选择，以及你对已安装 Codewhale 或已核对快速入门答案的确认。每一批计数都以一个随机安装 id 为键；该 id 由本站保存在此浏览器中，每 90 天更换一次。它不是由你或你的设备推算出来的，但在有效期内会把此浏览器发送的计数关联在一起，因此这些并不是匿名总数。每一批还带有本站的版本号和发送时间。不会保存页面地址、搜索词、来源、账户或内容，批次只会发送给 Codewhale 自己的统计服务。计数不等于独立访客，你的确认属于自行报告，不是安装或任务成功的证明。",
  },
  choice: {
    en: "This site counts nothing, and creates and reads no install id, while your browser sends Global Privacy Control or Do Not Track; it also clears any counts and install id that an earlier visit saved. You can turn counting off for this browser below. That choice is kept in this browser, clears any queued counts and the install id, and is never silently reversed. Nothing here records that you accepted anything.",
    zh: "只要你的浏览器发送全局隐私控制（Global Privacy Control）或“请勿跟踪”（Do Not Track）信号，本站就不统计任何内容，也不创建或读取安装 id，并会清除之前访问留下的计数和安装 id。你也可以在下方为此浏览器关闭统计。该选择保存在此浏览器中，会清除已排队的计数和安装 id，且不会被悄悄撤销。这里不会记录你同意了任何内容。",
  },
  status: {
    unsaved: { en: "Your preference could not be saved. Counting is off for this page; reloading may restore the previous preference.", zh: "未能保存你的选择。此页面已关闭统计；重新加载后可能恢复之前的设置。" },
    default: { en: "Usage counting is on (default).", zh: "使用统计已开启（默认）。" },
    on: { en: "Usage counting is on.", zh: "使用统计已开启。" },
    off: { en: "Usage counting is off for this browser.", zh: "此浏览器已关闭使用统计。" },
    signal: { en: "Your browser sends Global Privacy Control or Do Not Track, so this site counts nothing and keeps no install id.", zh: "你的浏览器发送了全局隐私控制或“请勿跟踪”信号，因此本站不统计任何内容，也不保存安装 id。" },
    unavailable: { en: "This browser has no site storage available, so no counts are kept and there is nothing to turn off.", zh: "此浏览器没有可用的站点存储，因此不会保留任何计数，也没有可关闭的项。" },
  },
  turnOff: { en: "Turn off", zh: "关闭" },
  turnOn: { en: "Turn on", zh: "开启" },
  elsewhere: {
    en: "In the Codewhale program, usage reporting is on by default. In the app, use Settings → Usage data. In the terminal, run codewhale config set telemetry false to turn it off, or set CODEWHALE_TELEMETRY=0 to turn it off for a single run. The installer itself sends no usage report.",
    zh: "在 Codewhale 程序中，使用情况上报默认开启。在应用中，使用设置 → 使用数据。在终端中，执行 codewhale config set telemetry false 将其关闭，或设置 CODEWHALE_TELEMETRY=0 仅在单次运行中关闭。安装程序本身不会发送使用情况报告。",
  },
} as const satisfies Record<string, LocalizedText | Record<string, LocalizedText>>;
