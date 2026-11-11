import type { ChromeDict } from "../types";

/**
 * Arabic chrome dictionary — فصحى معاصرة، بترجمة صيغة RTL.
 *
 * إعادة صياغة أصلية باتجاه الإنجليزية الحالي — «أي نموذج، على جهازك»،
 * لا تموضع «local-first» المتقاعد.
 *
 * أسماء الأوامر والمنتجات تبقى كما هي: Codewhale و GitHub و Issues
 * و `Runtime` و `fleet` و TUI. الأسهم تشير إلى الأمام في سياق RTL (←).
 *
 * التسميات الثانوية للتنقل تقارن التسمية العربية بنظيرة إنجليزية
 * قصيرة — ثنائية الهان هي أداة تحريرية خاصة بالنسخة الإنجليزية.
 */
export const chrome: ChromeDict = {
  navDocs: "التوثيق",
  navContribute: "المساهمة",
  navSignIn: "تسجيل الدخول",
  navDesktop: "سطح المكتب",

  navFeatures: "القدرات",
  navModels: "النماذج",
  navPlugins: "الإضافات",

  skipToContent: "الانتقال إلى المحتوى الرئيسي",

  navPrimaryAria: "التنقل الرئيسي",
  navHomeAria: "الصفحة الرئيسية لـ Codewhale",

  installCta: "ثبّت ←",

  dateLocale: "ar",

  menuOpen: "افتح القائمة",
  menuClose: "أغلق القائمة",
  translationMachineDraft: "هذه الترجمة مسودة آلية ولم يراجعها بعد متحدث أصلي.",

  themeAuto: "تلقائي",
  themeLight: "فاتح",
  themeDark: "داكن",
  themeAria: "السمة: {mode} (انقر للتبديل)",
  themeTitle: "السمة · تلقائي / فاتح / داكن",

  footerProduct: "المنتج",
  footerProject: "المشروع",
  footerDocs: "التوثيق",
  footerGuide: "البداية",
  footerInstall: "التثبيت",
  footerModels: "النماذج",
  footerFaq: "الأسئلة الشائعة",
  footerContribute: "المساهمة",
  footerRoadmap: "خارطة الطريق",
  footerLicense: "رخصة MIT",
  footerMerch: "منتجات الدعم",
  footerTerms: "شروط الخدمة",
  footerPrivacy: "الخصوصية",
  footerChangelog: "سجل التغييرات",
  footerCanonicalSource: "المصدر الرسمي: ",
  footerReleases: " · الإصدارات: ",
  footerReleasesLink: "إصدارات GitHub",
  footerSecurity: "الأمن",
  footerLegal: "قانوني",

  switcherLabel: "اللغة",
  switcherSwitchTo: "التبديل إلى {label}",
  partialBadge: "(جزئي)",

  stateReleased: "تم الإصدار",
  stateAvailable: "متاح",
  stateBeta: "تجريبي",
  stateDev: "نسخة تطوير",
  stateOpen: "مفتوح",
  stateInviteOnly: "بدعوة فقط",
  stateNotOpen: "لم يُفتح بعد",
  stateNotPublic: "غير متاح للعامة",
  stateInSource: "في الشيفرة المصدرية، ولم يصدر بعد",
  numbersAsOf: "اعتبارًا من {date}",
  downloadMacos: "تنزيل لنظام macOS",
  downloadVersion: "الإصدار {version}",
  downloadSize: "الحجم {size}",
  downloadChecksum: "المجموع الاختباري SHA-256",
  downloadSigned: "موقّع وموثّق",
};
