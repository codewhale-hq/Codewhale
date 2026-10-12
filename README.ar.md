<!-- source: README.md sha256:75bb6a506af5 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="brand/wordmark-inverted.svg">
  <img src="brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**وكيل البرمجة مفتوح المصدر الذي يعمل مع أي نموذج.**

يقرأ Codewhale مشروعك ويعدّل الملفات ويشغّل الأوامر ويتحقق من عمله بنفسه — في طرفيتك،
باستخدام نموذج مستضاف أو محلي تختاره.

[![CI](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[الموقع](https://codewhale.net) · [التوثيق](docs/README.md) · [سجل التغييرات](CHANGELOG.md) · [المساهمة](CONTRIBUTING.md)

[English](README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [Català](README.ca.md)

<img src="web/public/codewhale-tui-0102-demo.png" alt="جلسة Codewhale في الطرفية" width="760">

<sub>لقطة حقيقية لطرفية Codewhale من جلسة عرض محلية مُهيّأة.</sub>

</div>

## التثبيت

macOS وLinux:

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

ينزّل المثبّت ملفات ثنائية متحقَّقًا من مجموع اختبارها إلى `~/.local/bin`. إذا ظهرت
بعد ذلك رسالة "command not found" عند تشغيل `codewhale`، فشغّل سطر PATH الواحد الذي
يطبعه المثبّت، أو راجع [إضافته إلى PATH](docs/INSTALL.md#put-it-on-your-path).
يمكنك الترقية في أي وقت بالأمر `codewhale update`.

<details>
<summary><b>Windows وnpm وCargo وطرق أخرى</b></summary>

```bash
winget install HunterBown.CodeWhale  # Windows x64 (or Scoop, or the installer from GitHub Releases)
npm install -g codewhale            # wraps the same release binaries
cargo install codewhale-cli --locked  # build from crates.io
```

تغطي [دليل التثبيت](docs/INSTALL.md) طرق Docker وNix وHomebrew على Linux
وAndroid/Termux والتنزيل اليدوي مع التحقق من مجموع الاختبار ومرآة CNB الاختيارية.
اختر طريقة واحدة: تتنازع عدة عمليات تثبيت على جهاز واحد على `PATH`.

</details>

## البدء السريع

**مفاتيح API لمزوّديك، في مكان واحد.** [أنشئ حساب Codewhale](https://app.codewhale.net/register)
أو شغّل `codewhale login` لتسجيل الدخول. احفظ مفاتيح API وحدّثها في حسابك،
ثم استخدمها عبر مسار النماذج في Codewhale على الأجهزة التي سجّلت الدخول عليها.
[إعداد مفاتيح الحساب](docs/CONFIGURATION.md#account-provider-keys).
تسجيل الدخول لا يرفع المفاتيح المحلية الموجودة؛ ويمكنك مواصلة الاستخدام المحلي دون حساب.

1. **افتح مشروعك.** شغّل `codewhale` في المجلد الذي تريد العمل عليه.
2. **اربط نموذجًا.** شغّل `/provider` (أو اضغط `F3`) لإضافة مفتاح مستضاف أو اختيار
   بيئة تشغيل محلية. إذا كان Ollama يعمل بالفعل مع نموذج محادثة، ينتقل Codewhale إليه
   من تلقاء نفسه. استخدم `/model` لتغيير النموذج.
3. **أعطه مهمة محددة.**

```text
Fix the failing tests and explain what changed.
```

تعمل المهمة نفسها دون واجهة من سكربت أو من مهمة CI:

```bash
codewhale exec --auto "fix the failing tests and explain what changed"
```

شغّل `/help` لعرض الأوامر واختصارات لوحة المفاتيح.

## طرق التشغيل

يتعامل كل عميل مع Codewhale Runtime المحلي نفسه، لذا تتصرف الجلسات والأدوات
والأذونات بالطريقة نفسها في كل مكان.

| الأمر | ما يفعله |
| --- | --- |
| `codewhale` | واجهة الطرفية التفاعلية |
| `codewhale exec "…"` | دورة واحدة دون واجهة من سكربت أو CI؛ يسمح `--auto` بأدوات الملفات والصدفة، ويبث `--output-format stream-json` بيانات JSON |
| `codewhale web` | [عميل المتصفح المحلي](docs/WEB.md) المضمَّن على `127.0.0.1` |
| `codewhale review --pr N` | [مراجعة pull request](docs/GITHUB_ACTION.md) استشارية؛ والنشر اختياري |
| Runtime API | [واجهة HTTP محلية](docs/RUNTIME_API.md) للخيوط والأحداث والموافقات |

يجري تطوير تطبيق سطح مكتب أصلي (GPUI) ليكون عميل المنتج المسجَّل الدخول؛ راجع
[صفحة المنتج](https://codewhale.net/en/product) لمعرفة حالة الإتاحة. ويتصل
[امتداد VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)
الذي يصونه المجتمع بـ Runtime نفسه من شريط جانبي ([المصدر](https://github.com/HengQuWorld/CodeWhale-VSCode)).

## ما الذي يفعله

- **أي نموذج دون ارتهان.** أكثر من 40 مسار مزوّد مدمجًا — Anthropic وDeepSeek
  وGoogle وMistral وMoonshot وOpenAI وOpenRouter وxAI وغيرها — إضافة إلى أي نقطة
  نهاية متوافقة مع OpenAI ونماذج محلية عبر Ollama أو vLLM أو SGLang.
  [المزوّدون](docs/PROVIDERS.md)
- **تبقى السيطرة بيدك.** يستكشف وضع Plan دون تغيير أي شيء؛ ويُجري Work وOperate
  تغييرات. تحدد أوضاع الموافقة متى يحتاج استدعاء الأداة إلى موافقتك، ويستعيد `/undo`
  و`/restore` تغييرات مساحة العمل، ويسرد `/receipts` كل ملف وأمر وموافقة في الجلسة.
  [الأوضاع](docs/MODES.md) · [الإيصالات](docs/RECEIPTS.md)
- **مصمَّم للأعمال الطويلة.** حدّد `/goal` دائمًا، وفوّض الأعمال المحدودة إلى
  [الوكلاء الفرعيين](docs/SUBAGENTS.md)، وشغّل [فرق وكلاء](docs/FLEET.md) خاضعة
  للإشراف مع فحص قبل الإنفاق، أو اكتبها كسكربتات في
  [سير عمل](docs/WORKFLOW_AUTHORING.md) مضمَّنة في المستودع.
- **وسّع ما تستخدمه أصلًا.** اربط [خوادم MCP](docs/MCP.md)، وثبّت
  [المهارات](docs/SKILLS.md) و[الإضافات](docs/PLUGINS.md)، وشغّل
  [الخطافات](docs/HOOKS.md) عند أحداث الجلسة والأدوات، وحمّل
  [إضافات Claude Code](docs/CLAUDE_PLUGIN_COMPAT.md) الموجودة.
- **Computer Use.** تضيف إضافة مضمَّنة أدوات لمراقبة التطبيقات الأخرى وتشغيلها.
  راجع صلاحيات وصولها وفعّلها قبل الاستخدام.
  [الدليل](crates/tui/plugins/computer-use/README.md)

## الأوضاع والأذونات

| | طريقة الاختيار | الخيارات |
| --- | --- | --- |
| **Mode** — ما يفعله الوكيل | `Tab` أو `/mode` | Plan (استكشاف دون تغييرات) · Work (تعديل وتشغيل) · Operate (قيادة هدف عبر خطوات مخططة ومتحقق منها) |
| **Posture** — متى يسأل أولًا | `Shift+Tab` | Ask · Auto-Review · Full Access |

يلتزم Full Access أيضًا بحدود السياسة الصارمة.
يشرح [دليل الأوضاع والأذونات](docs/MODES.md) كل خيار.

## الأمان

يعمل Codewhale على جهازك بالصلاحيات التي تمنحها له. تحدّ أوضاع الموافقة وقواعد
المستودع مما يجوز للوكيل فعله، وتعمل الأوامر داخل صندوق حماية لنظام التشغيل حيثما
كان مدعومًا (Seatbelt على macOS؛ وbubblewrap على Linux عند تثبيته وعمله). يعرض
`/preview-request` الطلب المحجوب بياناته الحساسة (redacted) كما هو تمامًا قبل إرسال أي شيء.
تبقى أسعار النماذج غير المعروفة غير معروفة بدل أن تُعرض على أنها مجانية.

راجع [ترتيب التفويض](docs/AUTHORIZATION_ORDER.md) و[صندوق الحماية](docs/SANDBOX.md)
و[القياس عن بُعد](docs/TELEMETRY.md) — أعداد الاستخدام مفعّلة افتراضيًا، ويوقفها
الأمر `codewhale config set telemetry false`.

## التوثيق

| ابدأ من هنا | تعمّق أكثر |
| --- | --- |
| [التثبيت](docs/INSTALL.md) | [الإعدادات](docs/CONFIGURATION.md) |
| [المزوّدون والنماذج المحلية](docs/PROVIDERS.md) | [البنية](docs/ARCHITECTURE.md) |
| [الأوضاع والأذونات](docs/MODES.md) | [Runtime API](docs/RUNTIME_API.md) |
| [اختصارات لوحة المفاتيح](docs/KEYBINDINGS.md) | [تأليف الإضافات](docs/PLUGIN_AUTHORING.md) |
| [مراجعة PR على GitHub](docs/GITHUB_ACTION.md) | [كل التوثيق](docs/README.md) |

## المجتمع

نرحّب ببلاغات الأخطاء وأفكار الميزات وطلبات pull request — سواء كنت تستخدم
Codewhale منذ أشهر أو تجربه للمرة الأولى. إذا كان هناك مزوّد مفقود أو سير عمل
غير مريح، فـ[افتح issue](https://github.com/codewhale-hq/CodeWhale/issues/new/choose)
أو [أرسل pull request](CONTRIBUTING.md). المساهمات الأولى مرحَّب بها، ويحتفظ
المساهمون بفضل العمل الذي يُقبل. و[بنية المستودع](CONTRIBUTING.md#project-structure)
مكان جيد للبدء.

انضم إلى [Discord](https://discord.gg/37gfS3ksug)، أو أضف Hunter على WeChat
(`hunterbown`) واطلب الانضمام إلى مجموعة Whale Brothers.

## التاريخ والترخيص

بدأ Codewhale باسم `deepseek-tui` ولا يزال يقرأ إعدادات ذلك المشروع وجلساته. وهو
الآن محايد تجاه المزوّدين ويُصان باستقلال، وغير تابع لأي مزوّد نماذج. شكرًا
لـ[كل المساهمين](docs/CONTRIBUTORS.md) ولمجتمعات المصادر المفتوحة التي ساعدت في نموه.

[MIT](LICENSE). الأجزاء المقتبسة من مشاريع مفتوحة المصدر أخرى مسجَّلة في
[إشعارات الأطراف الثالثة](docs/THIRD_PARTY_NOTICES.md).
