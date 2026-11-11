import type { ChromeDict } from "../types";

/**
 * Hindi chrome dictionary.
 *
 * मानक हिन्दी, तुरंत-बरती तकनीकी शब्दावली के साथ — TUI पैक
 * (crates/tui/locales/hi.json) के रजिस्टर से मेल खाता हुआ: «समीक्षा करें»,
 * «चलाएँ»। अंग्रेज़ी की वर्तमान दिशा दर्शाती मूल पुनर्लेखन — कोई भी मॉडल,
 * आपकी मशीन पर।
 *
 * मोड और अनुमति-मुद्रा (posture) literal रहते हैं (Plan / Work / Operate,
 * Ask / Auto-Review / Full Access); `Runtime`, `fleet`, `TUI` उत्पाद-नाम
 * हैं और ऐसे ही रहते हैं।
 *
 * सेकेंडरी नेविगेशन लेबल हिन्दी मुख्य लेबल के साथ छोटा अंग्रेज़ी साथी
 * रखते हैं — हन जोड़ी अंग्रेज़ी संस्करण का अपना संपादकीय उपकरण है।
 */
export const chrome: ChromeDict = {
  navDocs: "दस्तावेज़ीकरण",
  navStart: "शुरुआत",
  navInstall: "इंस्टॉल",
  navFaq: "सामान्य प्रश्न",
  navCommunity: "समुदाय",
  navContribute: "योगदान",

  navFeatures: "क्षमताएँ",
  navProduct: "उत्पाद",
  navModels: "मॉडल",
  navPlugins: "प्लगइन",

  skipToContent: "मुख्य सामग्री पर जाएँ",

  navPrimaryAria: "मुख्य नेविगेशन",
  navHomeAria: "Codewhale होम",

  installCta: "इंस्टॉल करें →",

  dateLocale: "hi-IN",

  menuOpen: "मेनू खोलें",
  menuClose: "मेनू बंद करें",

  themeAuto: "ऑटो",
  themeLight: "लाइट",
  themeDark: "डार्क",
  themeAria: "थीम: {mode} (बदलने के लिए क्लिक करें)",
  themeTitle: "थीम · ऑटो / लाइट / डार्क",

  footerTagline: "किसी भी मॉडल का उपयोग ऐसे एजेंट से करें जिसे आप पढ़ सकते हैं.",
  footerProduct: "उत्पाद",
  footerProject: "प्रोजेक्ट",
  footerDocs: "दस्तावेज़ीकरण",
  footerGuide: "शुरुआत कैसे करें",
  footerInstall: "इंस्टॉल",
  footerApps: "डेस्कटॉप और वेब",
  footerModels: "मॉडल",
  footerRuntime: "Runtime",
  footerFaq: "सामान्य प्रश्न",
  footerIssues: "Issues",
  footerContribute: "योगदान दें",
  footerRoadmap: "रोडमैप",
  footerLicense: "MIT लाइसेंस",
  footerTerms: "सेवा की शर्तें",
  footerPrivacy: "गोपनीयता",
  footerChangelog: "परिवर्तन लॉग",
  footerCanonicalSource: "कैननिकल सोर्स: ",
  footerReleases: " · रिलीज़: ",
  footerReleasesLink: "GitHub रिलीज़",
  footerSecurity: "सुरक्षा",
  footerLegal: "कानूनी",

  switcherLabel: "भाषा",
  switcherSwitchTo: "{label} पर स्विच करें",
  partialBadge: "(आंशिक)",

  stateReleased: "जारी किया गया",
  stateAvailable: "उपलब्ध",
  stateBeta: "बीटा",
  stateDev: "डेवलपमेंट बिल्ड",
  stateOpen: "खुला",
  stateInviteOnly: "केवल आमंत्रण से",
  stateNotOpen: "अभी खुला नहीं है",
  stateNotPublic: "सार्वजनिक नहीं",
  stateInSource: "स्रोत में है, अभी जारी नहीं हुआ",
  numbersAsOf: "{date} तक",
  downloadMacos: "macOS के लिए डाउनलोड करें",
  downloadVersion: "संस्करण {version}",
  downloadSize: "आकार {size}",
  downloadChecksum: "SHA-256 चेकसम",
  downloadSigned: "हस्ताक्षरित और नोटराइज़्ड",
};
