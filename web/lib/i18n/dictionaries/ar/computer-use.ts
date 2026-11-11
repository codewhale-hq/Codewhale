import type { ComputerUseDict } from "../types";

/**
 * Arabic (Modern Standard) dictionary for `app/[locale]/computer-use/page.tsx`
 * and the Computer Use section of the install page. Product names, the menu
 * items Pause, Stop and Check for updates, and the macOS setting names stay
 * as the app shows them; the macOS names use the Arabic system UI labels.
 */
export const computerUse: ComputerUseDict = {
  metaTitle: "Computer Use · معاينة · Codewhale",
  metaDescription: "نزّل Codewhale Computer Use لنظام Mac وجهّزه. تحكّم في التطبيقات في الخلفية، وإعداد الأذونات، وأزرار Pause وStop بيدك.",
  title: "Computer Use",
  lead: "دع وكيلك يعمل في تطبيقات Mac بينما تواصل استخدام الكمبيوتر.",
  publisher: "من Codewhale",
  download: "نزّل لنظام Mac",
  downloadZip: "أرشيف ZIP (يستخدمه محدّث التطبيق الداخلي)",
  requirements: "macOS 13.5 أو أحدث · Apple silicon وIntel",
  included: "تنزيل تطبيق واحد فقط. لا حاجة إلى تثبيت Node أو مترجم بشكل منفصل.",
  pendingTitle: "تنزيل Mac قيد التحضير",
  pendingBody: "سيظهر المثبّت العام هنا بعد اكتمال توثيق Apple وفحوصات الإصدار.",
  unavailableTitle: "تعذّر التحقق من توفر التنزيل",
  unavailableBody: "حدّث هذه الصفحة للمحاولة مجددًا، أو راجع الإصدارات المنشورة أدناه.",
  releases: "الإصدارات المنشورة",
  receipt: "تفاصيل التحقق من التنزيل",
  setup: "جهّز جهاز Mac",
  steps: [
    { title: "ثبّت التطبيق", body: "افتح صورة القرص واسحب Codewhale Computer Use إلى مجلد «التطبيقات». افتحه من «التطبيقات»، ثم اختر Computer Use من أيقونة الحوت في شريط القوائم." },
    { title: "راجع الأذونات", body: "استخدم أزرار الإعداد لفتح «تسهيلات الاستخدام» و«تسجيل الشاشة» في إعدادات النظام. أنت من يقرر الأذونات التي تمنحها." },
    { title: "اربط وكيلك", body: "اختر إعداد Codewhale أدناه المناسب لإصدار CLI لديك. أما مضيفات MCP الأخرى المتوافقة فتستخدم دليل الإعداد في المستودع." },
  ],
  controlsTitle: "التحكم بيدك",
  controlsBody: "بعد منح أذونات macOS، يعمل الوضع القياسي دون طلب إذن لكل تطبيق. تظل التطبيقات التي تحظرها خارج نطاق الوصول؛ يعلّق Pause استخدام الكمبيوتر، وينهي Stop computer use الجلسات الحالية. وضع الموافقة الصارم اختياري. اطلب معاينة مباشرة عندما تريد متابعة العمل.",
  updateTitle: "تحديثات في الوقت الذي تختاره",
  updateBody: "اختر Check for updates (التحقق من التحديثات) من التطبيق. قبل تثبيت أي تحديث، يتحقق من ملف التنزيل وتوقيع Codewhale وتوثيق Apple، ويحتفظ بالنسخة السابقة من التطبيق للاسترداد.",
  help: "الإعداد واستكشاف الأخطاء وإصلاحها",
  notes: "ملاحظات الإصدار",
  demo: "شاهد فحص الخلفية",
  source: "عرض على GitHub",
  platforms: "مساعد Mac في مرحلة بيتا، وإضافة السوق متاحة لنظام Mac فقط. تتضمن الإصدارات المنشورة أيضًا معاينة تجريبية غير موقّعة لنظام Windows x64؛ ويتطلب Linux إعدادًا تجريبيًا من المصدر أو عبر Docker. يستخدم الإدخال في Windows وLinux سطح المكتب المشترك.",
  connectTitle: "ربط Codewhale",
  connectVersion: "يتضمن Codewhale 0.10.2 إضافة Computer Use قديمة. تثبيت مدخل الكتالوج لا يحدّثها. اتصل بمساعد Mac المثبّت مباشرة عبر MCP.",
  connectLimit: "يضيف اتصال MCP المباشر أدوات دون مهارات الإضافة. لا تستخدمه بالتزامن مع الإضافة المدمجة القديمة. إذا كانت مفعّلة، فنفّذ /plugin disable computer-use أولاً.",
  connectRuntime: "يتضمن التطبيق Node؛ لا حاجة إلى تثبيته بشكل منفصل. إذا كان التطبيق مثبّتاً في مكان آخر، فاضبط APP على المسار الفعلي لحزمة التطبيق.",
  connectReload: "في جلسة طرفية قيد التشغيل، نفّذ /mcp reload ثم اطلب:",
  connectPrompt: "تحقّق من أذونات Computer Use باستخدام request_access. لا ترسل أي إدخال.",
  connectExpected: "تحقّق من via: app وإصدار المساعد والأذونات الناقصة في الرد. إذا كان المساعد غير موجود، فتوقّف واتبع إرشادات الإعداد. يتحقّق هذا من الاتصال والأذونات فقط.",
  connectGuide: "دليل إعداد MCP",
  contributeTitle: "ساهم في تحسين Computer Use",
  contributeBody: "حسّن التوثيق أو اختبر سير عمل في أحد التطبيقات وشارك نتائج قابلة لإعادة الإنتاج.",
  contributeGuide: "دليل المساهمة",
  contributeIssue: "الإبلاغ عن مشكلة",
};
