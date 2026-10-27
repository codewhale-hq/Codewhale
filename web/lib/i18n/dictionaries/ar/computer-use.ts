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
  lead: "دع Codewhale يعمل في تطبيقاتك بينما تواصل عملك. امنحه الإذن مرة واحدة على هذا الـ Mac وأوقف استخدام الكمبيوتر متى شئت.",
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
    { title: "فعّل Computer Use", body: "من إعدادات Connectors في Codewhale، ثبّت Computer Use أو حدّثه، ثم راجعه وفعّله. اطلب منه العمل في أحد التطبيقات. عند أول استخدام، اختر Trust until revoked في نافذة المساعد الأصلية؛ وتظل هذه الثقة سارية عبر التطبيقات والنماذج والمهام." },
  ],
  controlsTitle: "التحكم بيدك",
  controlsBody: "يظهر عنصر تحكم صغير باسم Codewhale بجانب التطبيق المستخدم. يعلّق Pause استخدام الكمبيوتر، وينهي Stop computer use جميع جلسات الكمبيوتر الحالية. اختر Turn off في المساعد لسحب وصول Codewhale. اطلب معاينة مباشرة عندما تريد متابعة ما يجري.",
  updateTitle: "تحديثات في الوقت الذي تختاره",
  updateBody: "اختر Check for updates (التحقق من التحديثات) من التطبيق. قبل تثبيت أي تحديث، يتحقق من ملف التنزيل وتوقيع Codewhale وتوثيق Apple، ويحتفظ بالنسخة السابقة من التطبيق للاسترداد.",
  help: "الإعداد واستكشاف الأخطاء وإصلاحها",
  notes: "ملاحظات الإصدار",
  demo: "شاهد فحص الخلفية",
  source: "المصدر والمنصات الأخرى",
  platforms: "هذا التنزيل مخصص لنظام Mac. يعتمد Windows وLinux حاليًا على إضافة المصدر والإعداد من جهة المضيف.",
};
