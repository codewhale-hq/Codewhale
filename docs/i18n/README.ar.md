<!-- source: README.md sha256:fd76d718e3a2 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/mark-reversed.svg">
  <img src="../../brand/mark-mono.svg" alt="Codewhale" width="72" height="72">
</picture>

<h1>يمكن أن يكون النموذج ملكًا لأي أحد. أما الوكيل فينبغي أن يكون ملكًا لك.</h1>

Codewhale وكيل برمجة مفتوح المصدر يعمل في الطرفية لديك، ومكتوب بلغة Rust. يعمل مع أي نموذج، ويستطيع توزيع مهمة واحدة على عدة وكلاء، وكل سطر فيه متاح لك لتقرأه.

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

يثبّت هذا الأمر أحدث إصدار على macOS أو Linux، ولا يحتاج إلى حساب. طرق التثبيت على [Windows وnpm وCargo](#التثبيت) في الأسفل.

[![Latest release](https://img.shields.io/github/v/release/codewhale-hq/Codewhale?label=release)](https://github.com/codewhale-hq/Codewhale/releases/latest)
[![CI](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[الموقع](https://codewhale.net) · [التوثيق](../README.md) · [سجل التغييرات](../../CHANGELOG.md) · [المساهمة](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [Català](README.ca.md)

<sub>هذه الترجمة مسودة آلية ولم يراجعها بعد متحدث أصلي.</sub>

<img src="../../web/public/codewhale-tui-0102-demo.png" width="760" alt="Codewhale في الطرفية بعد مهمة تجريبية. يذكر أنه أضاف الخيار --json إلى export.py، وأن الاختبارات المحلية الثلاثة نجحت، وأن المراجعة المفوَّضة لم تجد مشكلات. تعرض قائمة المهام في الأسفل ثلاث خطوات مكتملة، ويعرض سطر الحالة Full Access والمزوّد المحلي website-demo.">

<sub>بنية تطوير من الإصدار 0.10.3، الذي لم يصدر بعد، مع نموذج اختبار مكتوب مسبقًا، في وضع Full Access داخل مساحة عمل مؤقتة. التعديل والاختبارات الثلاثة ومراجعة الوكيل الفرعي جرت فعلًا.</sub>

</div>

## التثبيت

يضع الأمر أعلاه `codewhale` الذي جرى التحقق من مجموعه الاختباري واسمه المختصر `codew` في `~/.local/bin`. إذا قالت الصدفة بعد ذلك `command not found`، فاتبع ملاحظة PATH التي يطبعها المثبّت ([التفاصيل](../INSTALL.md#put-it-on-your-path)). يحدّثه الأمر `codewhale update`.

```bash
winget install --id HunterBown.CodeWhale --exact --source winget  # Windows x64
npm install -g codewhale                                          # wraps the same release binaries
cargo install codewhale-cli --locked                              # builds from source; can trail GitHub Releases
```

تتوفر ملفات ثنائية جاهزة لأنظمة Linux وmacOS وWindows على x64 وarm64. تجد Scoop وNix وHomebrew على Linux وAndroid وTermux (معاينة) والتنزيل اليدوي في [دليل التثبيت](../INSTALL.md)، والحاويات في [Docker](../DOCKER.md). استخدم طريقة تثبيت واحدة لكل جهاز؛ وإلا ستوجد نسختان في `PATH` وتعمل التي تُعثر عليها أولًا.

## البدء السريع

1. احفظ عملك بـ commit أو stash، ثم شغّل `codewhale` من جذر مستودع git. افتراضيًا يطبّق تعديلات الملفات في المستودع دون أن يسأل، ويسأل قبل كل أمر shell لا يقتصر على القراءة.
2. شغّل `/provider` (أو اضغط `F3`)، واختر مزوّدًا، والصق مفتاح API الخاص به، واختر نموذجًا. المزوّد الافتراضي هو DeepSeek. عند أول تشغيل دون مفتاح، يسألك Codewhale عن مزوّد حين ترسل رسالتك الأولى، ويقترح نموذجًا محليًا إذا استجاب خادم Ollama محلي.
3. اكتب مهمة:

```text
Fix the failing tests and explain what changed.
```

تعمل أوامر القراءة فقط فورًا. أما أي أمر آخر في الصدفة فيسأل عنه Codewhale أولًا: `y` يشغّله مرة واحدة، و`a` يسمح بالأوامر المشابهة طوال بقية المحادثة، و`n` يرفضه. يتراجع `/undo` عن طلبك الأخير وتغييرات ملفاته، ويعرض `/help` الأوامر والمفاتيح.

في السكربتات وCI، يشغّل `codewhale exec` مهمة واحدة دون الواجهة. اقرأ [التشغيل دون واجهة](../INSTALL.md#headless-scripts-ci) قبل أن تمنحه الأدوات عبر `--auto`، لأن `--auto` يستخدم Full Access ما لم تضف بيئة معزولة.

## ما الذي يفعله

أعطِ Codewhale مهمة، فيقرأ المستودع ويعدّل الملفات وينفّذ الأوامر حتى تكتمل المهمة أو يحتاج إلى ردّك. تسرد [ملاحظات أحدث إصدار](https://github.com/codewhale-hq/Codewhale/releases/latest) أحدث التغييرات، ويضم [سجل التغييرات](../../CHANGELOG.md) كل إصدار.

### أي نموذج

يأتي مدمجًا مع أكثر من 40 مزوّدًا، منها Anthropic وOpenAI وDeepSeek وGoogle وMistral وMoonshot وOpenRouter وxAI. تعمل أي نقطة نهاية متوافقة مع OpenAI، وكذلك النماذج المحلية عبر Ollama أو vLLM أو SGLang. النموذج الافتراضي هو `deepseek-flash` من DeepSeek، ويخدمه الآن DeepSeek V4.1 Flash ([التفاصيل](../PROVIDERS.md#deepseek-v41-flash)).

يبدّل `/model` النموذج بين الأدوار، ويختار `/model auto` نموذجًا لكل دور (ويحدد `/router` طريقة الاختيار). يعرض `/turn inspect` النموذج الذي استخدمه دور ما وكلفته والمدة التي استغرقها. راجع [المزوّدون](../PROVIDERS.md).

### الإيصالات والتراجع

يسرد `/receipts`، أو `codewhale receipts --last` بعد الخروج، الملفات التي غيّرتها الجلسة، والأوامر التي شغّلتها، واستدعاءات الويب وMCP، والوكلاء الذين بدأتهم، وكل موافقة ومن أعطاها، وما فشل. يتراجع `/undo` عن طلبك الأخير والملفات التي غيّرها في خطوة واحدة. يعيد `/restore` مساحة العمل كلها إلى لقطة سابقة، ولأن ذلك يكتب فوق التعديلات اللاحقة فهو يحتاج إلى `/trust on` أو Full Access. راجع [الإيصالات](../RECEIPTS.md).

### عدة وكلاء على مهمة واحدة

يحدد `/goal` هدفًا يستمر عبر الأدوار. يستطيع Codewhale توزيع العمل على [وكلاء فرعيين](../SUBAGENTS.md)، أو تشغيل [Fleet](../FLEET.md) من الوكلاء لكل منهم دوره ونموذجه، أو اتباع [سير عمل](../WORKFLOW_AUTHORING.md) محفوظ في المستودع.

### الإضافات

صِل [خوادم MCP](../MCP.md)، وأضف [المهارات](../SKILLS.md) و[الإضافات البرمجية](../PLUGINS.md)، وشغّل [الخطافات](../HOOKS.md) عند أحداث الجلسة والأدوات. يتيح وضع الشيفرة (تجريبي، ومفعّل افتراضيًا) للنموذج كتابة برنامج `execute_tools` واحد يستدعي عدة أدوات MCP وأدوات إضافات؛ ويظل كل استدعاء خاضعًا لقواعد الموافقة لديك ويحتفظ بإيصاله. إضافة Computer Use المضمّنة في v0.10.2 (تجريبية، على macOS فقط) تستطيع مراقبة التطبيقات الأخرى وتشغيلها، وتبقى معطّلة حتى تراجع صلاحياتها وتفعّلها ([الدليل](https://github.com/codewhale-hq/Codewhale/blob/v0.10.2/crates/tui/plugins/computer-use/README.md)). قادم في 0.10.3: تصدر الإضافة [بشكل منفصل](https://github.com/codewhale-hq/codewhale-cu-plugin).

### طرق التشغيل

| الأمر | ما يفعله |
| --- | --- |
| `codewhale` | تطبيق الطرفية التفاعلي |
| `codewhale exec "…"` | مهمة واحدة دون الواجهة، للسكربتات وCI ([التشغيل دون واجهة](../INSTALL.md#headless-scripts-ci)) |
| `codewhale web` | عميل ويب محلي على `127.0.0.1` ([الدليل](../WEB.md)) |
| `codewhale review --pr N` | مراجعة استشارية لطلب سحب على GitHub؛ يشغّلها [إعداد بوت GitHub](../GITHUB_ACTION.md#set-up-your-own-bot) بمفتاح النموذج الخاص بك |
| `codewhale app-server --http` | [Runtime API](../RUNTIME_API.md)، واجهة HTTP محلية للخيوط والأحداث والموافقات وتغييرات الملفات في كل دور |
| `codewhale serve --acp` | بروتوكول Agent Client Protocol عبر stdio، لمحررات مثل Zed |

يتشارك تطبيق الطرفية و`exec` و`web` وRuntime API محركًا واحدًا ([البنية](../ARCHITECTURE.md)). تتصل به [إضافة VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode) التي يصونها المجتمع من الشريط الجانبي ([المصدر](https://github.com/HengQuWorld/CodeWhale-VSCode)).

## كيف يبقى آمنًا

مفتاحان يحددان ما يستطيع Codewhale فعله من تلقاء نفسه. عندما يكون حقل الإدخال فارغًا، يغيّر `Tab` الوضع: **Plan** يقرأ ويبحث ولا يغيّر شيئًا، و **Work** يعدّل الملفات وينفّذ الأوامر، و **Operate** له صلاحيات Work نفسها ويواصل العمل حتى يتحقق من الهدف أو يحتاج إليك. ويغيّر `Shift+Tab` الصلاحية. **Ask**، وهو الافتراضي، يطبّق تعديلات الملفات في مستودعك دون أن يسأل، ويسأل قبل أوامر shell التي لا تقتصر على القراءة. **Auto-Review** يترك للقواعد ولنموذج مراجِع الموافقة على الاستدعاءات الروتينية ويرفض العالية الخطورة؛ انتقل إلى Ask لتوافق عليها بنفسك. **Full Access** لا يسأل أبدًا ويعطّل sandbox الخاص بـ Codewhale، مع بقاء قواعد المستودع والسياسة المُدارة سارية. التفاصيل في [دليل v0.10.2](https://github.com/codewhale-hq/Codewhale/blob/v0.10.2/docs/MODES.md).

قادم في 0.10.3: يصبح `Shift+Tab` قرصًا واحدًا يمر بـ Ask و Accept Edits و Plan و Auto-Review و Full Access، ولن يغيّر `Tab` الوضع بعد ذلك، ويُلغى وضع Operate. سيسأل Ask قبل كل تعديل على الملفات أيضًا، وسيطبّق الافتراضي الجديد Accept Edits التعديلات داخل مستودع git ويواصل السؤال قبل الأوامر. يشرح [الدليل الحالي](../MODES.md) ذلك.

في Ask وAuto-Review تعمل الأوامر داخل بيئة معزولة من نظام التشغيل إن وُجدت: Seatbelt على macOS، وbubblewrap على Linux إذا كان مثبتًا. لا يملك Windows بيئة معزولة من النظام بعد، ويوقف Full Access البيئة المعزولة. داخل البيئة المعزولة لا تستطيع أوامر الصدفة وجلسات الطرفية قراءة مخازن بيانات الاعتماد مثل `~/.ssh` و`~/.aws` وملفات `.env`. يعرض `/preview-request` معاينة محجوبة البيانات الحساسة للطلب التالي دون إرساله.

لا تحتاج إلى حساب، وتبقى المفاتيح التي تضيفها عبر `/provider` على جهازك. حين تستخدم مفتاح API الخاص بك تدفع للمزوّد مباشرة، ولا تحتاج النماذج المحلية إلى مفتاح. يعرض التذييل و`/cost` كلفة الجلسة حين يكون السعر معروفًا، ويظهر السعر غير المعروف على أنه غير معروف، لا على أنه مجاني أبدًا.

القياس عن بُعد مفعّل افتراضيًا. يرسل أعدادًا مجمّعة (الإصدار والمنصة، ومدة الجلسة ونتيجتها، والمزوّدين المدمجين الذين استخدمتهم، وأعداد الميزات والأخطاء) مع معرّف تثبيت عشوائي يتغير كل 90 يومًا، ولا يرسل أبدًا المحادثات أو الشيفرة أو الموجّهات أو أسماء النماذج أو أسماء الملفات أو المستودعات أو بيانات الاعتماد. يوقفه هذا الأمر:

```bash
codewhale config set telemetry false
```

عند البدء يتحقق Codewhale أيضًا من GitHub بحثًا عن إصدار جديد؛ اضبط `CODEWHALE_NO_UPDATE_CHECK=1` لتخطي ذلك. المزيد في [ترتيب التفويض](../AUTHORIZATION_ORDER.md) و[البيئة المعزولة](../SANDBOX.md) و[القياس عن بُعد](../TELEMETRY.md).

## إلى أين يتجه

يتحول Codewhale إلى بيئة واحدة تمتد عبر طرفيتك وتطبيق سطح المكتب والمتصفح وحواسيب السحابة، يديرها كلها محرك واحد. يبيّن كل جزء أدناه أين يقف اليوم.

| الجزء | الحالة | اليوم |
| --- | --- | --- |
| تطبيق الطرفية | منشور | ثبّته على macOS أو Linux أو Windows وشغّل `codewhale` في أي مستودع. |
| عميل المتصفح المحلي | منشور | شغّل `codewhale web` لقراءة المحادثة وإرسال مهمة والرد على طلبات الموافقة في متصفحك؛ يأتي مع الطرفية ولا يحتاج إلى حساب. |
| عدة وكلاء على مهمة واحدة | منشور | الوكلاء الفرعيون وFleet وسير العمل تتيح لعدة وكلاء تقاسم مهمة واحدة، لكل منهم دوره. |
| تطبيق سطح المكتب | قيد التطوير، دون تنزيل | نبني تطبيقًا لسطح المكتب على المحرك نفسه، وليس له تنزيل عام بعد. |
| حواسيب السحابة | بالدعوة فقط | حواسيب السحابة بالدعوة فقط، ونحن نبنيها لتتمكن الجلسة من مواصلة العمل على جهاز في السحابة. |
| جلسة واحدة عبرها كلها | قيد البناء | نبني جلسة واحدة ترافقك من الطرفية إلى سطح المكتب والمتصفح والسحابة، وتبقى ملكًا لك طوال الطريق. |

توضح [خريطة الطريق](../ROADMAP.md) ما يشمله كل إصدار وأين يمكنك المساعدة.

## إن كنت قادمًا من وكيل آخر

يقرأ Codewhale ملف `AGENTS.md` من تلقاء نفسه. أما ملفات التعليمات المكتوبة لأدوات أخرى، مثل `CLAUDE.md` و`.cursorrules` و`.github/copilot-instructions.md`، فلا تُحمَّل إلا إذا فعّلتها بنفسك عبر `project_instruction_imports = ["claude"]` (أو `"cursor"` و`"cline"` و`"windsurf"` و`"copilot"` و`"all"`) في إعداداتك؛ راجع [config.example.toml](../../config.example.toml). ينبّهك Codewhale حين يجد ملفًا من هذا النوع لم يحمّله.

يجهّز `/import-claude` استيرادًا لإعدادات Claude Code وخوادم MCP لديك لتراجعه. يقبل `/plugin install` إضافات Claude Code التي تحتوي على مهارات وأوامر وملفات تعريف وكلاء وخوادم MCP، ويرفض الإضافات التي تحتاج إلى خطافات أو خوادم LSP ([التفاصيل](../CLAUDE_PLUGIN_COMPAT.md)).

## التوثيق

تتبع هذه الأدلة الفرع الرئيسي، لذا قد تصف تغييرات لم تصل إلى أحدث إصدار بعد.

| ابدأ من هنا | المرجع |
| --- | --- |
| [دليل المستخدم](../GUIDE.md) | [الإعدادات](../CONFIGURATION.md) |
| [التثبيت](../INSTALL.md) | [البنية](../ARCHITECTURE.md) |
| [المزوّدون والنماذج المحلية](../PROVIDERS.md) | [Runtime API](../RUNTIME_API.md) |
| [الأوضاع والأذونات](../MODES.md) | [كتابة الإضافات](../PLUGIN_AUTHORING.md) |
| [اختصارات المفاتيح](../KEYBINDINGS.md) | [الإيصالات](../RECEIPTS.md) |
| [مراجعة طلبات السحب على GitHub](../GITHUB_ACTION.md) | [كل التوثيق](../README.md) |

## المساهمة

يبني Codewhale في العلن الأشخاص الذين يستخدمونه، بلغات كثيرة.

<a href="../CONTRIBUTORS.md#everyone"><img src="../assets/contributors.png" width="760" alt="جدار المساهمين في Codewhale: صورة رمزية لكل شخص له في Codewhale شيفرة أو commit مشترك في التأليف أو طلب سحب جرى استكماله أو بلاغ نُسب إليه، والعدد الإجمالي في الأعلى."></a>

يعدّ [جدار المساهمين](../CONTRIBUTORS.md) كل من له في Codewhale شيفرة أو commit مشترك في التأليف أو طلب سحب جرى استكماله أو بلاغ نُسب إليه، بمن فيهم من تُغفلهم قائمة المساهمين في GitHub نفسها. يعيد `scripts/contributor-wall.py` إنشاء الصورة والقائمة من سجل git ومن GitHub.

كل ملاحظة مرحّب بها: خطأ، أو فكرة، أو شيء بدا أو بدا استخدامه غير صحيح. [افتح issue](https://github.com/codewhale-hq/Codewhale/issues/new/choose) لأي من ذلك؛ والـ issue أيضًا أفضل مكان لبدء ميزة جديدة. طلبات السحب مرحّب بها للإصلاحات والترجمات والمزوّدين والعمل المخطط له. الأسئلة مكانها [Discussions](https://github.com/codewhale-hq/Codewhale/discussions). يوضح [دليل المساهمة](../../CONTRIBUTING.md) أين يذهب كل نوع من المساعدة، وكيف يُدمج عملك باسمك، و[أين يوجد كل شيء](../../CONTRIBUTING.md#project-structure).

تحدّث على [Discord](https://discord.gg/37gfS3ksug)، أو أضف المشرف Hunter على WeChat (`hunterbown`) واطلب الانضمام إلى مجموعة Whale Brothers. الجميع مرحّب بهم.

## التاريخ والترخيص

بدأ Codewhale باسم `deepseek-tui` وما زال يقرأ إعدادات ذلك المشروع وجلساته. يُصان على نحو مستقل، ولا ينتمي إلى DeepSeek أو إلى أي مزوّد نماذج آخر.

[MIT](../../LICENSE). الشيفرة المقتبسة من مشاريع أخرى مفتوحة المصدر مدرجة في إشعارات الأطراف الثالثة الخاصة بـ[المصادر المضمّنة](../../THIRD_PARTY_NOTICES.md) و[التصاميم المقتبسة](../THIRD_PARTY_NOTICES.md).
