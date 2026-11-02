<!-- source: README.md sha256:0430d94f3d47 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/wordmark-inverted.svg">
  <img src="../../brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**وكيل برمجة مفتوح المصدر لطرفيتك يعمل مع أي نموذج.**

أعطِ Codewhale مهمة فيقرأ المستودع ويعدّل الملفات ويشغّل الأوامر إلى أن تكتمل
المهمة أو يحتاج إلى مُدخل منك. يعمل مع أكثر من 40 مزوّدًا مدمجًا، وDeepSeek V4.1 Flash هو
الخيار الافتراضي، ومع نماذج تعمل على جهازك. ويسرد `/receipts` الملفات التي غيّرها
والأوامر التي شغّلها واستدعاءات الويب وMCP التي أجراها.

[![CI](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[الموقع](https://codewhale.net) · [التوثيق](../README.md) · [سجل التغييرات](../../CHANGELOG.md) · [المساهمة](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [Català](README.ca.md)

<img src="../../web/public/codewhale-tui-0102-demo.png" alt="جلسة Codewhale في الطرفية" width="760">

<sub>نسخة تطوير من 0.10.2 مع نموذج اختبار ذي ردود مُعدّة مسبقًا، بإذن Full Access داخل مساحة عمل مؤقتة. أما التعديل والاختبارات الثلاثة ومراجعة الوكيل الفرعي فقد جرت فعلًا.</sub>

</div>

<p align="center">
  <a href="https://www.orcarouter.ai/redeem/1a6250545fdc4e0aaccb0a4470159e67" rel="sponsored">
    <img src="../../web/public/codewhale-x-orcarouter.webp" alt="Codewhale × OrcaRouter — استخدام إضافي بنسبة 30% لـ DeepSeek v4.1 Flash" width="760">
  </a>
</p>

[اطّلع على عرض Codewhale × OrcaRouter](https://www.orcarouter.ai/redeem/1a6250545fdc4e0aaccb0a4470159e67). تحصل Codewhale على عمولة إحالة من OrcaRouter.

## التثبيت

على macOS وLinux:

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

يضع المثبّت `codewhale` واسمه البديل المختصر `codew` في `~/.local/bin` بعد التحقق
منهما بمجموع الاختبار (checksum). إذا أظهرت الصدفة بعد ذلك `command not found`،
فشغّل سطر PATH الذي يطبعه المثبّت ([التفاصيل](../INSTALL.md#put-it-on-your-path)).
ويرقّيه الأمر `codewhale update`. تثبّت هذه الأوامر أحدث إصدار. أما هذه الصفحة
فتتبع `main`، لذا فالميزة المعلَّمة برقم إصدار تصل في ذلك الإصدار؛ ويسرد
[سجل التغييرات](../../CHANGELOG.md) ما يضيفه كل إصدار.

<details>
<summary><b>Windows وnpm وCargo وطرق تثبيت أخرى</b></summary>

```bash
winget install HunterBown.CodeWhale  # Windows x64 (or Scoop, or the installer from GitHub Releases)
npm install -g codewhale            # wraps the same release binaries
cargo install codewhale-cli --locked  # crates.io; can trail GitHub Releases
```

تتوفر ملفات ثنائية جاهزة لأنظمة Linux وmacOS وWindows على x64 وarm64. وتجد Nix
وHomebrew على Linux وAndroid/Termux (معاينة) والتنزيل اليدوي في
[دليل التثبيت](../INSTALL.md)، والحاويات في [Docker](../DOCKER.md). التزم بطريقة
تثبيت واحدة لكل جهاز؛ وإلا ستوجد نسختان على `PATH` وتُستخدم النسخة التي تأتي أولًا.

</details>

## البدء السريع

1. احفظ عملك بعمل commit أو stash، ثم شغّل `codewhale` من جذر مستودع git.
   افتراضيًا يطبّق تعديلات الملفات هناك دون أن يسأل، ويعرض لك الفروقات (diff).
2. شغّل `/provider` (أو اضغط `F3`)، واختر مزوّدًا، والصق مفتاح API الخاص به، ثم
   اختر نموذجًا. DeepSeek هو المزوّد الافتراضي. إن لم يكن لديك مفتاح، فاختر بيئة
   تشغيل محلية بدلًا من ذلك. وإذا لم يكن أي مزوّد مُعدًّا وكان Ollama يقدّم نموذج
   محادثة بالفعل، يستخدمه Codewhale تلقائيًا؛ وبدءًا من 0.10.2 يعرضه عليك عندما
   ترسل رسالتك الأولى.
3. اكتب مهمة:

```text
Fix the failing tests and explain what changed.
```

تعمل الأوامر المخصصة للقراءة فقط فورًا. أما أي أمر صدفة آخر فيسأل Codewhale عنه
أولًا: `y` يشغّله مرة واحدة، و`a` يسمح بالأوامر المشابهة له لبقية المحادثة، و`n`
يرفضه. ويُلغي `/undo` آخر طلب لك وتغييرات الملفات الناتجة عنه. ويسرد `/help` الأوامر واختصارات لوحة المفاتيح.

في السكربتات وCI، يشغّل `codewhale exec` مهمة واحدة دون الواجهة. افتراضيًا،
يستخدم `--auto` وضع Full Access: تعمل الأدوات دون سؤال، وتعمل أوامر الصدفة دون
صندوق حماية Codewhale، ويتيح وضع الثقة لأدوات الملفات الكتابة خارج مساحة العمل.
أضف `--sandbox workspace-write` للإبقاء على صندوق حماية الصدفة وقيود مساحة العمل
على أدوات الملفات، مع تعطيل وضع الثقة:

```bash
codewhale exec --auto --sandbox workspace-write "fix the failing tests and explain what changed"
```

## الأوضاع والأذونات

يبدأ التثبيت الجديد في وضع Work مع إذن Ask.

| | التبديل عبر | الخيارات |
| --- | --- | --- |
| الوضع (Mode) | `Tab` (عندما يكون حقل الإدخال فارغًا) أو `/mode` | في **Plan** يقرأ ويبحث لكنه لا يغيّر شيئًا. وفي **Work** يعدّل الملفات ويشغّل الأوامر. أما **Operate** فله صلاحيات Work نفسها، ويعامل طلبك على أنه هدف، ويواصل العمل حتى يُتحقَّق من الهدف أو يحتاج إلى مُدخل منك. |
| الإذن (Permission) | `Shift+Tab` | مع **Ask** تُطبَّق تعديلات الملفات في مستودعك دون سؤال، ويسأل قبل أوامر الصدفة التي ليست للقراءة فقط. ومع **Auto-Review** تعتمد القواعد ونموذج مراجِع الاستدعاءات الروتينية، ويرفض هذا الإذن الاستدعاءات عالية الخطورة؛ انتقل إلى Ask لتوافق عليها بنفسك. أما **Full Access** فلا يسأل أبدًا ويوقف صندوق حماية Codewhale؛ وتظل قواعد المستودع والسياسة المُدارة سارية. |

راجع [دليل الأوضاع والأذونات](../MODES.md) للتفاصيل.

## ما الذي يفعله

### الجديد في 0.10.2

- يحوّل `/pet on` طرفيتك إلى حوت متحرك يُظهر ما يفعله الوكيل، بينما يبقى مربع
  الرسائل قابلًا للاستخدام. وتكتب جسيماته البالغ عددها 980 الخطوة الحالية
  بالإنجليزية أو الصينية أو اليابانية.
- يعرض `/workbar terminal` الصدفات التي يشغّلها الوكيل أثناء عملها. اكتب في
  إحداها، أو اضغط `Ctrl+N` لتفتح صدفة خاصة بك (macOS وLinux).
- يتراجع `/undo` عن آخر طلب لك كاملًا في خطوة واحدة، بما في ذلك كل ملف غيّره،
  ويعمل `/diff` في المجلدات التي لا تستخدم Git.
- عندما تكون الخطة جاهزة، يسألك Codewhale هل تبدأ Work بإذن Ask أو Auto-Review،
  أم تواصل التخطيط.
- يمنح `/color` كل محادثة لونًا خاصًا بها لمربع الكتابة، ويُحفظ مع الجلسة.
- يمكن أن يستخدم `web_search` واجهة Search API الجديدة من DeepSeek: اضبط
  `[search] provider = "deepseek"` مع مفتاح DeepSeek.
- خارج Full Access، لا يعمل أبدًا أمر `curl` أو `wget` قد يرفع بيانات اعتماد دون
  موافقتك، ويُرفض الأمر الذي يُخفي محارف Unicode غير مرئية. ولم يعد `write`
  يستبدل ملفًا لم يقرأه الوكيل، أو ملفًا تغيّر بعد أن قرأه.

### النماذج

يأتي Codewhale بأكثر من 40 مزوّدًا مدمجًا، منها Anthropic وOpenAI وDeepSeek
وGoogle وMistral وMoonshot وOpenRouter وxAI. وتعمل أي نقطة نهاية متوافقة مع
OpenAI، وكذلك النماذج المحلية عبر Ollama أو vLLM أو SGLang. والنموذج الافتراضي
هو `deepseek-flash` من DeepSeek، الذي نقلته DeepSeek إلى DeepSeek-V4.1-Flash-1017
في 2026-10-17؛ ويشغّله 0.10.1 دون تحديث
([التفاصيل](../PROVIDERS.md#deepseek-v41-flash)).

ويبدّل `/model` النموذج بين دورات المحادثة، ويختار `/model auto` نموذجًا لكل دورة
(ويضبط `/router` طريقة الاختيار). ويعرض `/turn inspect` النموذج الذي استخدمته
الدورة وتكلفتها والمدة التي استغرقتها. ويتيح لك `codewhale auth chatgpt` استخدام
اشتراك ChatGPT بدلًا من مفتاح API (تجريبي). راجع [المزوّدين](../PROVIDERS.md).

### الإيصالات والتراجع

يسرد `/receipts`، أو `codewhale receipts --last` بعد الخروج، الملفات التي تغيّرت،
والأوامر التي شُغّلت، واستدعاءات الويب وMCP، والوكلاء الذين شُغّلوا، والموافقات
ومن منحها، وحالات الفشل. ويُلغي `/undo` آخر طلب لك والملفات التي غيّرها، في خطوة واحدة ومن دون وضع الثقة. ويعيد `/restore` مساحة العمل
كلها إلى لقطة سابقة؛ ولأن ذلك يكتب فوق التعديلات اللاحقة، فإنه يتطلب `/trust on`
أو Full Access. راجع [الإيصالات](../RECEIPTS.md).

تُحفظ عمليات تشغيل exec التي تتيح أدوات بكل صيغ الإخراج، لذا يعمل
`codewhale receipts --last` بعد `exec --auto`، بما في ذلك مع `--json`.
أما عمليات التشغيل التي تقتصر على الإجابة بإخراج نصي أو `--json` فلا تُحفظ إلا
عند الاستئناف أو عند بدء ضغط المحادثة؛ استخدم `--output-format stream-json`
لحفظها دائمًا من أجل الإيصالات أو `exec --continue`.

### الأعمال الأطول

يحدد `/goal` هدفًا يستمر عبر دورات المحادثة. ويستطيع Codewhale توزيع العمل على
[وكلاء فرعيين](../SUBAGENTS.md)، أو تشغيل [فريق وكلاء](../FLEET.md)، أو اتباع
[سير عمل](../WORKFLOW_AUTHORING.md) مضمَّن في المستودع.

### إن كنت قادمًا من وكيل آخر

يقرأ Codewhale ملف `AGENTS.md` من تلقاء نفسه. أما ملفات التعليمات المكتوبة لأدوات
أخرى، مثل `CLAUDE.md` أو `.cursorrules` أو `.github/copilot-instructions.md`، فلا
تُحمَّل إلا عندما تختار ذلك بإضافة `project_instruction_imports = ["claude"]` (أو
`"cursor"` أو `"cline"` أو `"windsurf"` أو `"copilot"` أو `"all"`) إلى ملف
إعداداتك؛ راجع [config.example.toml](../../config.example.toml). ويُصدر Codewhale تحذيرًا
عندما يعثر على ملف منها لم يحمّله.

يجهّز `/import-claude` استيرادًا لإعدادات Claude Code وخوادم MCP الخاصة بك لتراجعه.
ويقبل `/plugin install` إضافات Claude Code التي تحتوي على مهارات وأوامر وملفات تعريف
وكلاء وخوادم MCP، ويرفض الإضافات التي تحتاج إلى خطافات أو خوادم LSP
([التفاصيل](../CLAUDE_PLUGIN_COMPAT.md)).

### الامتدادات

اربط [خوادم MCP](../MCP.md)، وأضف [المهارات](../SKILLS.md) و[الإضافات](../PLUGINS.md)،
وشغّل [الخطافات](../HOOKS.md) عند أحداث الجلسة والأدوات. ويتيح وضع الشيفرة
(Code mode، تجريبي ومفعّل افتراضيًا) للنموذج كتابة برنامج `execute_tools` واحد
يستدعي عدة أدوات من MCP والإضافات؛ ويظل كل استدعاء خاضعًا لقواعد الموافقة لديك
ويحتفظ بإيصاله. وتستطيع إضافة Computer Use
المثبّتة بشكل مستقل (تجريبية، لنظام macOS فقط) مراقبة التطبيقات الأخرى والتحكم فيها. وتبقى
معطّلة إلى أن تراجع صلاحيات وصولها وتفعّلها
([الدليل](https://github.com/codewhale-hq/codewhale-cu-plugin)).

## طرق التشغيل

| الأمر | ما يفعله |
| --- | --- |
| `codewhale` | تطبيق الطرفية التفاعلي |
| `codewhale exec "…"` | مهمة واحدة دون الواجهة. دون خيارات إضافية يجيب دون استخدام الأدوات؛ ويفعّل `--auto` الأدوات مع Full Access افتراضيًا، ويقيّدها `--sandbox workspace-write`، ويبث `--output-format stream-json` الأحداث بصيغة JSON |
| `codewhale web` | واجهة ويب محلية على `127.0.0.1` ([الدليل](../WEB.md)) |
| `codewhale review --pr N` | مراجعة استشارية لطلب pull request على GitHub. يُنشئ [إعداد بوت GitHub](../GITHUB_ACTION.md#set-up-your-own-bot) سير عمل مثبّت الإصدار للمراجعات التلقائية و`@codewhale review`، باستخدام مفتاح النموذج الخاص بك وهوية App اختيارية |
| `codewhale app-server --http` | واجهة [Runtime API](../RUNTIME_API.md)، وهي واجهة HTTP محلية للخيوط والأحداث والموافقات وتغييرات الملفات في كل دورة |
| `codewhale serve --acp` | بروتوكول Agent Client Protocol عبر stdio، لمحررات مثل Zed |

يتشارك تطبيق الطرفية و`exec` و`web` وRuntime API محركًا واحدًا
([البنية](../ARCHITECTURE.md)). ويتصل به من الشريط الجانبي
[امتداد VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)
الذي يصونه المجتمع ([المصدر](https://github.com/HengQuWorld/CodeWhale-VSCode)).
ويجري تطوير تطبيق أصلي لسطح المكتب مبني على Runtime API؛ ولا يتوفر له تنزيل عام
بعد.

## التكلفة والحسابات

يُتاح Codewhale مجانًا بترخيص MIT. عندما تستخدم مفتاح API الخاص بك تدفع للمزوّد
مباشرة؛ ولا تحتاج النماذج المحلية إلى مفتاح. ويعرض الشريط السفلي و`/cost` تكلفة
الجلسة عندما يكون السعر معروفًا، ويُعرض السعر غير المعروف على أنه غير معروف، ولا
يُعرض أبدًا على أنه مجاني.

لا تحتاج إلى حساب. وتبقى المفاتيح التي تضيفها عبر `/provider` على جهازك.

## الأمان والخصوصية

في Ask وAuto-Review تعمل الأوامر داخل صندوق حماية نظام التشغيل حيثما يوفّر النظام
واحدًا: Seatbelt على macOS، وbubblewrap على Linux عندما يكون مثبّتًا. لا يوجد على
Windows صندوق حماية لنظام التشغيل بعد، ويوقف Full Access صندوق الحماية. وداخل
صندوق الحماية لا تستطيع أوامر الصدفة قراءة مخازن بيانات الاعتماد مثل `~/.ssh`
و`~/.aws` وملفات `.env`؛ وبدءًا من 0.10.2 تشمل القاعدة نفسها جلسات الطرفية ومزيدًا
من أدوات الملفات. ويعرض `/preview-request` معاينة للطلب التالي بعد حجب بياناته الحساسة (redacted)، دون
إرساله.

القياس عن بُعد مفعّل افتراضيًا. يرسل أعدادًا مجمّعة (الإصدار والمنصة، ومدة الجلسة
ونتيجتها، والمزوّدين المدمجين الذين استخدمتهم، وأعداد الميزات والأخطاء) مع معرّف
تثبيت عشوائي يتغيّر كل 90 يومًا، ولا يرسل أبدًا المحادثات أو الشيفرة أو الموجّهات
(prompts) أو أسماء النماذج أو أسماء الملفات أو المستودعات
أو بيانات الاعتماد. ويوقفه الأمر `codewhale config set telemetry false`. وعند بدء
التشغيل يتحقق Codewhale أيضًا من GitHub بحثًا عن إصدار جديد؛ اضبط
`CODEWHALE_NO_UPDATE_CHECK=1` لتخطي ذلك. المزيد في
[ترتيب التفويض](../AUTHORIZATION_ORDER.md) و[صندوق الحماية](../SANDBOX.md)
و[القياس عن بُعد](../TELEMETRY.md).

## التوثيق

| ابدأ من هنا | المرجع |
| --- | --- |
| [التثبيت](../INSTALL.md) | [الإعدادات](../CONFIGURATION.md) |
| [المزوّدون والنماذج المحلية](../PROVIDERS.md) | [البنية](../ARCHITECTURE.md) |
| [الأوضاع والأذونات](../MODES.md) | [Runtime API](../RUNTIME_API.md) |
| [اختصارات لوحة المفاتيح](../KEYBINDINGS.md) | [تأليف الإضافات](../PLUGIN_AUTHORING.md) |
| [مراجعة PR على GitHub](../GITHUB_ACTION.md) | [كل التوثيق](../README.md) |

## المجتمع

إذا كان هناك مزوّد مفقود أو سير عمل غير مريح،
[فافتح issue](https://github.com/codewhale-hq/CodeWhale/issues/new/choose) أو
[أرسل pull request](../../CONTRIBUTING.md). يحتفظ المساهمون بالفضل في العمل الذي يُدمج،
وتوضّح [بنية المستودع](../../CONTRIBUTING.md#project-structure) مكان كل شيء.

تواصل معنا على [Discord](https://discord.gg/37gfS3ksug)، أو أضف القائم على صيانة
المشروع، Hunter، على WeChat (`hunterbown`) واطلب الانضمام إلى مجموعة Whale Brothers.

## التاريخ والترخيص

بدأ Codewhale باسم `deepseek-tui` ولا يزال يقرأ إعدادات ذلك المشروع وجلساته. ويُصان
باستقلال، وليس تابعًا لـ DeepSeek ولا لأي مزوّد نماذج آخر. شكرًا
لـ[كل من ساهم](../CONTRIBUTORS.md).

[![Codewhale contributors](../assets/contributors.png)](../CONTRIBUTORS.md#everyone)

الترخيص: [MIT](../../LICENSE). والشيفرة المقتبسة من مشاريع مفتوحة المصدر أخرى مُدرجة في
[إشعارات الأطراف الثالثة](../THIRD_PARTY_NOTICES.md).
