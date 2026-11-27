<!-- source: README.md sha256:308ec0b2a2e8 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/wordmark-inverted.svg">
  <img src="../../brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**وكيل برمجة مفتوح المصدر بلغة Rust لطرفيتك. أي نموذج، أساطيل متعددة الوكلاء، عزل على مستوى نظام التشغيل، وإيصالات قابلة للتحقق.**

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

[![Latest release](https://img.shields.io/github/v/release/codewhale-hq/Codewhale?label=release)](https://github.com/codewhale-hq/Codewhale/releases/latest)
[![CI](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[الموقع](https://codewhale.net) · [التوثيق](../README.md) · [سجل التغييرات](../../CHANGELOG.md) · [المساهمة](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [Català](README.ca.md)

<img src="../../web/public/codewhale-tui-latest.png" width="760" alt="Codewhale v0.10.4 في الطرفية بعد مهمة حقيقية على Ollama المحلي (qwen3:4b): عدّل ملف ls.py، وانتظر الموافقة قبل تشغيل python3 -m unittest -v test_ls.py، وشغّل اختبارين ناجحين ثم عرض النتيجة.">

<sub>جلسة حقيقية على الإصدار المنشور Codewhale v0.10.4 (48669ad108fa) باستخدام Ollama المحلي (qwen3:4b) في وضع Accept Edits.</sub>

</div>

## التثبيت

يضع سكربت التثبيت أعلاه الملف التنفيذي `codewhale` المتحقق من بصمته والاختصار `codew` في `~/.local/bin` على macOS وLinux. إذا أبلغت الصدفة عن `command not found`، فأضف `~/.local/bin` إلى `PATH` ([التفاصيل](../INSTALL.md#put-it-on-your-path)). وللترقية في المكان نفسه شغّل `codewhale update`.

```bash
winget install --id HunterBown.CodeWhale --exact --source winget  # Windows x64
npm install -g codewhale                                          # wraps the same release binaries
cargo install codewhale-cli --locked                              # builds from source; can trail GitHub Releases
```

تتوفر ملفات تنفيذية جاهزة لأنظمة Linux وmacOS وWindows (معماريتي x64 وarm64). تجد طرق Scoop وNix وHomebrew على Linux وAndroid/Termux (معاينة) والتنزيل اليدوي في [docs/INSTALL.md](../INSTALL.md)، وصور الحاويات في [docs/DOCKER.md](../DOCKER.md). استخدم طريقة تثبيت واحدة لكل جهاز حتى لا يحجب ملفان تنفيذيان بعضهما في `PATH`.

## البدء السريع

1. احفظ عملك عبر commit أو stash، ثم شغّل `codewhale` من جذر مستودع git.
2. شغّل `/provider` (أو اضغط `F3`) لاختيار مزوّد، ولصق مفتاح API، واختيار نموذج. DeepSeek هو الافتراضي؛ وإذا لم يكن هناك مفتاح مُعَدّ وكان خادم Ollama محلي يستجيب، فسيعرض Codewhale نماذجه عند أول رسالة لك.
3. أعطه مهمة:

```text
Fix the failing tests and explain what changed.
```

تعمل أوامر القراءة فقط فورًا، إلا في وضع Ask الذي يسأل قبل كل أمر. أما أوامر الصدفة التي تُحدث تغييرات فتطلب الموافقة: `y` للتشغيل مرة واحدة، و`a` للسماح بالأوامر المطابقة لبقية الجلسة، و`n` للرفض. شغّل `/undo` للتراجع عن آخر دورة وتعديلات ملفاتها، أو `/help` لعرض الأوامر واختصارات المفاتيح.

## القدرات

- **أكثر من 50 مزوّدًا مدمجًا والنماذج المحلية:** Anthropic وOpenAI وDeepSeek (`deepseek-flash` / V4.1 Flash افتراضيًا؛ [التفاصيل](../PROVIDERS.md#deepseek-v41-flash)) وGoogle وMistral وMoonshot وOpenRouter وxAI، وأي نقطة نهاية متوافقة مع OpenAI، وبيئات التشغيل المحلية عبر Ollama أو vLLM أو SGLang. بدّل النماذج أثناء الجلسة عبر `/model`، ووجّه كل دورة عبر `/model auto` (`/router`)، وافحص زمن الاستجابة والتكلفة عبر `/turn inspect`. راجع [المزوّدون](../PROVIDERS.md).
- **الإيصالات والتراجع:** يسجّل `/receipts` (أو `codewhale receipts --last` بعد الخروج) الملفات المعدّلة والأوامر واستدعاءات الويب/MCP والوكلاء الفرعيين والموافقات والإخفاقات. يتراجع `/undo` عن آخر دورة في خطوة واحدة، بينما يعيد `/restore` مساحة العمل إلى لقطة سابقة (يتطلب `/trust on` أو Full Access). راجع [الإيصالات](../RECEIPTS.md).
- **الوكلاء الفرعيون والأساطيل وسير العمل:** نفّذ أهدافًا متعددة الدورات عبر `/goal`، وفوّض المهام إلى [وكلاء فرعيين](../SUBAGENTS.md)، ونسّق [أساطيل (Fleets)](../FLEET.md) بأدوار ونماذج محددة، أو شغّل [مسارات عمل](../WORKFLOW_AUTHORING.md) محفوظة داخل المستودع.
- **MCP والمهارات والخطافات والإضافات:** اربط [خوادم MCP](../MCP.md) و[المهارات](../SKILLS.md) و[الإضافات](../PLUGINS.md) و[خطافات](../HOOKS.md) دورة الحياة. يجمع وضع الكود التجريبي (`execute_tools`) عدة استدعاءات MCP/إضافات ضمن قواعد الموافقة المعتادة. ويبقى [Computer Use](https://github.com/codewhale-hq/codewhale-cu-plugin) الاختياري لنظام macOS معطّلًا حتى تراجعه وتفعّله صراحةً.
- **توافق مباشر:** يقرأ `AGENTS.md` تلقائيًا. ويمكنك تفعيل `CLAUDE.md` أو `.cursorrules` أو `.github/copilot-instructions.md` عبر `project_instruction_imports` في [config.example.toml](../../config.example.toml). استورد إعدادات Claude Code وخوادم MCP عبر `/import-claude`، أو ثبّت إضافات Claude Code (المهارات والأوامر وملفات الوكلاء وMCP) وإضافات DeepSeek Harness (DSH) عبر `/plugin install` ([التفاصيل](../CLAUDE_PLUGIN_COMPAT.md)).

## نقاط الدخول

| الأمر | الغرض |
| --- | --- |
| `codewhale` | تطبيق الطرفية التفاعلي |
| `codewhale exec "…"` | مشغّل مهمة واحدة دون واجهة للسكربتات وCI ([الدليل](../INSTALL.md#headless-scripts-ci)؛ يستخدم `--auto` وضع Full Access ما لم يكن معزولًا) |
| `codewhale web` | عميل ويب محلي على `127.0.0.1` ([الدليل](../WEB.md)) |
| `codewhale review --pr N` | مراجعة استشارية لطلبات السحب على GitHub؛ [إعداد بوت GitHub](../GITHUB_ACTION.md#set-up-your-own-bot) يعمل بمفتاحك الخاص |
| `codewhale app-server --http` | واجهة HTTP محلية لـ [Runtime API](../RUNTIME_API.md) للمحادثات والأحداث والموافقات وفروقات كل دورة |
| `codewhale serve --acp` | بروتوكول Agent Client Protocol عبر stdio لمحررات مثل Zed |

تشترك جميع نقاط الدخول في محرك Rust واحد ([البنية المعمارية](../ARCHITECTURE.md)). وتتصل [إضافة VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode) التي يرعاها المجتمع ([المصدر](https://github.com/HengQuWorld/CodeWhale-VSCode)) بالمحرك نفسه. راجع [ملاحظات أحدث إصدار](https://github.com/codewhale-hq/Codewhale/releases/latest) و[خارطة الطريق](../ROADMAP.md) لحالة الإصدار وسطح المكتب والسحابة.

## الأذونات والعزل والخصوصية

يدوّر `Shift+Tab` بين أوضاع الأذونات: **Ask → Accept Edits → Plan → Auto-Review → Full Access** (`Tab` للإكمال التلقائي فقط). يطبّق **Accept Edits** (الافتراضي) التعديلات داخل مستودع git ويسأل قبل تنفيذ الأوامر. يسأل **Ask** عند كل تعديل وأمر. ويعمل **Plan** للقراءة فقط. يستخدم **Auto-Review** قواعد ونموذج مراجعة للموافقة على الاستدعاءات الروتينية ومنع عالية الخطورة. ويتجاوز **Full Access** طلبات الموافقة ويعطّل عزل نظام التشغيل (مع بقاء قواعد المستودع والسياسات المُدارة سارية). راجع [الأوضاع](../MODES.md).

في أوضاع Ask وAccept Edits وAuto-Review، تعمل الأوامر داخل بيئة عزل لنظام التشغيل حيثما توفرت: Seatbelt على macOS وbubblewrap على Linux (لا يتوفر عزل نظام تشغيل على Windows بعد). لا تستطيع الأوامر المعزولة وجلسات الطرفية قراءة مسارات بيانات الاعتماد مثل `~/.ssh` أو `~/.aws` أو ملفات `.env`. شغّل `/preview-request` لفحص الطلب الصادر بعد تنقيحه قبل إرساله.

تبقى مفاتيح API المضافة عبر `/provider` على جهازك، ولا تحتاج النماذج المحلية إلى مفتاح. ويتتبع `/cost` والشريط السفلي إنفاق الجلسة عندما تكون الأسعار معروفة (ويُعرض السعر غير المعروف كغير معروف، لا كمجاني أبدًا).

القياس عن بُعد المجمّع مفعّل افتراضيًا (الإصدار، نظام التشغيل، مدة الجلسة ونتيجتها، معرّفات المزودين المدمجين، معرّفات النماذج في الكتالوج العام المدمج (تُسجَّل النماذج المخصّصة أو الخاصة أو المحلية كـ `other`)، إجمالي الرموز، عدّادات استدعاء وأخطاء الأدوات المدمجة، وعدّادات الميزات والأخطاء؛ معرّف تثبيت عشوائي يتغيّر كل 90 يومًا؛ لا يُرسل أبدًا الكود أو المطالبات أو المحادثات أو أسماء النماذج المخصّصة/الخاصة أو أسماء الملفات/المستودعات/الفروع أو بيانات الاعتماد أو عناوين IP). لتعطيله:

```bash
codewhale config set telemetry false
```

عند التشغيل يتحقق Codewhale أيضًا من وجود إصدار جديد على GitHub (`CODEWHALE_NO_UPDATE_CHECK=1` للتخطي) ويحدّث كتالوج النماذج العام `models.dev` كل 24 ساعة (`CODEWHALE_DISABLE_MODELS_DEV_FETCH=1` أو `[catalog] auto_refresh = false` للتخطي؛ ويُعطّل تلقائيًا في بيئات CI وعند تفعيل `DO_NOT_TRACK`). التفاصيل: [ترتيب التفويض](../AUTHORIZATION_ORDER.md)، [العزل](../SANDBOX.md)، و[القياس عن بُعد](../TELEMETRY.md).

## التوثيق

تتبع الأدلة فرع `main` وقد تصف تغييرات لم تصل بعد إلى أحدث إصدار.

| ابدأ هنا | المرجع |
| --- | --- |
| [دليل المستخدم](../GUIDE.md) | [الإعدادات](../CONFIGURATION.md) |
| [التثبيت](../INSTALL.md) | [البنية المعمارية](../ARCHITECTURE.md) |
| [المزوّدون والنماذج المحلية](../PROVIDERS.md) | [Runtime API](../RUNTIME_API.md) |
| [الأوضاع والأذونات](../MODES.md) | [تطوير الإضافات](../PLUGIN_AUTHORING.md) |
| [اختصارات المفاتيح](../KEYBINDINGS.md) | [الإيصالات](../RECEIPTS.md) |
| [مراجعة طلبات السحب على GitHub](../GITHUB_ACTION.md) | [جميع الوثائق](../README.md) |

## المساهمة والترخيص

<a href="../CONTRIBUTORS.md#everyone"><img src="../assets/contributors.png" width="760" alt="جدار المساهمين في Codewhale"></a>

يتابع [جدار المساهمين](../CONTRIBUTORS.md) (`scripts/contributor-wall.py`) الكود والمساهمات المشتركة وطلبات السحب المرحلة والبلاغات المعتمدة. تُرسل بلاغات الأخطاء ومقترحات الميزات إلى [Issues](https://github.com/codewhale-hq/Codewhale/issues/new/choose)، والأسئلة إلى [Discussions](https://github.com/codewhale-hq/Codewhale/discussions)، وتجد إرشادات طلبات السحب وهيكلة المشروع في [CONTRIBUTING.md](../../CONTRIBUTING.md) ([بنية المشروع](../../CONTRIBUTING.md#project-structure)). انضم إلينا على [Discord](https://discord.gg/37gfS3ksug) أو أضف `hunterbown` على WeChat للانضمام إلى مجموعة Whale Brothers.

بدأ Codewhale باسم `deepseek-tui` ويحافظ على التوافق الرجعي مع إعداداته وجلساته. يُصان المشروع بشكل مستقل ولا يرتبط بـ DeepSeek أو أي مزوّد نماذج آخر.

[MIT](../../LICENSE). إشعارات الطرف الثالث: [المصادر المضمّنة](../../THIRD_PARTY_NOTICES.md) و[التصاميم المكيّفة](../THIRD_PARTY_NOTICES.md).
