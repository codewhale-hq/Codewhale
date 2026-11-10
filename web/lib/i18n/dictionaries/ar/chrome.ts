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
  navStart: "البداية",
  navInstall: "التثبيت",
  navFaq: "الأسئلة الشائعة",
  navCommunity: "المجتمع",
  navContribute: "المساهمة",

  navFeatures: "القدرات",
  navProduct: "المنتج",
  navModels: "النماذج",
  navPlugins: "الإضافات",

  skipToContent: "الانتقال إلى المحتوى الرئيسي",

  navPrimaryAria: "التنقل الرئيسي",
  navHomeAria: "الصفحة الرئيسية لـ Codewhale",

  installCta: "ثبّت ←",

  dateLocale: "ar",

  menuOpen: "افتح القائمة",
  menuClose: "أغلق القائمة",

  themeAuto: "تلقائي",
  themeLight: "فاتح",
  themeDark: "داكن",
  themeAria: "السمة: {mode} (انقر للتبديل)",
  themeTitle: "السمة · تلقائي / فاتح / داكن",

  footerTagline:
    "وكيل البرمجة مفتوح المصدر لطرفيتك والنماذج التي تختارها.",
  footerProduct: "المنتج",
  footerProject: "المشروع",
  footerDocs: "التوثيق",
  footerGuide: "البداية",
  footerInstall: "التثبيت",
  footerModels: "النماذج",
  footerRuntime: "Runtime",
  footerFaq: "الأسئلة الشائعة",
  footerIssues: "Issues",
  footerContribute: "المساهمة",
  footerRoadmap: "خارطة الطريق",
  footerLicense: "رخصة MIT",
  footerTerms: "شروط الخدمة",
  footerPrivacy: "الخصوصية",
  footerChangelog: "سجل التغييرات",
  footerCanonicalSource: "المصدر الرسمي: ",
  footerReleases: " · الإصدارات: ",
  footerReleasesLink: "إصدارات GitHub",
  footerSecurity: "الأمن",

  switcherLabel: "اللغة",
  switcherSwitchTo: "التبديل إلى {label}",
  partialBadge: "(جزئي)",
};
